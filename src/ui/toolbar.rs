use crate::app::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Hyper Log");

            ui.separator();

            if ui.button("打开").clicked() {
                state.pending_open = true;
            }

            ui.separator();

            // 折行开关：默认关，横向滚动（spec G7）
            let wrap_label = if state.wrap {
                "折行: 开"
            } else {
                "折行: 关"
            };
            ui.toggle_value(&mut state.wrap, wrap_label);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = state.fileset.file_count();
                if count > 0 {
                    ui.label(format!("{} 个文件", count));
                }
            });
        });
    });
}
