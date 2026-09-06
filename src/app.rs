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

/// 面板布局：`count` 个面板按 `dir` 均分；`count == 4` 时渲染为 2×2 网格。
#[derive(Clone, Debug)]
pub struct PaneLayout {
    /// 拆分方向（`count == 4` 时用作外层方向）。
    pub dir: SplitDir,
    /// 面板数量，恒在 `1..=MAX_PANES`。
    pub count: usize,
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self {
            dir: SplitDir::Horizontal,
            count: 1,
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
    /// 待跳转行号，由该面板自己消费（替代原先的全局 `AppState::scroll_target`）。
    pub scroll_target: Option<usize>,
    /// 折行开关（每面板独立）。
    pub wrap: bool,
    /// 估算的最长行渲染宽度，用于固定横向滚动范围。
    pub max_line_width: f32,
    /// 是否显示检索命中视图（可做到「A 面板全量 + B 面板命中」的对比）。
    pub in_result_mode: bool,
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
    /// 持久化偏好（主题/窗口几何/折行/侧栏/最近检索词）；实时开关 `wrap`/`show_sidebar`
    /// 在保存时同步进本结构后写盘，加载时回填这两个实时字段。
    pub prefs: Prefs,
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
        // 拆分布局：数量与方向一并持久化（`split_vertical` 用 bool 表达，避免 core 依赖 egui）。
        p.split_count = self.pane_layout.count;
        p.split_vertical = self.pane_layout.dir == SplitDir::Vertical;
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

    /// 清空所有面板的视图态（文档被替换/重载时调用：旧的行号坐标已失效）。
    pub fn clear_pane_view_states(&mut self) {
        for p in &mut self.panes {
            p.selected_row = None;
            p.scroll_target = None;
            p.max_line_width = 0.0;
            p.fileset_override = None;
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
const FONT_CJK: &str = "MiSans";

/// 配置中文字体。
///
/// egui 内置字体（Hack / Ubuntu-Light / NotoEmoji）**均不含 CJK 字形**，
/// 未额外配置时界面与日志中的中文会渲染成空白方块（豆腐块）。
///
/// 这里把 MiSans 作为**兜底字体追加**到 Proportional / Monospace 两个字体族末尾，
/// 而不是替换主字体：
/// - 拉丁字符仍由内置字体绘制，Monospace 保持等宽，日志列对齐不受影响；
/// - 仅当内置字体缺字形时（中文等 CJK 字符）才回退到 MiSans。
///
/// 字体经 `include_bytes!` 编译进二进制，打包为 `.app` 后无需附带资源目录。
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_CJK.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/MiSans-Normal.ttf"
        ))),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(FONT_CJK.to_owned());
    }
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
        // 面板：布局（方向/数量）从偏好恢复，并确保 panes 覆盖布局所需数量。
        app.state.pane_layout = PaneLayout {
            dir: if app.state.prefs.split_vertical {
                SplitDir::Vertical
            } else {
                SplitDir::Horizontal
            },
            count: app.state.prefs.split_count.clamp(1, MAX_PANES),
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

        if pressed(ctx, cmd, egui::Key::O) {
            self.state.pending_open = true;
        }
        if pressed(ctx, cmd_shift, egui::Key::O) {
            self.state.pending_open_dir = true;
        }
        if pressed(ctx, cmd, egui::Key::F) {
            self.state.focus_search = true;
        }
        if pressed(ctx, cmd, egui::Key::L) {
            self.state.focus_line_jump = true;
        }
        if pressed(ctx, cmd, egui::Key::B) {
            self.state.show_sidebar = !self.state.show_sidebar;
            self.state.save_prefs();
        }
        // ⌘G / ⌘↵：触发检索（等价「查找」按钮）
        if (pressed(ctx, cmd, egui::Key::G) || pressed(ctx, cmd, egui::Key::Enter))
            && !self.state.is_searching
            && !self.state.search_pattern.trim().is_empty()
        {
            self.state.pending_search = true;
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

        self.load_paths(paths);
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
        self.load_paths(files);
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
        // 视图态是每面板独立持有的，必须遍历清空（含各面板单独打开的文件集）。
        self.state.clear_pane_view_states();
        self.state.selected_row = None;
        self.state.sidebar_active_file = None;
        self.state.dirty_files.clear();

        // 新文件可能比已加载的更宽，重新估算横向滚动范围（活动面板）。
        let max_line_width = log_view::estimate_content_width(&self.state.fileset);
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
    let closed = match n {
        1 => render_one_in_rect(ui, ui.available_rect_before_wrap(), state, 0),
        2 => split_panes(ui, state, &[0, 1], dir),
        3 => {
            let rect = ui.available_rect_before_wrap();
            let (r0, r1) = halve(rect, dir);
            if let Some(id) = render_one_in_rect(ui, r0, state, 0) {
                Some(id)
            } else {
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
            let (r0, r1) = halve(rect, dir);
            let mut c0 = ui.new_child(egui::UiBuilder {
                max_rect: Some(r0),
                layout: Some(*ui.layout()),
                ..Default::default()
            });
            if let Some(id) = split_panes(&mut c0, state, &[0, 1], dir.perpendicular()) {
                Some(id)
            } else {
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
            let (r0, r1) = halve(rect, dir);
            if let Some(id) = render_one_in_rect(ui, r0, state, ids[0]) {
                return Some(id);
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

/// 按方向把矩形对半切（返回左/右或上/下两部分）。
fn halve(rect: egui::Rect, dir: SplitDir) -> (egui::Rect, egui::Rect) {
    match dir {
        SplitDir::Horizontal => rect.split_left_right_at_x(rect.left() + rect.width() / 2.0),
        SplitDir::Vertical => rect.split_top_bottom_at_y(rect.top() + rect.height() / 2.0),
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

/// 渲染单个日志面板：标题条（文件名 + 关闭按钮 + 激活态高亮） + 正文。
///
/// 返回 `Some(pane_id)` 表示用户点了标题条的关闭按钮，由上层关闭面板。
fn render_one_pane(ui: &mut egui::Ui, state: &mut AppState, pane_id: usize) -> Option<usize> {
    let is_active = state.active_pane == pane_id;
    let p = theme::palette(ui.ctx());

    // —— 标题条：点击激活、显示文件、本面板打开 / 返回共享 / 关闭按钮 ——
    //
    // 不能用 `Frame::show`：`Frame::begin` 会把**整个面板高度**作为 content_ui 的 max_rect，
    // 而标题条里垂直居中的 `selectable_label` 会把 min_rect 撑满整个高度，导致 `Frame::end`
    // 把父 ui 的 cursor 推进到面板底部 —— 正文就只剩 0 高度（内容不显示的根因）。
    // 这里用 `horizontal` 手动布局，标题条只占自然高度，正文再取剩余矩形。
    let title_top = ui.cursor().min.y;
    let mut closed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let title = pane_title(state, pane_id);
        // 点击标题条（非按钮区）激活该面板。
        let label = ui.selectable_label(is_active, title);
        if label.clicked() && !is_active {
            state.active_pane = pane_id;
            state.sync_active_pane_mirror();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 该面板已单独打开文件 → 提供「返回共享」；否则提供「本面板打开」单独载入一个文件。
            if state.panes[pane_id].fileset_override.is_some() {
                if ui
                    .small_button("共享")
                    .on_hover_text("改回共享全局文件集")
                    .clicked()
                {
                    state.panes[pane_id].fileset_override = None;
                    state.panes[pane_id].selected_row = None;
                    state.panes[pane_id].scroll_target = None;
                    state.panes[pane_id].max_line_width = 0.0; // 惰性重算
                }
            } else if ui
                .small_button("本面板")
                .on_hover_text("在本面板单独打开一个日志文件（与全局文件集脱钩）")
                .clicked()
            {
                state.pending_open_in_pane = Some(pane_id);
            }
            if state.pane_layout.count > 1 && ui.small_button("✕").clicked() {
                closed = true;
            }
        });
    });
    // 标题条背景：活动面板用高亮色，非活动用面板底色。
    let title_rect = egui::Rect::from_min_max(
        egui::pos2(ui.max_rect().left(), title_top),
        egui::pos2(ui.max_rect().right(), ui.cursor().min.y),
    );
    ui.painter().rect_filled(
        title_rect,
        0.0,
        if is_active { p.row_active } else { p.panel },
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
        crate::ui::log_view::show(&mut body, &*state, &mut pane, pane_id);
        // 点击正文行会更新该面板的 selected_row：若发生变化，把此面板设为活动面板，
        // 使跳转 / 复制 / 折行等后续操作作用于它（spec §7.7.7）。
        let row_clicked = pane.selected_row != before;
        state.panes[pane_id] = pane;
        if row_clicked && !is_active {
            state.active_pane = pane_id;
            state.sync_active_pane_mirror();
        }
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

    // —— 矩形二等分（布局几何） ——

    #[test]
    fn halve_horizontal_splits_left_right_at_midpoint() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (l, r) = halve(rect, SplitDir::Horizontal);
        assert_eq!(l.width(), 50.0);
        assert_eq!(r.width(), 50.0);
        assert_eq!(l.height(), 50.0);
        assert_eq!(r.height(), 50.0);
        assert_eq!(l.right(), r.left()); // 无缝拼接
    }

    #[test]
    fn halve_vertical_splits_top_bottom_at_midpoint() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let (t, b) = halve(rect, SplitDir::Vertical);
        assert_eq!(t.height(), 25.0);
        assert_eq!(b.height(), 25.0);
        assert_eq!(t.width(), 100.0);
        assert_eq!(b.width(), 100.0);
        assert_eq!(t.bottom(), b.top()); // 无缝拼接
    }
}
