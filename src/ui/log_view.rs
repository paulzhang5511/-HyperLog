use eframe::egui;

use crate::app::AppState;

pub fn show(ui: &mut egui::Ui, _state: &AppState) {
    ui.centered_and_justified(|ui| {
        ui.label("尚未打开任何日志文件");
    });
}
