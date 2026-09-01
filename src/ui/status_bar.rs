use crate::app::AppState;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(&state.status_text);
        });
    });
}
