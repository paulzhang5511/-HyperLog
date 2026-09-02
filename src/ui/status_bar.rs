use crate::app::AppState;
use crate::util::{group_digits, human_bytes};

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(&state.status_text);

            // 导出中显示进度条 + spinner（优先于检索进度，常规使用二者互斥）
            if state.is_exporting {
                let (done, total) = state.export_progress;
                ui.add(progress_bar(done, total));
                ui.spinner();
            // 检索中显示进度条 + spinner；截断时提示（G6）
            } else if state.is_searching {
                let (done, total) = state.search_progress;
                ui.add(progress_bar(done as usize, total as usize));
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

            // 右侧：编辑器的状态栏分段——每段一个竖线分隔的弱色文本
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let files = state.fileset.file_count();
                if files == 0 {
                    // 未打开任何文件：至少提示可以怎么用
                    ui.weak("未打开文件");
                    return;
                }

                // 选中行信息（命中视图下索引语义不同，标签随之变化）
                if let Some(row) = state.selected_row {
                    let in_result = state.in_result_mode && !state.search_results.is_empty();
                    if in_result {
                        ui.weak(format!("命中 #{}", group_digits(row + 1)));
                    } else {
                        ui.weak(format!("行 {}", group_digits(row + 1)));
                    }
                    ui.separator();
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

                ui.weak(first_name);
                ui.separator();
                ui.weak(format!("索引 {}", human_bytes(index_mem_total)));
                ui.separator();
                ui.weak(human_bytes(bytes as u64));
                ui.separator();
                ui.weak(format!("{} 行", group_digits(lines)));
                ui.separator();
                ui.weak(format!("{files} 个文件"));
            });
        });
    });
}

/// 统一的进度条：总数为 0 时显示 0%，避免出现 NaN。
fn progress_bar(done: usize, total: usize) -> egui::ProgressBar {
    let frac = if total > 0 {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    egui::ProgressBar::new(frac)
        .desired_width(200.0)
        .show_percentage()
}
