use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use regex::Regex;

use crate::core::export::{ExportFormat, ExportMessage};
use crate::core::grepdir::{GrepHit, GrepMessage, GrepOptions};
use crate::core::indexer::{FileSet, IndexError, LogFileIndex};
use crate::core::prefs::Prefs;
use crate::core::recents::Recents;
use crate::core::search::{
    CancelToken, SearchError, SearchHit, SearchMessage, SearchMode, SearchOptions,
};
use crate::ui::{log_view, results_view, sidebar, status_bar, theme, toolbar};

/// 每帧最多处理 1000 条后台消息，防止消息洪水饿死渲染（spec §8.4，对应 D6）。
const MAX_MSG_PER_FRAME: usize = 1000;

/// 检索命中上限（G6）。
const MAX_HITS: usize = 2_000_000;

/// 单个面板最多显示多少个（2×2 网格上限，与 VSCode 编辑器组的实用密度一致）。
pub const MAX_PANES: usize = 4;

/// 面板标题条高度（px）。比 18px 的图标命中区略高，让图标垂直居中；
/// 固定值也让「先铺背景、再画内容」的绘制顺序（见 `render_one_pane`）易于实现。
const TITLE_BAR_HEIGHT: f32 = 20.0;

/// 检索命中导航方向（VS Code `F3` = 下一处，`⇧F3` = 上一处）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitDir {
    /// 下一个命中（到末尾后回到第一个）。
    Next,
    /// 上一个命中（到开头后回到最后一个）。
    Prev,
}

/// 拆分方向。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SplitDir {
    /// 左右并排。
    #[default]
    Horizontal,
    /// 上下并排。
    Vertical,
}

impl SplitDir {
    /// 垂直方向的反向（2×2 网格的内层用）。
    fn perpendicular(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

/// 面板布局：`count` 个面板按 `dir` 分割；`count == 4` 时渲染为 2×2 网格。
///
/// 分割位置由 [`PaneLayout::ratio`] 决定（第一块占总尺寸的比例，可拖拽分隔条调整），
/// 不再固定对半均分。
#[derive(Clone, Debug)]
pub struct PaneLayout {
    /// 拆分方向（`count == 4` 时用作外层方向）。
    pub dir: SplitDir,
    /// 面板数量，恒在 `1..=MAX_PANES`。
    pub count: usize,
    /// 第一块占总尺寸的比例（`0.0..=1.0`，实际 clamp 到 [`MIN_RATIO`]..=[`MAX_RATIO`]）。
    /// 所有分割点共用同一个比例（2×2 网格内外层一致，作为 MVP 简化）。
    pub ratio: f32,
}

/// 分割比例的下限 / 上限（避免某一面板被拖到不可见）。
const MIN_RATIO: f32 = 0.15;
const MAX_RATIO: f32 = 0.85;

impl Default for PaneLayout {
    fn default() -> Self {
        Self {
            dir: SplitDir::Horizontal,
            count: 1,
            ratio: 0.5,
        }
    }
}

impl PaneLayout {
    /// 向右/向下新增一个面板（已达上限则无操作）。
    pub fn split(&mut self, dir: SplitDir) {
        if self.count >= MAX_PANES {
            return;
        }
        if self.count == 1 {
            self.dir = dir; // 从单面板拆分时决定方向
        }
        self.count += 1;
    }

    /// 关闭一个面板（最少保留一个）；返回被关闭面板的索引（调用方据此修正 `active_pane`）。
    pub fn close(&mut self, index: usize) -> bool {
        if self.count <= 1 || index >= self.count {
            return false;
        }
        self.count -= 1;
        true
    }
}

/// 单个日志面板的状态。
///
/// 从 `AppState` 抽出的「每面板独立持有」部分：拆成多个面板后，选中行、跳转目标、
/// 折行、横向滚动范围、视图模式都应当各自独立，否则两个面板会互相干扰（例如 A 面板
/// 消费掉 `scroll_target` 后 B 面板永远收不到跳转）。
#[derive(Clone, Default)]
pub struct PaneState {
    /// 该面板单独打开的文件集；`None` 表示共享全局 `AppState::fileset`。
    pub fileset_override: Option<FileSet>,
    /// 选中行（全量视图为该文件集的全局行号，命中视图为命中索引）。
    pub selected_row: Option<usize>,
    /// 多选锚点（`selected_row` 为另一端）。
    ///
    /// 选中态用「锚点 + 主选中行」表示一段**闭区间**，而不是保存选中行的集合：
    /// 日志可达上亿行，全选时若存集合会爆内存，而区间只需两个 `usize`（O(1)）。
    /// - `None` → 仅 `selected_row` 单行选中；
    /// - `Some(a)` → 选中 `[min(a, selected_row), max(a, selected_row)]`。
    pub selection_anchor: Option<usize>,
    /// 行号槽拖动多选进行中时，记录「拖动起点行」；松手（pointer release）即清空。
    ///
    /// 与 `selection_anchor` 的区别：后者是选区端点之一（跨帧稳定），本字段仅用于
    /// 跟踪一次拖拽手势的起点，拖拽过程中 `selection_anchor` 被固定为 `drag_anchor`、
    /// `selected_row` 跟随指针悬停行，从而实现「按住行号槽上下拖 = 连续多选」。
    pub drag_anchor: Option<usize>,
    /// 正文拖选进行中时，记录「起点行 + 起点字符索引」；松手即清空。
    ///
    /// egui 不暴露 `Label` 的选区文本（`LabelSelectionState` 只有 `has_selection`，
    /// 拿不到选中的字符串），而 `⌘F` 又需要「把选中文本带入检索框」，故由
    /// `log_view` 自己按指针 x 反查字符索引，拖动过程中把选区文本写进
    /// [`Self::selected_word`]。跨行拖选不支持（只取起点行内的部分）。
    pub drag_text: Option<(usize, usize)>,
    /// 待跳转行号，由该面板自己消费（替代原先的全局 `AppState::scroll_target`）。
    pub scroll_target: Option<usize>,
    /// 折行开关（每面板独立）。
    pub wrap: bool,
    /// 估算的最长行渲染宽度，用于固定横向滚动范围。
    pub max_line_width: f32,
    /// 是否显示检索命中视图（可做到「A 面板全量 + B 面板命中」的对比）。
    pub in_result_mode: bool,
    /// 最近一次**在正文中选中的文本**（双击取词或拖动选择），供 `⌘F` 自动带入检索框。
    /// 空串表示无。
    ///
    /// 两种来源：双击取一个词（见 `log_view::word_at_offset`），或在正文拖动选择一段
    /// 文本（见 `log_view` 的正文拖选分支）。后者覆盖前者，语义即「当前选中的文本」。
    ///
    /// 存在面板上而不是 `AppState`，是因为取词发生在 `log_view::show` 的行渲染闭包里，
    /// 那里只拿得到 `&mut PaneState`（`state` 是不可变借用，见该函数的借用注释）。
    pub selected_word: String,
}

impl PaneState {
    /// 当前选中的**闭区间** `[start, end]`；无选中返回 `None`。
    ///
    /// `selection_anchor` 为 `None` 时是单行选中，区间退化为 `[row, row]`。
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let row = self.selected_row?;
        match self.selection_anchor {
            Some(a) => Some((a.min(row), a.max(row))),
            None => Some((row, row)),
        }
    }

    /// 第 `row` 行是否处于选中区间内。
    pub fn is_row_selected(&self, row: usize) -> bool {
        matches!(self.selected_range(), Some((s, e)) if row >= s && row <= e)
    }

    /// 选中区间内的行数（单行选中为 1，无选中为 0）。用于菜单文案与复制前预估。
    pub fn selected_count(&self) -> usize {
        match self.selected_range() {
            Some((s, e)) => e - s + 1,
            None => 0,
        }
    }

    /// Shift + 点击：把 `row` 设为选中行并按需设立锚点。
    ///
    /// 已有锚点则保留（可继续扩展选区）；否则以原 `selected_row` 为锚点，
    /// 此前无选中时锚点即 `row` 本身（退化为单行）。
    pub fn extend_selection_to(&mut self, row: usize) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.selected_row;
        }
        self.selected_row = Some(row);
    }

    /// 普通点击：只选中 `row`，清除多选锚点。
    pub fn select_single(&mut self, row: usize) {
        self.selected_row = Some(row);
        self.selection_anchor = None;
    }

    /// 全选：锚点置 0、主选中行置 `last_row`。上亿行也只有两个 `usize`，不会爆内存。
    pub fn select_all(&mut self, last_row: usize) {
        self.selection_anchor = Some(0);
        self.selected_row = Some(last_row);
    }

    /// 清空选中态（含锚点）。进行中的行号槽拖拽也一并取消。
    pub fn clear_selection(&mut self) {
        self.selected_row = None;
        self.selection_anchor = None;
        self.drag_anchor = None;
    }
}

/// 应用的全部可变状态。UI 各面板只借用它的引用，不持有状态本身。
#[derive(Default)]
pub struct AppState {
    /// 底部状态栏展示的文本。
    pub status_text: String,
    /// 已加载的日志文件集合（支持多文件全局行寻址）。
    pub fileset: FileSet,
    /// 着色器：级别高亮（检索时复用同一 `Regex` 做命中高亮）。
    pub highlighter: crate::highlight::Highlighter,
    /// 是否启用折行显示（默认关闭，横向滚动；见 spec G7）。
    /// **当前活动面板**的镜像，方便工具栏/状态栏读写；真实来源见 [`panes`]。
    pub wrap: bool,
    /// 当前选中的行（全量视图为全局行号，命中视图为命中索引），供高亮与复制使用。
    /// **当前活动面板**的镜像，方便状态栏/快捷键读写；真实来源见 [`panes`]。
    pub selected_row: Option<usize>,
    /// 估算的最长行渲染宽度（像素），用于固定横向滚动范围，避免滚动条随虚拟滚动抖动。
    /// **当前活动面板**的镜像，真实来源见 [`panes`]。
    pub max_line_width: f32,

    // —— 拆分面板（spec §7.7.7）——
    /// 所有面板的独立状态，下标即面板 id。
    pub panes: Vec<PaneState>,
    /// 面板布局（方向 + 数量）。
    pub pane_layout: PaneLayout,
    /// 当前活动面板下标（点击面板激活；跳转类操作只作用于它）。
    pub active_pane: usize,
    /// toolbar 置位后，在 `ui` 中弹出打开文件对话框。
    pub pending_open: bool,
    /// 最近打开的文件列表（M11 / spec Q3），持久化在平台配置目录。
    pub recents: Recents,
    /// toolbar 置位后，在 `ui` 中打开该最近文件（避免在同一帧内同时借用 UI 与 self）。
    pub pending_open_recent: Option<PathBuf>,
    /// toolbar 置位后，在 `ui` 中弹出「打开目录」对话框（Q「打开目录」）。
    pub pending_open_dir: bool,
    /// 面板标题栏「本面板打开」置位后，在 `ui` 中弹出文件对话框，把该文件单独载入此面板
    /// （写入 `panes[idx].fileset_override`，与全局文件集脱钩，便于对比两份不同日志）。
    pub pending_open_in_pane: Option<usize>,

    // —— 检索相关 ——
    /// 检索关键字。
    pub search_pattern: String,
    /// 检索模式：纯文本 / 正则。
    pub search_mode: SearchMode,
    /// 大小写敏感。
    pub search_case_sensitive: bool,
    /// 是否正在检索（禁用「打开/搜索」，保留「停止」与滚动，见 G1）。
    pub is_searching: bool,
    /// 检索结果坐标（只存 file_idx/line_idx，惰性解析文本）。
    pub search_results: Vec<SearchHit>,
    /// 正则编译错误原文（内联红色提示，不弹模态框，见 §7.5）。
    pub search_error: Option<String>,
    /// 是否因达上限而截断。
    pub search_truncated: bool,
    /// 进度字节数 (done, total)。
    pub search_progress: (u64, u64),
    /// 是否在结果视图（命中行）与全量日志间切换。
    pub in_result_mode: bool,
    /// 检索中用同一 `Regex` 做命中高亮（G5）。
    pub hit_regex: Option<Regex>,
    /// 命中导航游标：当前定位到 `search_results` 的第几个（VS Code `F3`/`⇧F3`）。
    ///
    /// `None` = 尚未开始导航（首次按 F3 落到第一个命中）。新检索开始或结果被清空时重置，
    /// 否则游标会指向另一批结果的无意义下标。
    pub hit_cursor: Option<usize>,

    /// toolbar 置位后，在 `ui` 中启动检索。
    pub pending_search: bool,
    /// toolbar 置位后，在 `ui` 中取消检索。
    pub pending_stop: bool,

    // —— 导出相关（M5 / T17）——
    /// 导出是否带 `<文件名>:<行号>:` 前缀（默认关，仅原始行，见 §7.8 / plan Q5）。
    pub export_with_prefix: bool,
    /// 是否正在导出（禁用「打开/搜索/导出」，保留「取消导出」，见 T17）。
    pub is_exporting: bool,
    /// 导出进度（已写行数, 总行数）。
    pub export_progress: (usize, usize),
    /// 导出失败原因（内联红字，不 panic）。
    pub export_error: Option<String>,
    /// 最近一次成功导出的目标路径（用于状态栏展示）。
    pub export_path: Option<PathBuf>,
    /// toolbar 置位后，在 `ui` 中弹出保存对话框并启动导出。
    pub pending_export: bool,
    /// toolbar 置位后，在 `ui` 中取消导出。
    pub pending_export_cancel: bool,

    // —— 性能 HUD（M9，spec P4/P5/P6 现场观测）——
    /// 是否显示性能 HUD（帧耗时 / FPS / p95），用于 spec P4/P5/P6 观测。
    /// 默认由环境变量 `HYPER_LOG_PERF=1` 开启，也可在工具栏「性能」按钮切换。
    pub show_perf: bool,
    /// 最近若干帧的帧耗时（毫秒），环形缓冲，用于 p95 / 峰值统计。
    frame_ms: Vec<f32>,
    /// 上一帧时间戳（秒，取自 `ctx.input().time`），用于计算帧间隔。
    last_frame_sec: f64,
    /// 性能日志上次打印时刻（ctx.time，秒），用于 `HYPER_LOG_PERF_LOG=1` 节流（每 ~1s 一行）。
    perf_log_last_sec: f64,

    // —— 侧边栏目录树（spec §7.7 侧边栏）——
    /// 是否显示左侧文件目录树。
    pub show_sidebar: bool,
    /// 侧边栏当前高亮（已跳转）的文件索引；点击文件后置位并持久高亮，类似 VSCode 高亮已打开文件。
    pub sidebar_active_file: Option<usize>,

    // —— 用户偏好持久化（spec §1.3 待定 P2，M17）——
    /// 持久化偏好（主题/窗口几何/折行/侧栏/最近检索词/字号）；实时开关 `wrap`/`show_sidebar`
    /// 在保存时同步进本结构后写盘，加载时回填这两个实时字段。
    pub prefs: Prefs,
    /// 日志正文字号（`⌘+`/`⌘-`/`⌘0` 缩放，持久化在 [`Prefs::font_size`]）。
    /// 行高与行号槽宽度都按它联动缩放，故改动后必须清空各面板的 `max_line_width` 重算。
    pub font_size: f32,
    /// 上次写偏好（窗口几何）的时刻（ctx.time，秒），节流到约每 2s 一次避免拖动时频繁写盘。
    pub last_prefs_save: f64,

    // —— 外部文件修改检测（spec Q6）——
    /// 被外部修改/截断/轮转的文件路径列表（状态栏告警 + 「重新加载」依据），由 `logic` 节流检测。
    pub dirty_files: Vec<PathBuf>,
    /// 上次脏检查的时刻（ctx.time，秒），节流到约每秒一次避免频繁 stat。
    pub last_dirty_check: f64,
    /// 状态栏「重新加载」置位后，在 `ui` 中清空并重新打开所有已加载文件（spec Q6）。
    pub pending_reload: bool,

    // —— 行号跳转（spec §7.7 行跳转）——
    /// 行号跳转输入框的缓冲区（1-based 全局行号）。
    pub line_jump: String,
    /// 待跳转行号由**活动面板**自己持有（[`PaneState::scroll_target`]），见 [`AppState::jump_to_row`]。

    // —— 快捷键焦点请求 ——
    /// 请求把焦点移到检索输入框（⌘F），由 `toolbar` 在下一帧落实。
    pub focus_search: bool,
    /// 请求把焦点移到行号跳转输入框（⌘L），由 `toolbar` 在下一帧落实。
    pub focus_line_jump: bool,

    // —— 目录检索（"查找全部"）相关 ——
    /// 是否显示独立结果页（`true` 时中央区渲染结果而非日志正文）。
    pub show_results: bool,
    /// 目录检索命中结果（内联行文本 + 展示路径）。
    pub grep_hits: Vec<GrepHit>,
    /// 结果页当前选中的命中索引。
    pub grep_selected_row: Option<usize>,
    /// 目录检索是否因达上限而截断。
    pub grep_truncated: bool,
    /// 是否正在目录检索。
    pub is_grepping: bool,
    /// 目录检索进度：(已扫描文件数, 总文件数, 已扫描字节)。
    pub grep_progress: (usize, usize, u64),
    /// 目录检索失败原因（内联提示）。
    pub grep_error: Option<String>,
    /// 当前目录检索的根目录（供展示路径裁剪与「打开目录」回显）。
    pub grep_root: Option<PathBuf>,
    /// toolbar 置位后，在 `ui` 中弹出「查找全部」目录选择并启动。
    pub pending_grep: bool,
    /// 结果页「保存结果」置位后，在 `ui` 中弹出保存对话框。
    pub pending_grep_save: bool,
    /// toolbar 置位后，在 `ui` 中取消目录检索。
    pub pending_grep_stop: bool,
    /// 结果页点击某条命中后置位：(绝对路径, 文件内行号 1-based)，在 `ui` 中打开并跳转原文。
    pub pending_grep_jump: Option<(PathBuf, usize)>,
}

impl AppState {
    /// 把当前实时偏好（主题/折行/侧栏/最近检索词/窗口几何）同步进自身 `prefs` 并写盘。
    /// 主题等即时开关在切换处调用；窗口几何由 `logic()` 节流写盘。失败仅记日志，不打断用户。
    pub fn save_prefs(&self) {
        let mut p = self.prefs.clone();
        p.wrap = self.wrap;
        p.sidebar_visible = self.show_sidebar;
        // 仅持久化分割比例（下次拆分时的布局偏好）；拆分数与方向是会话内临时状态，
        // 启动恒为单面板，故不写盘（见 `LogViewerApp::new` 的固定单面板初始化）。
        p.split_ratio = self.pane_layout.ratio;
        p.font_size = self.font_size;
        p.save();
    }

    // —— 拆分面板辅助（spec §7.7.7）——

    /// 确保 `panes` 至少覆盖布局所需的数量（新增的面板取默认状态）。
    pub fn ensure_panes(&mut self) {
        let need = self.pane_layout.count.max(1);
        if self.panes.len() < need {
            self.panes.resize_with(need, PaneState::default);
        }
        if self.active_pane >= self.panes.len() {
            self.active_pane = self.panes.len() - 1;
        }
    }

    /// 活动面板的可变引用（越界时回退到最后一个，`min(len-1)`）。
    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        let i = self.active_pane.min(self.panes.len().saturating_sub(1));
        &mut self.panes[i]
    }

    /// 活动面板的不可变引用。
    pub fn active_pane(&self) -> Option<&PaneState> {
        self.panes.get(self.active_pane)
    }

    /// 请求把**活动面板**跳转到指定行（侧边栏点击、行号跳转、查找结果跳转都走这里）。
    pub fn jump_to_row(&mut self, row: usize) {
        if let Some(p) = self.panes.get_mut(self.active_pane) {
            p.scroll_target = Some(row);
        }
    }

    /// 在检索命中之间前/后导航（VS Code `F3` / `⇧F3`）。
    ///
    /// 活动面板在命中视图时按**命中索引**定位，在全量视图时换算成**全局行号**
    /// （`file_global_start + line_idx`），这样 F3 在全量日志里也能带着上下文跳到那一行。
    /// 两端环绕（最后一个之后回到第一个）。无命中时返回 `false` 且不改状态。
    pub fn goto_hit(&mut self, dir: HitDir) -> bool {
        let total = self.search_results.len();
        if total == 0 {
            return false;
        }
        let idx = match (dir, self.hit_cursor) {
            (HitDir::Next, None) => 0,
            (HitDir::Next, Some(c)) => (c + 1) % total,
            (HitDir::Prev, None) => total - 1,
            // 结果可能比游标短（重搜后命中变少），先夹取再减，避免下溢 panic。
            (HitDir::Prev, Some(c)) if c == 0 || c > total => total - 1,
            (HitDir::Prev, Some(c)) => c - 1,
        };
        self.hit_cursor = Some(idx);

        let hit = self.search_results[idx];
        let global_row = self
            .fileset
            .file_global_start(hit.file_idx as usize)
            .map(|start| start + hit.line_idx as usize);
        let pane = self.active_pane_mut();
        // 命中视图下 `selected_row` 的语义是命中索引，全量视图下才是全局行号。
        let target = if pane.in_result_mode {
            idx
        } else {
            global_row.unwrap_or(idx)
        };
        pane.select_single(target);
        pane.scroll_target = Some(target);
        self.selected_row = Some(target); // 顶层镜像（状态栏/快捷键读它）
        true
    }

    /// 按增量缩放正文字号（`⌘+` / `⌘-`），夹取到合法范围。
    pub fn zoom_font(&mut self, delta: f32) {
        self.set_font_size(self.font_size + delta);
    }

    /// 设置正文字号并落盘。
    ///
    /// 行高与行号槽宽度都由字号派生，故必须清空各面板的 `max_line_width` 让它惰性重算，
    /// 否则横向滚动范围还停留在旧字号下的估算值（表现为右侧滚不到底或滚动条过长）。
    pub fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(
            crate::core::prefs::MIN_FONT_SIZE,
            crate::core::prefs::MAX_FONT_SIZE,
        );
        if (size - self.font_size).abs() < f32::EPSILON {
            return;
        }
        self.font_size = size;
        self.prefs.font_size = size;
        for p in &mut self.panes {
            p.max_line_width = 0.0;
        }
        self.max_line_width = 0.0;
        self.save_prefs();
    }

    /// 恢复默认字号（`⌘0`）。
    pub fn reset_font_size(&mut self) {
        self.set_font_size(crate::core::prefs::DEFAULT_FONT_SIZE);
    }

    /// 清空所有面板的视图态（文档被替换/重载时调用：旧的行号坐标已失效）。
    pub fn clear_pane_view_states(&mut self) {
        for p in &mut self.panes {
            p.clear_selection();
            p.scroll_target = None;
            p.max_line_width = 0.0;
            p.fileset_override = None;
            p.selected_word.clear(); // 旧文档的词已无意义，别被 ⌘F 带入新检索
        }
    }

    /// 向右（`SplitDir::Horizontal`）或向下（`Vertical`）拆分出一个新面板。
    pub fn split_pane(&mut self, dir: SplitDir) {
        self.pane_layout.split(dir);
        self.ensure_panes();
        self.active_pane = self.pane_layout.count - 1; // 新面板即活动面板
        self.sync_active_pane_mirror();
    }

    /// 关闭指定面板；至少保留一个。被关的是活动面板时把活动权交给前一个。
    pub fn close_pane(&mut self, index: usize) {
        if !self.pane_layout.close(index) {
            return;
        }
        if index < self.panes.len() {
            self.panes.remove(index);
        }
        self.active_pane = self.active_pane.min(self.panes.len().saturating_sub(1));
        self.sync_active_pane_mirror();
    }

    /// 关闭「当前打开的文件」，回到空面板初始态（单面板时点 ✕ 触发）。
    ///
    /// 与「打开文件整体替换」的清理清单一致：清空全局文件集、检索态、每面板视图态、
    /// 顶层镜像与脏标记，状态栏提示「未打开文件」。多面板时不走此方法（点 ✕ = 关闭面板）。
    pub fn close_current_file(&mut self) {
        self.fileset.clear();
        self.search_results.clear();
        self.search_truncated = false;
        self.search_error = None;
        self.hit_regex = None;
        self.hit_cursor = None;
        // 视图态是每面板独立持有的，必须遍历清空（含各面板单独打开的文件集）。
        self.clear_pane_view_states();
        self.selected_row = None;
        self.sidebar_active_file = None;
        self.dirty_files.clear();
        self.max_line_width = 0.0;
        self.in_result_mode = false;
        self.status_text = "未打开文件".to_owned();
        self.sync_active_pane_mirror();
    }

    /// 把活动面板的 `wrap`/`selected_row`/`max_line_width`/`in_result_mode` 同步到 `AppState`
    /// 顶层镜像字段（工具栏、状态栏、快捷键读的是这些镜像，避免它们关心面板下标）。
    pub fn sync_active_pane_mirror(&mut self) {
        let (wrap, sel, w, res) = match self.active_pane() {
            Some(p) => (p.wrap, p.selected_row, p.max_line_width, p.in_result_mode),
            None => (false, None, 0.0, false),
        };
        self.wrap = wrap;
        self.selected_row = sel;
        self.max_line_width = w;
        self.in_result_mode = res;
    }

    /// 把顶层镜像的 `wrap` 写回活动面板（工具栏切换折行时调用）。
    pub fn apply_wrap_to_active_pane(&mut self) {
        let wrap = self.wrap;
        self.active_pane_mut().wrap = wrap;
    }

    /// 把一批路径加载到**活动面板**（写入 `fileset_override`，与全局文件集脱钩）。
    ///
    /// 复用 `load_paths` 的校验（空文件跳过、32GB 上限），但只影响活动面板的视图，
    /// 其余面板与全局 `fileset` 保持不变。拆分面板后，普通「打开文件」走此路径，
    /// 使两个面板可分别显示不同文件（spec §7.7.7）。
    pub fn open_paths_to_active_pane(&mut self, paths: Vec<PathBuf>) {
        let mut opened: Vec<Arc<LogFileIndex>> = Vec::new();
        let mut bytes_total: u64 = 0;
        let mut loaded = 0usize;
        let mut skipped = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for path in paths {
            let bytes = match std::fs::metadata(&path).map(|m| m.len()) {
                Ok(b) => b,
                Err(e) => {
                    skipped += 1;
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            if bytes_total + bytes > 32 * 1024 * 1024 * 1024 {
                skipped += 1;
                errors.push(format!(
                    "{}: 累计超过 {} 上限，已跳过",
                    path.display(),
                    crate::util::human_bytes(32 * 1024 * 1024 * 1024)
                ));
                continue;
            }
            match LogFileIndex::open(&path) {
                Ok(idx) => {
                    bytes_total += bytes;
                    opened.push(Arc::new(idx));
                    self.recents.push(path.clone());
                    loaded += 1;
                }
                Err(IndexError::Empty(_)) => {
                    skipped += 1;
                    errors.push(format!("{}: 空文件，已跳过", path.display()));
                }
                Err(e) => {
                    skipped += 1;
                    errors.push(format!("{e}"));
                }
            }
        }

        if loaded == 0 {
            let tail = errors.join("；");
            self.status_text = if errors.is_empty() {
                "未选择文件".to_owned()
            } else {
                format!("未加载任何文件（跳过 {skipped}）；{tail}")
            };
            return;
        }

        self.recents.save();

        let mut fs = crate::core::indexer::FileSet::default();
        for idx in opened {
            fs.push(idx);
        }

        // 只替换活动面板的视图：旧行号坐标失效，清该面板视图态（不碰全局 fileset/其他面板）。
        let pane_idx = self.active_pane;
        let pane = &mut self.panes[pane_idx];
        pane.fileset_override = Some(fs);
        pane.selected_row = None;
        pane.scroll_target = None;
        pane.max_line_width = 0.0; // 惰性重算
        self.sync_active_pane_mirror();

        let total = self.panes[pane_idx]
            .fileset_override
            .as_ref()
            .map(|f| f.total_lines())
            .unwrap_or(0);
        self.status_text = if errors.is_empty() {
            format!(
                "面板 {} 已打开 {loaded} 个文件，共 {total} 行",
                pane_idx + 1
            )
        } else {
            let tail = errors.join("；");
            format!(
                "面板 {} 已打开 {loaded} 个文件（跳过 {skipped}），共 {total} 行；{tail}",
                pane_idx + 1
            )
        };
    }
}

pub struct LogViewerApp {
    state: AppState,
    search_tx: Sender<SearchMessage>,
    search_rx: Receiver<SearchMessage>,
    /// 当前检索的取消令牌；完成后置 `None`。
    search_cancel: Option<CancelToken>,
    /// 导出后台消息通道。
    export_tx: Sender<ExportMessage>,
    export_rx: Receiver<ExportMessage>,
    /// 当前导出的取消令牌；完成后置 `None`。
    export_cancel: Option<CancelToken>,
    /// 目录检索后台消息通道。
    grep_tx: Sender<GrepMessage>,
    grep_rx: Receiver<GrepMessage>,
    /// 当前目录检索的取消令牌；完成后置 `None`。
    grep_cancel: Option<CancelToken>,
}

/// CJK 兜底字体在 `FontDefinitions::font_data` 中的键名。
///
/// assets/fonts 仅保留 3 个实际嵌入的字体文件（其余未使用字重已移除）：
/// `NotoSerifSC-Medium`(中文兜底) / `SauceCodeProNerdFont-Regular`(等宽主字体) /
/// `SauceCodeProNerdFont-Bold`(命中加粗独立族)。字重选择的核心约束是
/// **与拉丁主字体的字重相称**：
/// 日志正文拉丁部分固定走 `SauceCodeProNerdFont-Regular`(400)，若中文用
/// `Black`(900) 会出现「英文细、中文极粗」的割裂感（`Regular`(400) 又因衬线
/// 笔画纤细在同字号下显小）。取 **`Medium`(500)**：比 Regular 粗一档，暗色主题下
/// 中文更清晰，与拉丁 400 搭配不突兀。
///
/// 注意命名歧义：**"Black" 是字重（900，超粗），不是"黑体"（无衬线）**——
/// NotoSerifSC 全系都是衬线。若要真正的无衬线黑体需另找字体（仓库内暂无）。
/// 各字重 CJK advance 均为 1.0em，换字重不影响 `WIDE_W` 等字宽估算常量。
const FONT_CJK: &str = "NotoSerifSC";

/// 等宽主字体在 `FontDefinitions::font_data` 中的键名。
///
/// SauceCodePro Nerd Font 是等宽代码字体，且内嵌 Nerd Font 图标字形（如箭头、
/// 几何符号），用它替换 egui 内置 Hack 作 Monospace 族**主字体**，日志正文的
/// 代码/符号显示更佳，也顺带覆盖了此前「几何/箭头符号字形覆盖不可靠」需自绘
/// 矢量规避的痛点。
const FONT_MONO: &str = "SauceCodeProNerdFont";

/// 等宽**粗体**在 `FontDefinitions::font_data` 中的键名。
///
/// 单独注册为 [`theme::FONT_FAMILY_MONO_BOLD`] 族（不并入 Monospace，否则全文变粗），
/// 仅用于检索命中的文本段：VS Code 的查找命中除了底色还有字重强调，命中在长行里
/// 更容易被肉眼扫到。体积约 2.4MB，可接受。
///
/// 该族同样要追加 CJK 兜底——SauceCodePro 无 CJK 字形，否则命中的中文会变豆腐块
/// （中文因此不参与加粗，回退到 Medium，属可接受的降级）。
const FONT_MONO_BOLD: &str = "SauceCodeProNerdFontBold";

/// 配置中文字体与等宽字体。
///
/// egui 内置字体（Hack / Ubuntu-Light / NotoEmoji）**均不含 CJK 字形**，
/// 未额外配置时界面与日志中的中文会渲染成空白方块（豆腐块）。
///
/// 配置策略：
/// - **Monospace 族**：把 SauceCodePro Nerd Font 前置为主字体（`push` 到末尾会让它被
///   内置 Hack 抢先，故需 `insert(0)` 放到最前），拉丁/代码/符号走它，等宽对齐保持；
///   再追加 NotoSerifSC 兜底中文，内置 Hack/NotoEmoji 仍留在族内做最后回退。
/// - **Proportional 族**：不动内置主字体（Ubuntu-Light），仅把 NotoSerifSC 追加到末尾
///   做中文兜底，拉丁仍由内置字体绘制、界面观感不变。
///
/// 字体经 `include_bytes!` 编译进二进制，打包为 `.app` 后无需附带资源目录。
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_CJK.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/NotoSerifSC-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        FONT_MONO.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/SauceCodeProNerdFont-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        FONT_MONO_BOLD.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/SauceCodeProNerdFont-Bold.ttf"
        ))),
    );

    // Monospace：SauceCodePro 主字体（最前）+ 中文兜底 + 内置回退。
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, FONT_MONO.to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push(FONT_CJK.to_owned());

    // Proportional：内置主字体不变，仅追加中文兜底。
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push(FONT_CJK.to_owned());

    // 命中强调族：等宽粗体 + 中文兜底（SauceCodePro 无 CJK 字形，缺兜底会豆腐块）。
    fonts.families.insert(
        egui::FontFamily::Name(theme::FONT_FAMILY_MONO_BOLD.into()),
        vec![FONT_MONO_BOLD.to_owned(), FONT_CJK.to_owned()],
    );

    ctx.set_fonts(fonts);
}

impl LogViewerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial_paths: Vec<std::path::PathBuf>,
        prefs: Prefs,
    ) -> Self {
        log::info!("Hyper Log starting up");
        // 编辑器风格主题（按持久化偏好，默认暗色），再装字体：主题只改颜色，字体与主题互不影响。
        theme::apply(&cc.egui_ctx, prefs.theme.into());
        setup_fonts(&cc.egui_ctx);
        let show_perf = std::env::var("HYPER_LOG_PERF")
            .map(|v| v == "1")
            .unwrap_or(false);
        let (search_tx, search_rx) = crossbeam_channel::unbounded();
        let (export_tx, export_rx) = crossbeam_channel::unbounded();
        let (grep_tx, grep_rx) = crossbeam_channel::unbounded();
        let mut app = Self {
            state: AppState {
                show_perf,
                recents: Recents::load(),
                prefs: prefs.clone(),
                ..Default::default()
            },
            search_tx,
            search_rx,
            search_cancel: None,
            export_tx,
            export_rx,
            export_cancel: None,
            grep_tx,
            grep_rx,
            grep_cancel: None,
        };
        // 回填实时开关（折行/侧栏）：持久化的是唯一真相，实时字段初值取自偏好。
        app.state.show_sidebar = app.state.prefs.sidebar_visible;
        // 正文字号同理取自偏好（`⌘+`/`⌘-` 缩放后落盘）。
        app.state.font_size = app.state.prefs.font_size.clamp(
            crate::core::prefs::MIN_FONT_SIZE,
            crate::core::prefs::MAX_FONT_SIZE,
        );
        // 面板：每次启动固定为**单面板**（拆分是会话内临时状态，不跨启动持久化）。
        // 仅保留分割比例（用户下次拆分时的布局偏好），方向默认左右、数量恒 1。
        app.state.pane_layout = PaneLayout {
            dir: SplitDir::Horizontal,
            count: 1,
            ratio: app.state.prefs.split_ratio,
        };
        app.state.ensure_panes();
        // 折行是每面板独立的状态，初值取自偏好后同步进活动面板。
        app.state.wrap = app.state.prefs.wrap;
        app.state.apply_wrap_to_active_pane();
        // 启动即载入（命令行 `--open`/位置参数）：M16 为支撑实机观测与「终端秒开日志」而加。
        // 目录在此展开为日志文件列表，使 `hyper-log <dir>` 与「打开目录」等价。
        let initial_paths = Self::expand_initial_paths(initial_paths);
        if !initial_paths.is_empty() {
            app.load_paths(initial_paths);
        }
        app
    }

    /// 展开初始路径：目录递归展开为日志文件（复用目录扫描），文件保持原样。
    ///
    /// 命令行既可以是文件也可以是目录（拖目录到图标/终端传目录都应当可用）；
    /// 目录展开失败（无日志文件）只告警，不中断其余路径的加载。
    fn expand_initial_paths(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            if p.is_dir() {
                let files = crate::core::dirscan::collect_log_files(&p);
                if files.is_empty() {
                    log::warn!(
                        "目录 {} 下未找到日志文件（.log/.txt/.out 或无后缀）",
                        p.display()
                    );
                }
                out.extend(files);
            } else {
                out.push(p);
            }
        }
        out
    }

    /// 全局快捷键（spec §7.7 常用快捷键）。
    ///
    /// 用 `consume_shortcut` 一次性消费，避免重复触发。`⌘O`/`⌘⇧O` 在检索中仍受 G1 禁用
    /// （`pending_open`/`pending_open_dir` 的落实处已加 `!is_searching` 守卫）。
    fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let cmd = egui::Modifiers::COMMAND;
        let cmd_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;

        let pressed = |ctx: &egui::Context, m: egui::Modifiers, k: egui::Key| {
            ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(m, k)))
        };
        // 字号缩放专用：忽略 key-repeat。`consume_shortcut`（→`count_and_consume_key`）对
        // repeat 事件同样返回 true（其 `matches!` 用 `pressed: true, ..` 吞掉了 `repeat` 字段），
        // 长按 ⌘- 一秒就会把字号从 12.5 一路跌到下限 8.0 并落盘，表现为「文字突然变得特别小」。
        // 这里改为只消费「非 repeat」的按下事件，长按仅缩放一次，需多次短按才能持续缩放。
        let zoom_pressed = |ctx: &egui::Context, m: egui::Modifiers, k: egui::Key| {
            let mut matched = false;
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    let is_match = matches!(
                        e,
                        egui::Event::Key {
                            key,
                            modifiers,
                            pressed: true,
                            repeat: false,
                            ..
                        } if *key == k && modifiers.matches_logically(m)
                    );
                    matched |= is_match;
                    !is_match
                });
            });
            matched
        };

        if pressed(ctx, cmd, egui::Key::O) {
            self.state.pending_open = true;
        }
        if pressed(ctx, cmd_shift, egui::Key::O) {
            self.state.pending_open_dir = true;
        }
        if pressed(ctx, cmd, egui::Key::F) {
            // 正文中选中过文本就把它带入检索框——VS Code「⌘F 带入选中文本」的轻量版。
            // egui 拿不到 Label 选区文本（`LabelSelectionState` 只有 `has_selection`），
            // 故由 `log_view` 在双击取词 / 拖动选择时按指针 x 反查字符索引，存进面板。
            let word = self
                .state
                .active_pane()
                .map(|p| p.selected_word.clone())
                .unwrap_or_default();
            if !word.is_empty() {
                self.state.search_pattern = word;
            }
            self.state.focus_search = true;
        }
        if pressed(ctx, cmd, egui::Key::L) {
            self.state.focus_line_jump = true;
        }
        if pressed(ctx, cmd, egui::Key::B) {
            self.state.show_sidebar = !self.state.show_sidebar;
            self.state.save_prefs();
        }
        // ⇧⌘G / ⌘G：在命中之间导航（VS Code Find Previous / Next）。
        // 先判 ⇧⌘G：egui 的 `Modifiers::matches` 是「包含」语义，⌘G 也会匹配 ⇧⌘G 的按键，
        // 反过来则不会；且 `consume_shortcut` 会摘掉该事件，前者命中后后者本帧不再触发。
        if pressed(ctx, cmd_shift, egui::Key::G) {
            self.state.goto_hit(HitDir::Prev);
        } else if (pressed(ctx, cmd, egui::Key::G) || pressed(ctx, cmd, egui::Key::Enter))
            && !self.state.is_searching
        {
            if self.state.search_results.is_empty() {
                // 还没检索过：⌘G / ⌘↵ 仍等价「查找」按钮（旧行为）。
                if !self.state.search_pattern.trim().is_empty() {
                    self.state.pending_search = true;
                }
            } else {
                self.state.goto_hit(HitDir::Next);
            }
        }
        // F3 / ⇧F3：命中导航（Windows/Linux 惯用的 Find Next / Previous）。
        // 用 `key_pressed` + `modifiers.shift` 判断方向，避免 `consume_shortcut` 的
        // 「包含」语义把无修饰的 F3 也当成 ⇧F3。
        if ctx.input(|i| i.key_pressed(egui::Key::F3)) {
            let prev = ctx.input(|i| i.modifiers.shift);
            self.state
                .goto_hit(if prev { HitDir::Prev } else { HitDir::Next });
        }
        // ⌘+ / ⌘- / ⌘0：正文字号缩放（VS Code 同款），改动即落盘。
        // 用 `zoom_pressed`（忽略 key-repeat）而非 `pressed`，避免长按瞬间缩到极值。
        if zoom_pressed(ctx, cmd, egui::Key::Plus) {
            self.state.zoom_font(1.0);
        }
        if zoom_pressed(ctx, cmd, egui::Key::Minus) {
            self.state.zoom_font(-1.0);
        }
        if zoom_pressed(ctx, cmd, egui::Key::Num0) {
            self.state.reset_font_size();
        }
        // Esc：退出**活动面板**的命中视图（返回全量日志）；否则清除活动面板选中行。
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            let active_in_res = self
                .state
                .active_pane()
                .map(|p| p.in_result_mode)
                .unwrap_or(false);
            if active_in_res && !self.state.search_results.is_empty() {
                self.state.active_pane_mut().in_result_mode = false;
                self.state.in_result_mode = false;
            } else {
                self.state.selected_row = None;
                self.state.active_pane_mut().selected_row = None;
            }
        }
    }

    /// 通过系统原生对话框选择并加载日志文件。
    ///
    /// 单文件 > 16 GiB 由 `LogFileIndex::open` 拒绝；累计 > 32 GiB 在此处拒绝。
    /// 空文件与索引错误会跳过并在状态栏提示，不会中断其余文件加载。
    pub fn open_files(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("打开日志文件")
            .add_filter("日志文件", &["log", "txt", "out"])
            .add_filter("所有文件", &["*"])
            .pick_files();

        let Some(paths) = picked else {
            self.state.status_text = "已取消打开".to_owned();
            return;
        };

        self.open_paths(paths);
    }

    /// 通过系统原生对话框选择目录，递归收集目录下的日志文件并批量加载（Q「打开目录」）。
    pub fn open_directory(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("打开日志目录")
            .pick_folder();

        let Some(dir) = picked else {
            self.state.status_text = "已取消打开目录".to_owned();
            return;
        };

        let files = crate::core::dirscan::collect_log_files(&dir);
        if files.is_empty() {
            self.state.status_text = format!(
                "目录 {} 下未找到日志文件（.log/.txt/.out 或无后缀）",
                dir.display()
            );
            return;
        }
        self.open_paths(files);
    }

    /// 打开一批路径的**入口**：决定加载到全局文件集还是活动面板。
    ///
    /// - 未拆分（`pane_layout.count == 1`）：与旧行为一致，全局替换 `fileset`。
    /// - 已拆分（≥2 面板）：加载到**活动面板**的 `fileset_override`，两个面板可分别显示
    ///   不同文件，便于对比（spec §7.7.7）。
    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        if self.state.pane_layout.count <= 1 {
            self.load_paths(paths);
        } else {
            self.state.open_paths_to_active_pane(paths);
        }
    }

    /// 加载一批路径：文件对话框与「最近文件」共用同一套校验与提示逻辑（M11）。
    ///
    /// 语义为**替换**而非追加：打开新文件即切换当前文档，与编辑器「打开文件」一致
    /// （早期版本是追加，导致打开第二个文件后视口仍停在第一个文件的内容上，像是没刷新）。
    ///
    /// 先把新文件全部打开成功再整体替换，避免中途失败时旧文件已被清空、又没有新文件可显示。
    /// 若一个都没打开成功，则保留原集合不动，只报告错误。
    fn load_paths(&mut self, paths: Vec<PathBuf>) {
        let mut loaded = 0usize;
        let mut skipped = 0usize;
        let mut errors: Vec<String> = Vec::new();
        // 新集合先攒在临时 Vec 里，全部成功后再接管 `fileset`。
        let mut opened: Vec<Arc<LogFileIndex>> = Vec::new();
        let mut bytes_total: u64 = 0;

        for path in paths {
            let bytes = match std::fs::metadata(&path).map(|m| m.len()) {
                Ok(b) => b,
                Err(e) => {
                    skipped += 1;
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            // 替换语义：上限只针对本次打开的新集合，不含即将被替换掉的旧文件。
            if bytes_total + bytes > 32 * 1024 * 1024 * 1024 {
                skipped += 1;
                errors.push(format!(
                    "{}: 累计超过 {} 上限，已跳过",
                    path.display(),
                    crate::util::human_bytes(32 * 1024 * 1024 * 1024)
                ));
                continue;
            }

            match LogFileIndex::open(&path) {
                Ok(idx) => {
                    bytes_total += bytes;
                    opened.push(Arc::new(idx));
                    // 只有真正成功打开才记入最近文件（M11）。
                    self.state.recents.push(path.clone());
                    loaded += 1;
                }
                Err(IndexError::Empty(_)) => {
                    skipped += 1;
                    errors.push(format!("{}: 空文件，已跳过", path.display()));
                }
                Err(e) => {
                    skipped += 1;
                    errors.push(format!("{e}"));
                }
            }
        }

        if loaded > 0 {
            self.state.recents.save();
        } else {
            // 一个都没打开成功：保留原有内容，仅提示错误。
            let tail = errors.join("；");
            self.state.status_text = if errors.is_empty() {
                "未选择文件".to_owned()
            } else {
                format!("未加载任何文件（跳过 {skipped}）；{tail}")
            };
            log::warn!("{}", self.state.status_text);
            return;
        }

        // —— 整体替换当前文档 ——
        self.state.fileset.clear();
        for idx in opened {
            self.state.fileset.push(idx);
        }

        // 文档已换：旧的检索坐标按 file_idx 索引，指向的已不是同一批文件，必须清空；
        // 选中行 / 跳转目标 / 侧边栏高亮 / 脏标记同样失效。
        self.state.search_results.clear();
        self.state.search_truncated = false;
        self.state.search_error = None;
        self.state.hit_regex = None;
        self.state.hit_cursor = None; // 文档换了，旧命中游标失效
        // 视图态是每面板独立持有的，必须遍历清空（含各面板单独打开的文件集）。
        self.state.clear_pane_view_states();
        self.state.selected_row = None;
        self.state.sidebar_active_file = None;
        self.state.dirty_files.clear();

        // 新文件可能比已加载的更宽，重新估算横向滚动范围（活动面板）。
        let max_line_width =
            log_view::estimate_content_width(&self.state.fileset, self.state.font_size);
        self.state.max_line_width = max_line_width;
        self.state.active_pane_mut().max_line_width = max_line_width;

        let total = self.state.fileset.total_lines();
        let size = crate::util::human_bytes(self.state.fileset.total_bytes() as u64);
        self.state.status_text = if errors.is_empty() {
            format!("已加载 {loaded} 个文件，共 {total} 行 / {size}")
        } else {
            let tail = errors.join("；");
            format!("已加载 {loaded} 个文件（跳过 {skipped}），共 {total} 行 / {size}；{tail}")
        };
        log::info!("{}", self.state.status_text);
    }

    /// 重新加载所有已打开的文件（spec Q6「重新加载」）：关闭旧 mmap 索引（`FileSet::clear`
    /// 释放 `Arc<LogFileIndex>`）并重新打开，使外部 truncate/轮转后的内容生效。
    ///
    /// 重载后行号语义整体变化，故清空选中行/跳转目标/侧边栏高亮/脏标记。后台检索等持有的是旧
    /// `Arc`，旧 mmap 在其结束前仍有效（不触发 SIGBUS），本方法已在 `ui` 中于非活动期调用（G1）。
    fn reload_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .state
            .fileset
            .files()
            .iter()
            .map(|f| f.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.state.fileset.clear();
        // 视图态是每面板独立持有的，重载后旧行号坐标失效，遍历清空。
        self.state.clear_pane_view_states();
        self.state.selected_row = None;
        self.state.sidebar_active_file = None;
        self.state.dirty_files.clear();
        self.state.status_text = "重新加载中…".to_owned();
        self.load_paths(paths);
    }

    /// 启动一次检索：快照已加载文件，后台线程流式回传结果（MVP 禁止并发）。
    fn start_search(&mut self) {
        let pattern = self.state.search_pattern.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        if self.state.fileset.file_count() == 0 {
            self.state.status_text = "请先打开日志文件".to_owned();
            return;
        }
        // 记录最近检索词（M17 偏好持久化），下次启动后可用于检索历史下拉。
        self.state.prefs.push_recent_search(&pattern);
        self.state.save_prefs();

        let options = SearchOptions {
            mode: self.state.search_mode,
            case_sensitive: self.state.search_case_sensitive,
            max_hits: MAX_HITS,
        };
        let files: Vec<Arc<LogFileIndex>> = self.state.fileset.files().to_vec();
        // 与检索复用同一 `Regex` 供结果命中高亮（G5）。
        let hit_re = crate::core::search::build_regex(&options, &pattern).ok();

        let cancel = CancelToken::new();
        self.search_cancel = Some(cancel.clone());
        self.state.is_searching = true;
        self.state.search_results.clear();
        self.state.search_error = None;
        // 新的一批结果：旧游标指向的是上一批的下标，必须重置（否则 F3 会跳到无关行）。
        self.state.hit_cursor = None;
        self.state.search_truncated = false;
        self.state.search_progress = (0, self.state.fileset.total_bytes() as u64);
        // 检索结果视图是「每面板独立」的：发起检索时把**活动面板**切到命中视图，
        // 其它面板保持全量，从而可以做到「A 面板全量 + B 面板命中」的并排对比（spec §7.7.7）。
        self.state.active_pane_mut().in_result_mode = true;
        self.state.in_result_mode = true; // 顶层镜像
        self.state.hit_regex = hit_re;
        self.state.status_text = "检索中…".to_owned();

        let tx = self.search_tx.clone();
        std::thread::spawn(move || {
            crate::core::search::run_search(&files, &pattern, &options, &cancel, &tx);
        });
    }

    /// 启动一次目录检索（"查找全部"）：选择目录后递归检索其下所有日志文件。
    ///
    /// 与单文件检索（[`Self::start_search`]）互斥：目录检索运行期间禁用普通检索。
    fn start_grep(&mut self, root: PathBuf) {
        let pattern = self.state.search_pattern.trim().to_string();
        if pattern.is_empty() {
            return;
        }

        let options = GrepOptions {
            search: SearchOptions {
                mode: self.state.search_mode,
                case_sensitive: self.state.search_case_sensitive,
                max_hits: MAX_HITS,
            },
            max_hits: MAX_HITS,
            base: Some(root.clone()),
        };

        let cancel = CancelToken::new();
        self.grep_cancel = Some(cancel.clone());
        self.state.is_grepping = true;
        self.state.show_results = true;
        self.state.grep_hits.clear();
        self.state.grep_selected_row = None;
        self.state.grep_truncated = false;
        self.state.grep_error = None;
        self.state.grep_progress = (0, 0, 0);
        self.state.grep_root = Some(root.clone());
        self.state.status_text = "查找全部…".to_owned();

        let tx = self.grep_tx.clone();
        std::thread::spawn(move || {
            crate::core::grepdir::run_grep(root, &pattern, &options, &cancel, &tx);
        });
    }

    /// 把目录检索命中结果写盘保存（复用 [`crate::core::grepdir::GrepHit`] 内联文本）。
    fn save_grep_results(&mut self, dest: PathBuf) {
        let text = results_view::hits_to_text(&self.state.grep_hits);
        match std::fs::write(&dest, text) {
            Ok(_) => {
                self.state.status_text = format!(
                    "已保存 {} 条命中到 {}",
                    self.state.grep_hits.len(),
                    dest.display()
                );
            }
            Err(e) => {
                self.state.grep_error = Some(format!("保存失败 {}: {e}", dest.display()));
                self.state.status_text = "保存结果失败，见结果页".to_owned();
            }
        }
    }

    /// 点击目录检索结果 → 跳转到原文对应行（notepad++ 风格）。
    ///
    /// 命中文件可能并不在当前 `fileset` 中（目录检索是临时打开、检索完即释放）：
    /// - 若目标文件已在 `fileset`，直接按 `file_global_start + 行号` 定位；
    /// - 否则用 [`Self::load_paths`] 打开它（替换语义，与「打开文件」一致），
    ///   随后按该文件的全局起始行定位。
    ///
    /// 定位后清掉结果页选区，把正文滚动到该行并高亮（`scroll_target` + `selected_row`）。
    fn jump_to_grep_hit(&mut self, path: &std::path::Path, line_number: usize) {
        // 目标文件是否已在当前文件集合中？
        let existing = self
            .state
            .fileset
            .files()
            .iter()
            .position(|f| f.path == path);

        if existing.is_none() {
            // 不在：打开该文件（替换语义）。失败则仅提示，不打断结果页。
            self.load_paths(vec![path.to_path_buf()]);
            if self.state.fileset.file_count() == 0 {
                self.state.status_text = format!("无法打开 {} 以跳转", path.display());
                return;
            }
        }

        // 定位到目标文件内行号：file_idx → 全局起始行 + (line_number - 1)。
        let file_idx = existing.unwrap_or(0);
        let Some(file) = self.state.fileset.file(file_idx) else {
            return;
        };
        let local = (line_number.saturating_sub(1)).min(file.line_count().saturating_sub(1));
        let Some(start) = self.state.fileset.file_global_start(file_idx) else {
            return;
        };
        let row = start + local;
        // 跳转只作用于活动面板（spec §7.7.7）。
        self.state.jump_to_row(row);
        self.state.active_pane_mut().selected_row = Some(row);
        self.state.selected_row = Some(row);
        self.state.sidebar_active_file = Some(file_idx);
    }

    /// 处理一条后台目录检索消息（由 `logic` 每帧 drain）。
    fn handle_grep_msg(&mut self, msg: GrepMessage) {
        match msg {
            GrepMessage::Partial { hits } => {
                self.state.grep_hits.extend(hits);
            }
            GrepMessage::Progress {
                files_done,
                files_total,
                bytes_done,
            } => {
                self.state.grep_progress = (files_done, files_total, bytes_done);
                let pct = if files_total > 0 {
                    files_done as f64 / files_total as f64 * 100.0
                } else {
                    0.0
                };
                self.state.status_text = format!(
                    "查找全部… {pct:.0}% · {} 命中",
                    crate::util::group_digits(self.state.grep_hits.len())
                );
            }
            GrepMessage::Truncated { hits } => {
                self.state.grep_truncated = true;
                self.state.status_text = format!("结果已截断（>{hits} 条），请缩小范围");
            }
            GrepMessage::Completed {
                hits,
                files,
                elapsed,
            } => {
                self.state.is_grepping = false;
                self.grep_cancel = None;
                self.state.status_text = format!(
                    "查找全部完成：{files} 个文件中 {} 行命中，耗时 {:.2?}",
                    crate::util::group_digits(hits),
                    elapsed
                );
            }
            GrepMessage::Failed(e) => {
                self.state.is_grepping = false;
                self.grep_cancel = None;
                self.state.grep_error = Some(e);
                self.state.status_text = "查找全部失败，见结果页".to_owned();
            }
            GrepMessage::Cancelled => {
                self.state.is_grepping = false;
                self.grep_cancel = None;
                self.state.status_text = "已取消查找".to_owned();
            }
        }
    }

    /// 通过系统原生保存对话框选择导出目标，并启动后台流式导出（M5 / T17）。
    ///
    /// 与检索复用同一份命中坐标；`Arc` 共享 FileSet 与命中 Vec，引擎内部不再 clone 大对象。
    fn start_export(&mut self, dest: PathBuf) {
        if self.state.search_results.is_empty() {
            return;
        }
        if self.state.fileset.file_count() == 0 {
            self.state.status_text = "没有可导出的文件".to_owned();
            return;
        }

        let files = Arc::new(self.state.fileset.clone());
        let hits = Arc::new(self.state.search_results.clone());
        let format = if self.state.export_with_prefix {
            ExportFormat::WithPrefix
        } else {
            ExportFormat::RawLines
        };

        let cancel = CancelToken::new();
        self.export_cancel = Some(cancel.clone());
        self.state.is_exporting = true;
        self.state.export_progress = (0, hits.len());
        self.state.export_error = None;
        self.state.export_path = None;
        self.state.status_text = "导出中…".to_owned();

        let tx = self.export_tx.clone();
        std::thread::spawn(move || {
            crate::core::export::export_async(files, hits, dest, format, cancel, tx);
        });
    }

    /// 处理一条后台导出消息（由 `logic` 每帧 drain）。
    fn handle_export_msg(&mut self, msg: ExportMessage) {
        match msg {
            ExportMessage::Progress { done, total } => {
                self.state.export_progress = (done, total);
                let pct = if total > 0 {
                    done as f64 / total as f64 * 100.0
                } else {
                    100.0
                };
                self.state.status_text = format!("导出中… {pct:.0}%");
            }
            ExportMessage::Completed { path, bytes } => {
                self.state.is_exporting = false;
                self.export_cancel = None;
                self.state.export_path = Some(path.clone());
                self.state.status_text = format!(
                    "导出完成：{} ({} 字节)",
                    path.display(),
                    crate::util::human_bytes(bytes)
                );
            }
            ExportMessage::Failed(e) => {
                self.state.is_exporting = false;
                self.export_cancel = None;
                self.state.export_error = Some(e);
                self.state.status_text = "导出失败，见状态栏".to_owned();
            }
            ExportMessage::Cancelled => {
                self.state.is_exporting = false;
                self.export_cancel = None;
                self.state.status_text = "已取消导出".to_owned();
            }
        }
    }

    /// 处理一条后台检索消息（由 `logic` 每帧 drain）。
    fn handle_search_msg(&mut self, msg: SearchMessage) {
        match msg {
            SearchMessage::Partial {
                hits,
                bytes_done,
                bytes_total,
            } => {
                self.state.search_results.extend(hits);
                self.state.search_progress = (bytes_done, bytes_total);
                let pct = if bytes_total > 0 {
                    bytes_done as f64 / bytes_total as f64 * 100.0
                } else {
                    100.0
                };
                self.state.status_text = format!(
                    "检索中… {pct:.0}% · {} 命中",
                    self.state.search_results.len()
                );
            }
            SearchMessage::Truncated { hits } => {
                self.state.search_truncated = true;
                self.state.status_text = format!("结果已截断（>{hits} 条），请缩小范围");
            }
            SearchMessage::Completed { hits, elapsed } => {
                self.state.is_searching = false;
                self.search_cancel = None;
                self.state.status_text = format!("检索完成：{} 行命中，耗时 {:.2?}", hits, elapsed);
            }
            SearchMessage::Failed(e) => {
                self.state.is_searching = false;
                self.search_cancel = None;
                self.state.search_error = Some(error_text(&e));
                self.state.active_pane_mut().in_result_mode = false;
                self.state.in_result_mode = false;
                self.state.status_text = "检索失败，见检索框下方提示".to_owned();
            }
            SearchMessage::Cancelled => {
                self.state.is_searching = false;
                self.search_cancel = None;
                self.state.status_text = "已取消检索".to_owned();
            }
        }
    }
}

fn error_text(e: &SearchError) -> String {
    e.to_string()
}

impl eframe::App for LogViewerApp {
    /// 每帧 UI 绘制前调用；后台消息轮询放这里（不绘制 UI，窗口隐藏时也推进）。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 性能 HUD：累计帧耗时（仅在有帧处理时更新；空闲时不重绘 → HUD 冻结，正是 P12 期望的空闲行为）。
        let now = ctx.input(|i| i.time);
        if self.state.last_frame_sec > 0.0 {
            let dt = (now - self.state.last_frame_sec) as f32 * 1000.0;
            if (0.0..=1000.0).contains(&dt) {
                self.state.frame_ms.push(dt);
                if self.state.frame_ms.len() > 120 {
                    self.state.frame_ms.remove(0);
                }
            }
        }
        self.state.last_frame_sec = now;

        // Q6：节流检测外部文件修改（约每秒一次），避免逐帧 stat 所有文件。
        // 只读检测、不触碰 mmap，因此不会触发 macOS 上 truncate 导致的 SIGBUS（D15）。
        if now - self.state.last_dirty_check > 1.0 {
            self.state.last_dirty_check = now;
            self.state.dirty_files = self.state.fileset.detect_dirty();
            if !self.state.dirty_files.is_empty() {
                ctx.request_repaint();
            }
        }

        let mut n = 0;
        while n < MAX_MSG_PER_FRAME {
            match self.search_rx.try_recv() {
                Ok(msg) => {
                    self.handle_search_msg(msg);
                    n += 1;
                }
                Err(_) => break,
            }
        }
        // 导出消息与检索消息使用独立通道，但共享每帧 drain 上限。
        while n < MAX_MSG_PER_FRAME {
            match self.export_rx.try_recv() {
                Ok(msg) => {
                    self.handle_export_msg(msg);
                    n += 1;
                }
                Err(_) => break,
            }
        }
        // 目录检索消息（"查找全部"）同样共享每帧 drain 上限。
        while n < MAX_MSG_PER_FRAME {
            match self.grep_rx.try_recv() {
                Ok(msg) => {
                    self.handle_grep_msg(msg);
                    n += 1;
                }
                Err(_) => break,
            }
        }
        // 仅后台有活动时才请求重绘，空闲时不烧 CPU（spec §8.4）。
        if self.state.is_searching || self.state.is_exporting || self.state.is_grepping {
            ctx.request_repaint();
        }

        // M17：窗口几何持久化（节流写盘，约每 2s 一次），下次启动恢复尺寸。
        // 仅在尺寸确有变化时才写，避免每帧触盘；prefs 的其它字段（主题/折行/侧栏/最近检索词）
        // 已由各自开关处即时保存，这里只更新 window 部分。
        if let Some(size) = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()))
            && ((size.x - self.state.prefs.window_w).abs() > 1.0
                || (size.y - self.state.prefs.window_h).abs() > 1.0)
            && now - self.state.last_prefs_save > 2.0
        {
            self.state.prefs.window_w = size.x;
            self.state.prefs.window_h = size.y;
            self.state.last_prefs_save = now;
            self.state.prefs.save();
        }

        // 测量辅助（默认关闭）：HYPER_LOG_REPAINT=1 时强制每帧重绘，便于性能 HUD 在无人工
        // 滚动/检索时稳定采样 P4/P5/P6（不影响 P12——不设置该变量时空闲仍不重绘）。
        if std::env::var("HYPER_LOG_REPAINT")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(ui);

        if self.state.pending_search {
            self.state.pending_search = false;
            self.start_search();
        }
        if self.state.pending_stop {
            self.state.pending_stop = false;
            if let Some(c) = &self.search_cancel {
                c.cancel();
            }
        }
        if self.state.pending_open && !self.state.is_searching {
            self.state.pending_open = false;
            self.open_files();
        }
        // 打开目录（Q「打开目录」）：检索中禁用。
        if self.state.pending_open_dir && !self.state.is_searching {
            self.state.pending_open_dir = false;
            self.open_directory();
        }
        // 面板标题栏「本面板打开」：把单个文件单独载入该面板（写入 `fileset_override`），
        // 与全局文件集脱钩，便于对比两份不同日志。检索中禁用（G1）。
        if let Some(pane_idx) = self.state.pending_open_in_pane.take()
            && !self.state.is_searching
        {
            let picked = rfd::FileDialog::new()
                .set_title("在本面板打开日志文件")
                .add_filter("日志文件", &["log", "txt", "out"])
                .add_filter("所有文件", &["*"])
                .pick_file();
            if let Some(path) = picked {
                match LogFileIndex::open(&path) {
                    Ok(idx) => {
                        let mut fs = crate::core::indexer::FileSet::default();
                        fs.push(std::sync::Arc::new(idx));
                        // 文档换成单独文件，旧的行号坐标失效，清该面板视图态。
                        let pane = &mut self.state.panes[pane_idx];
                        pane.fileset_override = Some(fs);
                        pane.selected_row = None;
                        pane.scroll_target = None;
                        pane.max_line_width = 0.0; // 惰性重算
                        self.state.status_text =
                            format!("面板 {} 已单独打开 {}", pane_idx + 1, path.display());
                    }
                    Err(e) => {
                        self.state.status_text = format!("无法打开 {}: {e}", path.display());
                    }
                }
            }
        }
        // 查找全部：弹出目录选择，选中后启动目录检索（与普通检索互斥）。
        if self.state.pending_grep && !self.state.is_searching && !self.state.is_grepping {
            self.state.pending_grep = false;
            let picked = rfd::FileDialog::new()
                .set_title("查找目录（查找全部）")
                .pick_folder();
            if let Some(dir) = picked {
                self.start_grep(dir);
            }
        }
        // 保存目录检索结果。
        if self.state.pending_grep_save {
            self.state.pending_grep_save = false;
            let default_name = format!(
                "hyper-log-grep-{}.log",
                crate::util::export_filename_stamp()
            );
            let picked = rfd::FileDialog::new()
                .set_title("保存查找结果")
                .set_file_name(&default_name)
                .save_file();
            if let Some(path) = picked {
                self.save_grep_results(path);
            }
        }
        // 取消目录检索。
        if self.state.pending_grep_stop {
            self.state.pending_grep_stop = false;
            if let Some(c) = &self.grep_cancel {
                c.cancel();
            }
        }
        // 结果页点击命中 → 跳转原文：打开目标文件并定位到行。结果面板保持打开，
        // 便于连续点击多个结果（notepad++ 风格）；仅「返回日志」按钮显式关闭面板。
        if let Some((path, line_number)) = self.state.pending_grep_jump.take() {
            self.jump_to_grep_hit(&path, line_number);
        }
        // 重新加载（spec Q6「重新加载」）：清空旧索引并重新打开所有文件，使外部 truncate/轮转生效。
        // 检索/导出/目录检索进行中禁用（G1），避免与后台线程持有的旧 Arc<LogFileIndex> 竞争。
        if self.state.pending_reload
            && !self.state.is_searching
            && !self.state.is_exporting
            && !self.state.is_grepping
        {
            self.state.pending_reload = false;
            self.reload_all();
        }
        // 最近文件（M11）：同样在检索中禁用（G1）。
        if !self.state.is_searching
            && let Some(path) = self.state.pending_open_recent.take()
        {
            self.load_paths(vec![path]);
        }
        if self.state.pending_export {
            self.state.pending_export = false;
            // 保存对话框：默认文件名带本地时间戳（spec T17）。
            let default_name = format!(
                "hyper-log-export-{}.log",
                crate::util::export_filename_stamp()
            );
            let picked = rfd::FileDialog::new()
                .set_title("导出检索结果")
                .set_file_name(&default_name)
                .save_file();
            if let Some(path) = picked {
                self.start_export(path);
            }
        }
        if self.state.pending_export_cancel {
            self.state.pending_export_cancel = false;
            if let Some(c) = &self.export_cancel {
                c.cancel();
            }
        }

        toolbar::show(ui, &mut self.state);
        status_bar::show(ui, &mut self.state);
        // 左侧文件目录树（可折叠）：点击文件跳转其首行。
        if self.state.show_sidebar {
            egui::Panel::left("sidebar_panel")
                .resizable(true)
                .default_size(240.0)
                .frame(egui::Frame::default().fill(theme::palette(ui.ctx()).panel))
                .show(ui, |ui| {
                    sidebar::show(ui, &mut self.state);
                });
        }
        // 日志区用编辑器正文底色（与顶栏/底栏区分），且不留窗口内边距：
        // 行号槽要从最左侧开始，否则整块行背景会与正文错位。
        // 多面板拆分在中央区内按布局均分（spec §7.7.7）。
        let bg = theme::palette(ui.ctx()).bg;
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(bg))
            .show(ui, |ui| {
                render_log_area(ui, &mut self.state);
            });

        // 目录检索结果：独立浮动窗口（notepad++ 风格），不再挤压正文日志区布局。
        // 可自由拖动、可缩放、可折叠、可关闭；点击命中行跳转原文，窗口保持打开以便连续跳转。
        // `.open(&mut self.state.show_results)` 让窗口右上角 ✕ 与「返回日志」按钮
        // 都作用到同一个状态，关闭后窗口即消失。
        //
        // 定位：不用 `.anchor`（它会在窗口每次从关闭重开时强制拉回锚点，表现为「固定不可移动」），
        // 改用 `.default_pos` 仅**首次出现**时定位到底部居中，此后完全由用户拖动决定、egui 记忆。
        // 宽度：取主窗口内矩形的 80%（下限 400px 兜底超窄窗口），首次出现时生效。
        let mut open = self.state.show_results;
        let screen = ui.ctx().input(|i| i.content_rect());
        let target_w = (screen.width() * 0.8).max(400.0);
        let target_pos = egui::pos2(
            screen.left() + (screen.width() - target_w) / 2.0,
            screen.bottom() - 228.0, // 高度 220 + 底部留白 8
        );
        egui::Window::new("查找结果")
            .id(egui::Id::new("grep_results_window"))
            .open(&mut open)
            .movable(true)
            .resizable(true)
            .collapsible(true)
            .default_width(target_w)
            .default_height(220.0)
            .default_pos(target_pos)
            .show(ui.ctx(), |ui| {
                results_view::show(ui, &mut self.state);
            });
        self.state.show_results = open;

        // 性能 HUD（spec P4/P5/P6 可观测化）：仅在开启时绘制，且不主动请求重绘，
        // 避免拉高空闲 CPU（P12）。窗口内容仅在产生新帧时（滚动/检索）刷新。
        if self.state.show_perf {
            egui::Window::new("性能 (P4/P5/P6)")
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
                .resizable(false)
                .collapsible(true)
                .show(ui.ctx(), |ui| {
                    let ms = &self.state.frame_ms;
                    if ms.is_empty() {
                        ui.label("等待帧数据…（滚动或检索时更新）");
                        return;
                    }
                    let last = *ms.last().unwrap();
                    let avg = ms.iter().sum::<f32>() / ms.len() as f32;
                    let mut sorted = ms.clone();
                    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let p95_idx = ((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1);
                    let p95 = sorted[p95_idx];
                    let peak = *sorted.last().unwrap();
                    let fps = 1000.0 / avg.max(0.01);
                    ui.label(format!("FPS ≈ {fps:.0}"));
                    ui.label(format!("帧耗时  last {last:.1}ms / avg {avg:.1}ms"));
                    ui.label(format!("p95 {p95:.1}ms / 峰值 {peak:.1}ms"));
                    ui.label(format!(
                        "P4 (p95<16.6ms): {}",
                        if p95 < 16.6 { "✅" } else { "⚠️" }
                    ));
                    ui.label("P5: 虚拟滚动，节点数≈视口行+10（代码审查）");
                    ui.label("P6: 检索峰值帧见「峰值」行应 < 50ms");

                    // 测量辅助（默认关闭）：HYPER_LOG_PERF_LOG=1 时把指标打到 stderr，
                    // 便于无头/CI 环境采样 P4(p95)/P6(峰值)（配合 HYPER_LOG_REPAINT=1 持续重绘）。
                    if std::env::var("HYPER_LOG_PERF_LOG")
                        .map(|v| v == "1")
                        .unwrap_or(false)
                    {
                        let now = ui.ctx().input(|i| i.time);
                        if now - self.state.perf_log_last_sec > 1.0 {
                            self.state.perf_log_last_sec = now;
                            eprintln!(
                                "hyper_log PERF fps={fps:.1} p95={p95:.1}ms peak={peak:.1}ms last={last:.1}ms frames={}",
                                ms.len()
                            );
                        }
                    }
                });
        }
    }
}

/// 在中央区渲染全部日志面板（VSCode 风格拆分，spec §7.7.7）。
///
/// 各面板独立持有选中行 / 跳转 / 折行 / 视图模式；点击面板的标题条或正文行即激活该面板，
/// 跳转类操作只作用于活动面板。最多 [`MAX_PANES`] 个，按 [`PaneLayout`] 均分或 2×2 网格。
///
/// 关闭面板请求在本函数内就地处理（布局数量减一、活动权交给前一个），并立即停止本帧剩余渲染
/// （避免关闭后面板下标失效导致越界）。
fn render_log_area(ui: &mut egui::Ui, state: &mut AppState) {
    state.ensure_panes();
    let n = state.pane_layout.count.clamp(1, MAX_PANES);
    let dir = state.pane_layout.dir;
    let ratio = state.pane_layout.ratio;
    let closed = match n {
        1 => render_one_in_rect(ui, ui.available_rect_before_wrap(), state, 0),
        2 => split_panes(ui, state, &[0, 1], dir),
        3 => {
            let rect = ui.available_rect_before_wrap();
            let (r0, r1) = split_at_ratio(rect, dir, ratio);
            if let Some(id) = render_one_in_rect(ui, r0, state, 0) {
                Some(id)
            } else {
                let (new_ratio, stopped) = split_drag_handle(ui, r0, r1, dir);
                if let Some(r) = new_ratio {
                    state.pane_layout.ratio = r;
                }
                if stopped {
                    state.save_prefs();
                }
                let mut child = ui.new_child(egui::UiBuilder {
                    max_rect: Some(r1),
                    layout: Some(*ui.layout()),
                    ..Default::default()
                });
                split_panes(&mut child, state, &[1, 2], dir.perpendicular())
            }
        }
        4 => {
            let rect = ui.available_rect_before_wrap();
            let (r0, r1) = split_at_ratio(rect, dir, ratio);
            let mut c0 = ui.new_child(egui::UiBuilder {
                max_rect: Some(r0),
                layout: Some(*ui.layout()),
                ..Default::default()
            });
            if let Some(id) = split_panes(&mut c0, state, &[0, 1], dir.perpendicular()) {
                Some(id)
            } else {
                let (new_ratio, stopped) = split_drag_handle(ui, r0, r1, dir);
                if let Some(r) = new_ratio {
                    state.pane_layout.ratio = r;
                }
                if stopped {
                    state.save_prefs();
                }
                let mut c1 = ui.new_child(egui::UiBuilder {
                    max_rect: Some(r1),
                    layout: Some(*ui.layout()),
                    ..Default::default()
                });
                split_panes(&mut c1, state, &[2, 3], dir.perpendicular())
            }
        }
        _ => None,
    };
    // 关闭面板：布局数量减一，活动权交给前一个（close_pane 内部处理）。
    if let Some(id) = closed {
        state.close_pane(id);
    }
    // 渲染后统一把活动面板的视图态同步回顶层镜像（工具栏 / 状态栏 / 快捷键读取）。
    state.sync_active_pane_mirror();
}

/// 把一个方向下的若干面板均分到当前 `ui` 的可用矩形（仅处理 1 或 2 个；更多由调用方递归）。
fn split_panes(
    ui: &mut egui::Ui,
    state: &mut AppState,
    ids: &[usize],
    dir: SplitDir,
) -> Option<usize> {
    match ids.len() {
        1 => render_one_pane(ui, state, ids[0]),
        2 => {
            let rect = ui.available_rect_before_wrap();
            let ratio = state.pane_layout.ratio;
            let (r0, r1) = split_at_ratio(rect, dir, ratio);
            if let Some(id) = render_one_in_rect(ui, r0, state, ids[0]) {
                return Some(id);
            }
            // 分隔条：可拖拽，拖动时按指针位置改写分割比例（作用于所有分割点）。
            let (new_ratio, stopped) = split_drag_handle(ui, r0, r1, dir);
            if let Some(r) = new_ratio {
                state.pane_layout.ratio = r;
            }
            if stopped {
                state.save_prefs();
            }
            render_one_in_rect(ui, r1, state, ids[1])
        }
        _ => {
            for &id in ids {
                if let Some(closed) = render_one_pane(ui, state, id) {
                    return Some(closed);
                }
            }
            None
        }
    }
}

/// 在左右/上下两块之间绘制一个可拖拽的分隔条。
///
/// 返回 `(new_ratio, stopped)`：`new_ratio` 为本次帧拖动后的新比例（`Some` 表示拖动中），
/// `stopped` 表示本次帧结束拖动（调用方据此持久化）。分隔条宽度 `SEP_W`，两侧各留响应热区，
/// 拖动时指针位置映射回比例并 clamp。
fn split_drag_handle(
    ui: &mut egui::Ui,
    r0: egui::Rect,
    r1: egui::Rect,
    dir: SplitDir,
) -> (Option<f32>, bool) {
    const SEP_W: f32 = 5.0;
    let p = theme::palette(ui.ctx());
    let sep_rect = match dir {
        SplitDir::Horizontal => egui::Rect::from_min_max(
            egui::pos2(r0.right(), r0.top()),
            egui::pos2(r1.left(), r0.bottom()),
        ),
        SplitDir::Vertical => egui::Rect::from_min_max(
            egui::pos2(r0.left(), r0.bottom()),
            egui::pos2(r0.right(), r1.top()),
        ),
    };
    // 热区比可视条略宽，便于抓取。
    let hot = sep_rect.expand2(if dir == SplitDir::Horizontal {
        egui::vec2(SEP_W, 0.0)
    } else {
        egui::vec2(0.0, SEP_W)
    });
    let resp = ui.interact(hot, ui.id().with("split_drag"), egui::Sense::drag());

    let total = match dir {
        SplitDir::Horizontal => r0.width() + r1.width(),
        SplitDir::Vertical => r0.height() + r1.height(),
    };
    // 拖动中 / 结束拖动：指针位置相对总矩形映射回比例。
    let mut new_ratio = None;
    if (resp.dragged() || resp.drag_stopped())
        && total > 0.0
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let frac = match dir {
            SplitDir::Horizontal => (pos.x - r0.left()) / total,
            SplitDir::Vertical => (pos.y - r0.top()) / total,
        };
        new_ratio = Some(frac.clamp(MIN_RATIO, MAX_RATIO));
    }

    // 视觉：悬停/拖动时高亮分隔条，否则画细线。
    let color = if resp.hovered() || resp.dragged() {
        p.accent
    } else {
        p.border
    };
    ui.painter().rect_filled(sep_rect, 0.0, color);
    // 拖动时更新光标，提示可拖。
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(if dir == SplitDir::Horizontal {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
    }
    (new_ratio, resp.drag_stopped())
}

/// 按方向和比例把矩形切成两块（返回左/右或上/下两部分）。
///
/// `ratio` 为第一块占总尺寸的比例，clamp 到 [`MIN_RATIO`]..=[`MAX_RATIO`]。
fn split_at_ratio(rect: egui::Rect, dir: SplitDir, ratio: f32) -> (egui::Rect, egui::Rect) {
    let ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
    match dir {
        SplitDir::Horizontal => rect.split_left_right_at_x(rect.left() + rect.width() * ratio),
        SplitDir::Vertical => rect.split_top_bottom_at_y(rect.top() + rect.height() * ratio),
    }
}

/// 在给定矩形内渲染单个面板（标题条 + 正文）。
fn render_one_in_rect(
    parent: &mut egui::Ui,
    rect: egui::Rect,
    state: &mut AppState,
    pane_id: usize,
) -> Option<usize> {
    let mut child = parent.new_child(egui::UiBuilder {
        max_rect: Some(rect),
        layout: Some(*parent.layout()),
        ..Default::default()
    });
    render_one_pane(&mut child, state, pane_id)
}

/// 标题条图标按钮：扁平无边框（VS Code action icon 观感），默认仅显示图标本体，
/// hover 时才铺一层浅色圆角背景，按下态用更深一档的 `control_hover`。
///
/// `paint` 闭包负责绘制图标本体，收到居中区域 `rect` 与最终前景色 `color`（已按
/// 启用/hover/按下态算好），这样字符图标与自绘矢量图标共用同一套状态与命中区域。
fn titlebar_icon(
    ui: &mut egui::Ui,
    tip: &str,
    enabled: bool,
    paint: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> egui::Response {
    let p = theme::palette(ui.ctx());
    // 18×18 命中区：与编辑器标题条图标尺寸一致，且不至于误触相邻图标。
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    if enabled {
        if resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, 3.0, p.control_hover);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, p.row_hover);
        }
    }
    let color = if !enabled {
        p.text_dim.gamma_multiply(0.45)
    } else if resp.hovered() || resp.is_pointer_button_down_on() {
        p.text_strong
    } else {
        p.text_dim
    };
    paint(ui.painter(), rect, color);
    resp = resp.on_hover_text(tip);
    resp
}

/// 标题条「拆分」图标：VS Code 的 split 图标是「一个矩形被一条竖线左右平分」，
/// 这里用 `Painter` 自绘（外框 + 中线），避免依赖字体对几何符号的支持。
fn titlebar_split_icon(ui: &mut egui::Ui, tip: &str, enabled: bool) -> egui::Response {
    titlebar_icon(ui, tip, enabled, |painter, rect, color| {
        let r = rect.shrink(4.0);
        let stroke = egui::Stroke::new(1.2, color);
        painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
        let cx = r.center().x;
        painter.line_segment(
            [egui::pos2(cx, r.top()), egui::pos2(cx, r.bottom())],
            stroke,
        );
    })
}

/// 标题条字符图标：单字符居中绘制（关闭 `×`、单独打开 `＋`）。
fn titlebar_char_icon(ui: &mut egui::Ui, ch: &str, tip: &str, enabled: bool) -> egui::Response {
    titlebar_icon(ui, tip, enabled, |painter, rect, color| {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            ch,
            egui::FontId::proportional(13.0),
            color,
        );
    })
}

/// 标题条「返回共享」图标：自绘左箭头。不用 U+21A9（`↩`，Arrows 区）——
/// 自绘矢量与拆分图标风格统一且确定性渲染（虽有 Nerd Font 兜底，仍保持自绘以稳字宽）。
fn titlebar_back_icon(ui: &mut egui::Ui, tip: &str, enabled: bool) -> egui::Response {
    titlebar_icon(ui, tip, enabled, |painter, rect, color| {
        let r = rect.shrink(4.0);
        let stroke = egui::Stroke::new(1.2, color);
        let mid_y = r.center().y;
        // 主干：从右端指向左端的水平线。
        painter.line_segment(
            [egui::pos2(r.right(), mid_y), egui::pos2(r.left(), mid_y)],
            stroke,
        );
        // 箭头：左上 + 左下两条斜线。
        painter.line_segment(
            [
                egui::pos2(r.left(), mid_y),
                egui::pos2(r.left() + 3.0, mid_y - 3.0),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(r.left(), mid_y),
                egui::pos2(r.left() + 3.0, mid_y + 3.0),
            ],
            stroke,
        );
    })
}

/// 渲染单个日志面板：标题条（文件名 + 关闭按钮 + 激活态高亮） + 正文。
///
/// 返回 `Some(pane_id)` 表示用户点了标题条的关闭按钮，由上层关闭面板。
fn render_one_pane(ui: &mut egui::Ui, state: &mut AppState, pane_id: usize) -> Option<usize> {
    let is_active = state.active_pane == pane_id;
    let p = theme::palette(ui.ctx());

    // —— 标题条：点击激活、显示文件名 + 右侧图标（本面板打开/返回共享、拆分、关闭）——
    //
    // **绘制顺序（关键）**：egui 的 `Painter` 把 shape 按调用顺序压进同一图层，
    // **后压的盖住先压的**。若像早期版本那样「先在 `horizontal` 内画文件名/图标 →
    // 再在 `horizontal` 之后铺标题条背景」，背景会把标题条内容整个盖掉，表现为
    // 「标题条一片空白、图标完全不可见」（用户反馈「没实现」的根因）。
    // 因此这里先用 `allocate_ui_with_layout` 定下固定高度的标题条矩形，
    // **先铺背景**，再在其上画内容。
    //
    // 另注：不能用 `Frame::show` 包标题条——`Frame::begin` 会把整个面板高度作为
    // content_ui 的 max_rect，垂直居中的 `selectable_label` 把 min_rect 撑满整高，
    // `Frame::end` 把父 ui cursor 推到面板底部 → 正文高度归零（M18 的坑）。
    let mut closed = false;
    let mut close_file = false;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), TITLE_BAR_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // ① 背景：活动面板用高亮色，非活动用面板底色。**必须在内容之前绘制**。
            ui.painter().rect_filled(
                ui.max_rect(),
                0.0,
                if is_active { p.row_active } else { p.panel },
            );
            // ② 内容：收紧 spacing 让标题条保持纤薄（不影响正文）。
            // 全局 spacing 里按钮最小高 22px、上下 padding 3px，而 `ui.horizontal` 的行高
            // 直接取 `interact_size.y`（22px），加上 padding 让标题条高达 26px；
            // 用 `scope` 隔离收紧三档，贴近编辑器组观感。
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 2.0);
                ui.spacing_mut().interact_size.y = 16.0;
                ui.spacing_mut().button_padding.y = 0.0;
                ui.horizontal(|ui| {
                    let title = pane_title(state, pane_id);
                    // 点击标题条（非按钮区）激活该面板。
                    let label = ui.selectable_label(is_active, title);
                    if label.clicked() && !is_active {
                        state.active_pane = pane_id;
                        state.sync_active_pane_mirror();
                    }
                    // 标题条右侧图标区（right_to_left：最右为关闭，往左依次拆分、单独打开/返回共享）。
                    // 全部图标化、扁平无边框，对齐 VS Code 编辑器组的 action icon 观感。
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 关闭按钮始终显示：多面板=关闭该面板，单面板=关闭当前文件（回到空面板）。
                        if titlebar_char_icon(
                            ui,
                            "×",
                            if state.pane_layout.count > 1 {
                                "关闭面板"
                            } else {
                                "关闭文件"
                            },
                            true,
                        )
                        .clicked()
                        {
                            if state.pane_layout.count > 1 {
                                closed = true;
                            } else {
                                close_file = true;
                            }
                        }

                        // 拆分：向右拆分出一个新面板（VS Code 默认 split 方向），达到上限则禁用。
                        let can_split = state.pane_layout.count < MAX_PANES;
                        if titlebar_split_icon(
                            ui,
                            if can_split {
                                "向右拆分出新面板"
                            } else {
                                "已达到面板数量上限"
                            },
                            can_split,
                        )
                        .clicked()
                        {
                            state.split_pane(SplitDir::Horizontal);
                            state.save_prefs();
                        }

                        // 该面板已单独打开文件 → 提供「返回共享」；否则提供「本面板打开」单独载入文件。
                        if state.panes[pane_id].fileset_override.is_some() {
                            if titlebar_back_icon(ui, "改回共享全局文件集", true).clicked()
                            {
                                state.panes[pane_id].fileset_override = None;
                                state.panes[pane_id].selected_row = None;
                                state.panes[pane_id].scroll_target = None;
                                state.panes[pane_id].max_line_width = 0.0; // 惰性重算
                            }
                        } else if titlebar_char_icon(
                            ui,
                            "＋",
                            "在本面板单独打开一个日志文件（与全局文件集脱钩）",
                            true,
                        )
                        .clicked()
                        {
                            state.pending_open_in_pane = Some(pane_id);
                        }
                    });
                });
            });
        },
    );

    // —— 正文：编辑器风格日志区 ——
    let body_rect = ui.available_rect_before_wrap();
    let mut body = ui.new_child(egui::UiBuilder {
        max_rect: Some(body_rect),
        layout: Some(*ui.layout()),
        ..Default::default()
    });
    // 活动面板描边高亮，非活动用细边框区分（VSCode 编辑器组选中态）。
    body.painter().rect_stroke(
        body_rect,
        0.0,
        if is_active {
            egui::Stroke::new(2.0, p.accent)
        } else {
            egui::Stroke::new(1.0, p.border)
        },
        egui::StrokeKind::Inside,
    );
    {
        let before = state.panes[pane_id].selected_row;
        let mut pane = state.panes[pane_id].clone();
        let search_word = crate::ui::log_view::show(&mut body, &*state, &mut pane, pane_id);
        // 点击正文行会更新该面板的 selected_row：若发生变化，把此面板设为活动面板，
        // 使跳转 / 复制 / 折行等后续操作作用于它（spec §7.7.7）。
        let row_clicked = pane.selected_row != before;
        state.panes[pane_id] = pane;
        if row_clicked && !is_active {
            state.active_pane = pane_id;
            state.sync_active_pane_mirror();
        }
        // 右键「查找选中词」：把词带入检索框并聚焦（不自动跑检索，让用户确认/改词后回车）。
        if let Some(word) = search_word {
            state.search_pattern = word;
            state.focus_search = true;
        }
    }

    // 单面板点 ✕：关闭当前文件，回到空面板（本帧直接生效，返回 None 不关面板）。
    if close_file {
        state.close_current_file();
    }

    if closed { Some(pane_id) } else { None }
}

/// 面板的标题文字：共享文件集显示当前高亮文件名（或文件数），单独打开的文件集显示其文件名。
fn pane_title(state: &AppState, pane_id: usize) -> String {
    let pane = &state.panes[pane_id];
    if let Some(fs) = &pane.fileset_override {
        let n = fs.file_count();
        if n == 1 {
            fs.file(0)
                .and_then(|f| f.path.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "单独文件".to_owned())
        } else {
            format!("{n} 个文件（单独）")
        }
    } else {
        let n = state.fileset.file_count();
        if n == 0 {
            "未打开文件".to_owned()
        } else if let Some(idx) = state.sidebar_active_file {
            state
                .fileset
                .file(idx)
                .and_then(|f| f.path.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| format!("{n} 个文件"))
        } else {
            format!("{n} 个文件")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个已就绪的 `AppState`：`panes` 覆盖 `pane_layout.count` 且 `active_pane` 指向 0。
    fn ready_state() -> AppState {
        let mut s = AppState::default();
        s.ensure_panes();
        s
    }

    // —— SplitDir / PaneLayout ——

    #[test]
    fn split_dir_perpendicular_swaps_axis() {
        assert_eq!(SplitDir::Horizontal.perpendicular(), SplitDir::Vertical);
        assert_eq!(SplitDir::Vertical.perpendicular(), SplitDir::Horizontal);
    }

    #[test]
    fn pane_layout_default_is_single_horizontal() {
        let l = PaneLayout::default();
        assert_eq!(l.count, 1);
        assert_eq!(l.dir, SplitDir::Horizontal);
    }

    #[test]
    fn split_from_single_sets_direction_then_increments() {
        let mut l = PaneLayout::default();
        l.split(SplitDir::Vertical);
        assert_eq!(l.count, 2);
        assert_eq!(l.dir, SplitDir::Vertical); // 首个拆分决定方向
        l.split(SplitDir::Horizontal);
        assert_eq!(l.count, 3);
        assert_eq!(l.dir, SplitDir::Vertical); // 方向不再被后续拆分覆盖
    }

    #[test]
    fn split_caps_at_max_panes() {
        let mut l = PaneLayout::default();
        for _ in 0..10 {
            l.split(SplitDir::Horizontal);
        }
        assert_eq!(l.count, MAX_PANES);
        assert_eq!(l.count, 4);
    }

    #[test]
    fn close_refuses_single_and_out_of_range() {
        let mut l = PaneLayout::default();
        assert!(!l.close(0)); // 单面板不可关
        assert_eq!(l.count, 1);

        l.split(SplitDir::Horizontal);
        assert!(!l.close(99)); // 越界
        assert_eq!(l.count, 2);
    }

    #[test]
    fn close_decrements_count_and_reports_success() {
        let mut l = PaneLayout::default();
        l.split(SplitDir::Horizontal);
        l.split(SplitDir::Horizontal);
        assert_eq!(l.count, 3);
        assert!(l.close(1));
        assert_eq!(l.count, 2);
    }

    // —— AppState 拆分/关闭/激活 ——

    #[test]
    fn ensure_panes_creates_panes_up_to_layout_count() {
        let mut s = AppState::default();
        assert!(s.panes.is_empty());

        s.ensure_panes();
        assert_eq!(s.panes.len(), 1); // 默认 count=1

        s.pane_layout.count = 3;
        s.ensure_panes();
        assert_eq!(s.panes.len(), 3);
    }

    #[test]
    fn ensure_panes_clamps_active_pane_when_out_of_range() {
        let mut s = AppState::default();
        s.pane_layout.count = 2;
        s.ensure_panes();
        s.active_pane = 99; // 人为越界
        s.ensure_panes();
        assert_eq!(s.active_pane, s.panes.len() - 1);
    }

    #[test]
    fn split_pane_activates_new_pane_and_syncs_mirror() {
        let mut s = ready_state();
        s.panes[0].wrap = true;

        s.split_pane(SplitDir::Horizontal);
        assert_eq!(s.pane_layout.count, 2);
        assert_eq!(s.panes.len(), 2);
        assert_eq!(s.active_pane, 1); // 新面板成为活动面板
        assert!(!s.wrap); // 镜像已同步到新面板（默认 false）
    }

    #[test]
    fn close_pane_removes_and_reassigns_activity() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        s.split_pane(SplitDir::Horizontal);
        assert_eq!(s.panes.len(), 3);

        // 关闭非活动面板（0）：活动面板下标前移修正。
        s.active_pane = 2;
        s.close_pane(0);
        assert_eq!(s.panes.len(), 2);
        assert_eq!(s.active_pane, 1);

        // 关闭当前活动面板：活动权交给前一个（clamp 到最后一个）。
        s.close_pane(1);
        assert_eq!(s.panes.len(), 1);
        assert_eq!(s.active_pane, 0);
    }

    #[test]
    fn close_pane_never_closes_last_pane() {
        let mut s = ready_state();
        s.close_pane(0);
        assert_eq!(s.panes.len(), 1); // 仍保留一个
        assert_eq!(s.pane_layout.count, 1);
    }

    #[test]
    fn close_current_file_clears_fileset_and_view_state() {
        let mut s = ready_state();
        let p = tmp_log("close_current.log");
        let idx = crate::core::indexer::LogFileIndex::open(&p).unwrap();
        s.fileset.push(std::sync::Arc::new(idx));
        assert_eq!(s.fileset.file_count(), 1);

        // 制造脏状态：选中行 / 侧边栏高亮 / 横向宽度 / 命中视图 / 状态文本。
        s.selected_row = Some(0);
        s.sidebar_active_file = Some(0);
        s.max_line_width = 123.0;
        s.in_result_mode = true;
        s.status_text = "已加载".to_owned();

        s.close_current_file();

        assert_eq!(s.fileset.file_count(), 0);
        assert_eq!(s.selected_row, None);
        assert_eq!(s.sidebar_active_file, None);
        assert_eq!(s.max_line_width, 0.0);
        assert!(!s.in_result_mode);
        assert_eq!(s.status_text, "未打开文件");
    }

    #[test]
    fn close_current_file_drops_pane_override() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        let p = tmp_log("override.log");
        s.open_paths_to_active_pane(vec![p.clone()]);
        assert!(s.panes[1].fileset_override.is_some());

        s.close_current_file();
        assert!(s.panes[1].fileset_override.is_none());
        assert_eq!(s.fileset.file_count(), 0);
    }

    // —— 命中导航（goto_hit）与字号缩放 ——

    /// 给 `search_results` 填入 N 个命中（file_idx 全部 0，避免依赖 fileset）。
    fn seed_hits(s: &mut AppState, n: u32) {
        s.search_results = (0..n)
            .map(|i| SearchHit {
                file_idx: 0,
                line_idx: i,
            })
            .collect();
    }

    #[test]
    fn goto_hit_wraps_forward_and_backward() {
        let mut s = ready_state();
        seed_hits(&mut s, 3);

        // 无游标 → Next 从 0 开始
        assert!(s.goto_hit(HitDir::Next));
        assert_eq!(s.hit_cursor, Some(0));
        assert!(s.goto_hit(HitDir::Next));
        assert_eq!(s.hit_cursor, Some(1));
        assert!(s.goto_hit(HitDir::Next));
        assert_eq!(s.hit_cursor, Some(2));
        // 末尾再 Next → 环绕回 0
        assert!(s.goto_hit(HitDir::Next));
        assert_eq!(s.hit_cursor, Some(0));

        // 无游标 → Prev 从末尾开始
        s.hit_cursor = None;
        assert!(s.goto_hit(HitDir::Prev));
        assert_eq!(s.hit_cursor, Some(2));
        // 0 再 Prev → 环绕回末尾
        s.hit_cursor = Some(0);
        assert!(s.goto_hit(HitDir::Prev));
        assert_eq!(s.hit_cursor, Some(2));
    }

    #[test]
    fn goto_hit_no_results_returns_false() {
        let mut s = ready_state();
        assert!(!s.goto_hit(HitDir::Next));
        assert_eq!(s.hit_cursor, None);
        assert!(!s.goto_hit(HitDir::Prev));
    }

    #[test]
    fn set_font_size_clamps_and_clears_line_width_cache() {
        let mut s = ready_state();
        s.panes[0].max_line_width = 1234.5;

        // 超上限 → 夹取到 MAX
        s.set_font_size(9999.0);
        assert_eq!(s.font_size, crate::core::prefs::MAX_FONT_SIZE);
        assert_eq!(s.prefs.font_size, crate::core::prefs::MAX_FONT_SIZE);
        assert_eq!(s.panes[0].max_line_width, 0.0); // 清缓存

        // 低于下限 → 夹取到 MIN
        s.set_font_size(0.1);
        assert_eq!(s.font_size, crate::core::prefs::MIN_FONT_SIZE);
    }

    #[test]
    fn reset_font_size_returns_to_default() {
        let mut s = ready_state();
        s.set_font_size(24.0);
        assert_ne!(s.font_size, crate::core::prefs::DEFAULT_FONT_SIZE);
        s.reset_font_size();
        assert_eq!(s.font_size, crate::core::prefs::DEFAULT_FONT_SIZE);
    }

    #[test]
    fn min_font_size_is_still_readable() {
        // 缩放下限曾为 8.0，长按 ⌘- 缩到底后正文几乎不可读；衬线字体下小字号更显瘦弱，
        // 下限随默认字号抬到 12.0。直接断言常量会触发 `clippy::assertions_on_constants`，
        // 故通过行为验证：从默认字号连续缩到不能再缩，最终值即 MIN，断言其仍 ≥ 12.0。
        let mut s = ready_state();
        for _ in 0..100 {
            s.zoom_font(-1.0);
        }
        assert!(s.font_size >= 12.0);
        assert_eq!(s.font_size, crate::core::prefs::MIN_FONT_SIZE);
    }

    #[test]
    fn active_pane_mut_falls_back_to_last_on_overflow() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        assert_eq!(s.panes.len(), 2);

        s.active_pane = usize::MAX; // 越界
        s.active_pane_mut().wrap = true;
        assert!(s.panes[1].wrap); // 回退到最后一个面板（min(len-1)）
        assert!(!s.panes[0].wrap);
    }

    #[test]
    fn active_pane_returns_none_when_empty() {
        let s = AppState::default();
        assert!(s.active_pane().is_none());
    }

    // —— 镜像同步 ——

    #[test]
    fn sync_active_pane_mirror_copies_view_state_to_top_level() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        s.panes[1].wrap = true;
        s.panes[1].selected_row = Some(42);
        s.panes[1].max_line_width = 128.0;
        s.panes[1].in_result_mode = true;

        s.active_pane = 1;
        s.sync_active_pane_mirror();

        assert!(s.wrap);
        assert_eq!(s.selected_row, Some(42));
        assert_eq!(s.max_line_width, 128.0);
        assert!(s.in_result_mode);
    }

    #[test]
    fn sync_active_pane_mirror_clears_when_no_panes() {
        let mut s = AppState {
            wrap: true,
            selected_row: Some(7),
            max_line_width: 99.0,
            in_result_mode: true,
            ..Default::default()
        };

        s.sync_active_pane_mirror();

        assert!(!s.wrap);
        assert_eq!(s.selected_row, None);
        assert_eq!(s.max_line_width, 0.0);
        assert!(!s.in_result_mode);
    }

    #[test]
    fn apply_wrap_writes_mirror_back_to_active_pane() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        s.active_pane = 1;
        s.wrap = true;
        s.apply_wrap_to_active_pane();
        assert!(s.panes[1].wrap);
        assert!(!s.panes[0].wrap); // 只影响活动面板
    }

    // —— 跳转与清态 ——

    #[test]
    fn jump_to_row_targets_only_active_pane() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        s.active_pane = 1;

        s.jump_to_row(100);
        assert_eq!(s.panes[1].scroll_target, Some(100));
        assert_eq!(s.panes[0].scroll_target, None);
    }

    #[test]
    fn clear_pane_view_states_resets_every_pane_and_drops_overrides() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        s.split_pane(SplitDir::Horizontal);
        // 给每个面板填上状态（含一个单独文件集）。
        for (i, p) in s.panes.iter_mut().enumerate() {
            p.selected_row = Some(i);
            p.scroll_target = Some(i * 10);
            p.max_line_width = 1.0;
            if i == 1 {
                p.fileset_override = Some(FileSet::new());
            }
        }

        s.clear_pane_view_states();
        for p in &s.panes {
            assert_eq!(p.selected_row, None);
            assert_eq!(p.scroll_target, None);
            assert_eq!(p.max_line_width, 0.0);
            assert!(p.fileset_override.is_none());
        }
    }

    // —— 锚点选区模型（selection_anchor + selected_row 表示闭区间） ——

    #[test]
    fn select_single_is_single_row_and_clears_anchor() {
        let mut p = PaneState::default();
        p.select_single(5);
        assert_eq!(p.selected_row, Some(5));
        assert_eq!(p.selection_anchor, None);
        assert_eq!(p.selected_range(), Some((5, 5)));
        assert!(p.is_row_selected(5));
        assert!(!p.is_row_selected(4));
        assert_eq!(p.selected_count(), 1);
    }

    #[test]
    fn extend_selection_to_builds_closed_range_both_directions() {
        let mut p = PaneState::default();
        p.select_single(2);
        p.extend_selection_to(5);
        assert_eq!(p.selected_range(), Some((2, 5)));
        assert_eq!(p.selected_count(), 4);
        assert!(p.is_row_selected(2) && p.is_row_selected(3) && p.is_row_selected(5));
        // 反向扩展（向上）同样正确，区间自动归一化
        p.extend_selection_to(0);
        assert_eq!(p.selected_range(), Some((0, 2)));
        assert_eq!(p.selected_count(), 3);
    }

    #[test]
    fn extend_without_prior_selection_stays_single() {
        let mut p = PaneState::default();
        p.extend_selection_to(7);
        assert_eq!(p.selected_row, Some(7));
        // 无既有选中 → 不设立锚点，退化为单行
        assert_eq!(p.selection_anchor, None);
        assert_eq!(p.selected_range(), Some((7, 7)));
    }

    #[test]
    fn select_all_spans_full_range() {
        let mut p = PaneState::default();
        p.select_all(9);
        assert_eq!(p.selected_range(), Some((0, 9)));
        assert_eq!(p.selected_count(), 10);
        assert!(p.is_row_selected(0) && p.is_row_selected(9));
    }

    #[test]
    fn clear_selection_resets_anchor_too() {
        let mut p = PaneState::default();
        p.select_all(9);
        p.clear_selection();
        assert_eq!(p.selected_row, None);
        assert_eq!(p.selection_anchor, None);
        assert_eq!(p.selected_range(), None);
        assert_eq!(p.selected_count(), 0);
    }

    // —— 矩形分割（布局几何） ——

    #[test]
    fn split_at_ratio_horizontal_splits_left_right() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (l, r) = split_at_ratio(rect, SplitDir::Horizontal, 0.5);
        assert_eq!(l.width(), 50.0);
        assert_eq!(r.width(), 50.0);
        assert_eq!(l.height(), 50.0);
        assert_eq!(r.height(), 50.0);
        assert_eq!(l.right(), r.left()); // 无缝拼接
    }

    #[test]
    fn split_at_ratio_vertical_splits_top_bottom() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (t, b) = split_at_ratio(rect, SplitDir::Vertical, 0.5);
        assert_eq!(t.height(), 25.0);
        assert_eq!(b.height(), 25.0);
        assert_eq!(t.width(), 100.0);
        assert_eq!(b.width(), 100.0);
        assert_eq!(t.bottom(), b.top()); // 无缝拼接
    }

    #[test]
    fn split_at_ratio_uses_given_fraction() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (l, r) = split_at_ratio(rect, SplitDir::Horizontal, 0.3);
        assert!((l.width() - 30.0).abs() < 0.01);
        assert!((r.width() - 70.0).abs() < 0.01);
    }

    #[test]
    fn split_at_ratio_clamps_to_sane_range() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        // 0.0 → clamp 到 MIN_RATIO(0.15)
        let (l, _) = split_at_ratio(rect, SplitDir::Horizontal, 0.0);
        assert!((l.width() - 15.0).abs() < 0.01);
        // 1.0 → clamp 到 MAX_RATIO(0.85)
        let (l2, _) = split_at_ratio(rect, SplitDir::Horizontal, 1.0);
        assert!((l2.width() - 85.0).abs() < 0.01);
    }

    // —— 打开路径的分流（open_paths） ——

    /// 测试辅助：在临时目录下建一个非空日志文件，返回**唯一**路径。
    ///
    /// 文件名带全局递增序号：并行 `#[test]` 会并发调用本函数，若都用固定名（如 `a.log`），
    /// 一个线程 `std::fs::write` 截断重写时，另一个线程正 `LogFileIndex::open` 对其 mmap，
    /// 会撞上空窗读到 0 字节 → `IndexError::Empty` → `loaded==0` 提前 return（flaky）。
    fn tmp_log(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join("hyper-log-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{seq}_{name}"));
        std::fs::write(&p, format!("line one\nline two\n{name}\n")).unwrap();
        p
    }

    #[test]
    fn open_paths_to_active_pane_loads_into_active_pane_override() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        assert_eq!(s.panes.len(), 2);
        assert_eq!(s.active_pane, 1); // 拆分后新面板即活动面板

        let p = tmp_log("a.log");
        s.open_paths_to_active_pane(vec![p.clone()]);
        // 全局文件集保持不变，活动面板(1)拿到 override。
        assert_eq!(s.fileset.file_count(), 0);
        assert!(s.panes[0].fileset_override.is_none());
        let ov = s.panes[1].fileset_override.as_ref().unwrap();
        assert_eq!(ov.file_count(), 1);
        assert_eq!(ov.file(0).unwrap().path, p);
    }

    #[test]
    fn open_paths_to_active_pane_twice_gives_two_panes_different_files() {
        let mut s = ready_state();
        s.split_pane(SplitDir::Horizontal);
        assert_eq!(s.active_pane, 1);

        let pa = tmp_log("a.log");
        let pb = tmp_log("b.log");

        // 第一次打开 → 活动面板(1)
        s.open_paths_to_active_pane(vec![pa.clone()]);
        assert_eq!(
            s.panes[1]
                .fileset_override
                .as_ref()
                .unwrap()
                .file(0)
                .unwrap()
                .path,
            pa
        );

        // 激活面板 0，再打开 → 面板 0 拿到不同文件
        s.active_pane = 0;
        s.open_paths_to_active_pane(vec![pb.clone()]);
        assert_eq!(
            s.panes[0]
                .fileset_override
                .as_ref()
                .unwrap()
                .file(0)
                .unwrap()
                .path,
            pb
        );

        // 两个面板的文件集互不相同
        let a = s.panes[0].fileset_override.as_ref().unwrap();
        let b = s.panes[1].fileset_override.as_ref().unwrap();
        assert_ne!(a.file(0).unwrap().path, b.file(0).unwrap().path);
    }
}
