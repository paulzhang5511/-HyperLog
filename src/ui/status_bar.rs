use crate::app::AppState;
use crate::util::human_bytes;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(&state.status_text);

            // 导出中显示进度条 + spinner（优先于检索进度，常规使用二者互斥）
            if state.is_exporting {
                let (done, total) = state.export_progress;
                let frac = if total > 0 {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(200.0)
                        .show_percentage(),
                );
                ui.spinner();
            // 检索中显示进度条 + spinner；截断时提示（G6）
            } else if state.is_searching {
                let (done, total) = state.search_progress;
                let frac = if total > 0 {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(200.0)
                        .show_percentage(),
                );
                ui.spinner();
            } else if state.search_truncated {
                ui.colored_label(
                    egui::Color32::from_rgb(0xE0, 0x9F, 0x3E),
                    "结果已截断（>200 万条）",
                );
            }

            // 导出失败内联红字（不 panic）
            if let Some(err) = &state.export_error {
                ui.colored_label(egui::Color32::RED, format!("导出错误：{err}"));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let files = state.fileset.file_count();
                if files == 0 {
                    return;
                }
                let lines = state.fileset.total_lines();
                let bytes = state.fileset.total_bytes();
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
