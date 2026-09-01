use crate::core::indexer::{FileSet, IndexError, LogFileIndex};
use crate::ui::{log_view, status_bar, toolbar};

/// 多文件累计加载上限：单文件 16 GiB（索引器内已限制）+ 累计 32 GiB（spec §13 Q9），
/// 防止多个大文件叠加撑爆地址空间。
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

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
}

pub struct LogViewerApp {
    state: AppState,
}

impl LogViewerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        log::info!("Hyper Log starting up");
        Self {
            state: AppState::default(),
        }
    }

    /// 通过系统原生对话框选择并加载日志文件。
    ///
    /// 单文件 > 16 GiB 由 `LogFileIndex::open` 拒绝；累计 > 32 GiB 在此处拒绝。
    /// 空文件与索引错误会跳过并在状态栏提示，不会中断其余文件加载。
    pub fn open_files(&mut self) {
        // 阻塞式原生对话框：弹窗期间 UI 线程挂起，属正常行为。
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
            if self.state.fileset.total_bytes() as u64 + bytes > MAX_TOTAL_BYTES {
                skipped += 1;
                errors.push(format!(
                    "{}: 累计超过 {} 上限，已跳过",
                    path.display(),
                    crate::util::human_bytes(MAX_TOTAL_BYTES)
                ));
                continue;
            }

            match LogFileIndex::open(&path) {
                Ok(idx) => {
                    self.state.fileset.push(std::sync::Arc::new(idx));
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
}

impl eframe::App for LogViewerApp {
    /// 在每个 [`Self::ui`] 之前调用一次；窗口隐藏时也会被 eframe 调用。
    /// 因此后台检索/导出的消息轮询放这里——不绘制任何 UI，隐藏时任务照样推进。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 后台有任务时才请求重绘，空闲时不烧 CPU（spec.md §8.4）。
        let _ = ctx;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.state.pending_open {
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
