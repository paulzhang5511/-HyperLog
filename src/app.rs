use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use regex::Regex;

use crate::core::export::{ExportFormat, ExportMessage};
use crate::core::indexer::{FileSet, IndexError, LogFileIndex};
use crate::core::recents::Recents;
use crate::core::search::{
    CancelToken, SearchError, SearchHit, SearchMessage, SearchMode, SearchOptions,
};
use crate::ui::{log_view, status_bar, theme, toolbar};

/// 每帧最多处理 1000 条后台消息，防止消息洪水饿死渲染（spec §8.4，对应 D6）。
const MAX_MSG_PER_FRAME: usize = 1000;

/// 检索命中上限（G6）。
const MAX_HITS: usize = 2_000_000;

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
    pub wrap: bool,
    /// 当前选中的行（全量视图为全局行号，命中视图为命中索引），供高亮与复制使用。
    pub selected_row: Option<usize>,
    /// 估算的最长行渲染宽度（像素），用于固定横向滚动范围，避免滚动条随虚拟滚动抖动。
    pub max_line_width: f32,
    /// toolbar 置位后，在 `ui` 中弹出打开文件对话框。
    pub pending_open: bool,
    /// 最近打开的文件列表（M11 / spec Q3），持久化在平台配置目录。
    pub recents: Recents,
    /// toolbar 置位后，在 `ui` 中打开该最近文件（避免在同一帧内同时借用 UI 与 self）。
    pub pending_open_recent: Option<PathBuf>,

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
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        log::info!("Hyper Log starting up");
        // 编辑器风格主题（默认暗色），再装字体：主题只改颜色，字体与主题互不影响。
        theme::apply(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let show_perf = std::env::var("HYPER_LOG_PERF")
            .map(|v| v == "1")
            .unwrap_or(false);
        let (search_tx, search_rx) = crossbeam_channel::unbounded();
        let (export_tx, export_rx) = crossbeam_channel::unbounded();
        Self {
            state: AppState {
                show_perf,
                recents: Recents::load(),
                ..Default::default()
            },
            search_tx,
            search_rx,
            search_cancel: None,
            export_tx,
            export_rx,
            export_cancel: None,
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
            .pick_files();

        let Some(paths) = picked else {
            self.state.status_text = "已取消打开".to_owned();
            return;
        };

        self.load_paths(paths);
    }

    /// 加载一批路径：文件对话框与「最近文件」共用同一套校验与提示逻辑（M11）。
    ///
    /// 成功加载的路径会记入最近文件列表并立即落盘。沿用「打开」的既有语义：
    /// 新文件是**追加**到当前文件集合，而非替换（MVP 未提供关闭单个文件的能力）。
    fn load_paths(&mut self, paths: Vec<PathBuf>) {
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
            if self.state.fileset.total_bytes() as u64 + bytes > 32 * 1024 * 1024 * 1024 {
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
                    self.state.fileset.push(Arc::new(idx));
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
        }

        // 新文件可能比已加载的更宽，重新估算横向滚动范围；行号语义也随之变化，清掉选中行。
        let max_line_width = log_view::estimate_content_width(&self.state.fileset);
        self.state.max_line_width = max_line_width;
        self.state.selected_row = None;

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
        self.state.in_result_mode = true;
        self.state.hit_regex = hit_re;
        self.state.status_text = "检索中…".to_owned();

        let tx = self.search_tx.clone();
        std::thread::spawn(move || {
            crate::core::search::run_search(&files, &pattern, &options, &cancel, &tx);
        });
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
        // 仅后台有活动时才请求重绘，空闲时不烧 CPU（spec §8.4）。
        if self.state.is_searching || self.state.is_exporting {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
        status_bar::show(ui, &self.state);
        // 日志区用编辑器正文底色（与顶栏/底栏区分），且不留窗口内边距：
        // 行号槽要从最左侧开始，否则整块行背景会与正文错位。
        let bg = theme::palette(ui.ctx()).bg;
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(bg))
            .show(ui, |ui| {
                log_view::show(ui, &mut self.state);
            });

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
                });
        }
    }
}
