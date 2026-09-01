use crate::app::AppState;
use crate::util::human_bytes;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(&state.status_text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let files = state.fileset.file_count();
                if files == 0 {
                    return;
                }
                let lines = state.fileset.total_lines();
                let bytes = state.fileset.total_bytes();
                // 索引内存统计（spec §11 P3：每 8 字节/行）
                let index_mem_total: u64 = (0..files)
                    .filter_map(|i| state.fileset.file(i))
                    .map(|f| f.index_memory_bytes() as u64)
                    .sum();

                let first_name = state
                    .fileset
                    .file(0)
                    .and_then(|f| f.path.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();

                ui.label(format!(
                    "{} · {} 行 · {} · 索引 {} · 首文件: {}",
                    files,
                    lines,
                    human_bytes(bytes as u64),
                    human_bytes(index_mem_total),
                    first_name
                ));
            });
        });
    });
}
