use crate::app::AppState;

pub fn show(ui: &mut egui::Ui, _state: &mut AppState) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Hyper Log");
        });
    });
}
