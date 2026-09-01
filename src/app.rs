use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use regex::Regex;

use crate::core::indexer::{FileSet, IndexError, LogFileIndex};
use crate::core::search::{
    CancelToken, SearchError, SearchHit, SearchMessage, SearchMode, SearchOptions,
};
use crate::ui::{log_view, status_bar, toolbar};

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
    /// toolbar 置位后，在 `ui` 中弹出打开文件对话框。
    pub pending_open: bool,

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
}

pub struct LogViewerApp {
    state: AppState,
    search_tx: Sender<SearchMessage>,
    search_rx: Receiver<SearchMessage>,
    /// 当前检索的取消令牌；完成后置 `None`。
    search_cancel: Option<CancelToken>,
}

impl LogViewerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        log::info!("Hyper Log starting up");
        let (search_tx, search_rx) = crossbeam_channel::unbounded();
        Self {
            state: AppState::default(),
            search_tx,
            search_rx,
            search_cancel: None,
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
        // 仅后台有活动时才请求重绘，空闲时不烧 CPU（spec §8.4）。
        if self.state.is_searching {
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

        toolbar::show(ui, &mut self.state);
        status_bar::show(ui, &self.state);
        egui::CentralPanel::default().show(ui, |ui| {
            log_view::show(ui, &self.state);
        });
    }
}
