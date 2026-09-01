use crate::ui::{log_view, status_bar, toolbar};

/// 应用的全部可变状态。UI 各面板只借用它的引用，不持有状态本身。
#[derive(Default)]
pub struct AppState {
    /// 底部状态栏展示的文本。
    pub status_text: String,
}

pub struct LogViewerApp {
    state: AppState,
}

impl LogViewerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        log::info!("Hyper Log starting up");
        Self {
            state: AppState {
                status_text: "就绪".to_owned(),
            },
        }
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
        toolbar::show(ui, &mut self.state);
        status_bar::show(ui, &self.state);
        egui::CentralPanel::default().show(ui, |ui| {
            log_view::show(ui, &self.state);
        });
    }
}
