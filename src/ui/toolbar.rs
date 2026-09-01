use crate::app::AppState;
use crate::core::search::SearchMode;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.heading("Hyper Log");

                ui.separator();

                // 检索中禁用「打开」（G1）
                if ui
                    .add_enabled(!state.is_searching, egui::Button::new("打开"))
                    .clicked()
                {
                    state.pending_open = true;
                }

                ui.separator();

                // 检索控件：检索中整体禁用（「停止」按钮保持可用，见下方）
                ui.add_enabled_ui(!state.is_searching, |ui| {
                    // 检索关键字输入框
                    let te = egui::TextEdit::singleline(&mut state.search_pattern)
                        .hint_text("输入关键字或正则")
                        .desired_width(220.0);
                    ui.add(te);

                    // 模式：纯文本 / 正则
                    egui::ComboBox::from_id_salt("search_mode")
                        .selected_text(match state.search_mode {
                            SearchMode::Plain => "纯文本",
                            SearchMode::Regex => "正则",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.search_mode,
                                SearchMode::Plain,
                                "纯文本",
                            );
                            ui.selectable_value(&mut state.search_mode, SearchMode::Regex, "正则");
                        });

                    // 大小写敏感开关
                    ui.toggle_value(&mut state.search_case_sensitive, "大小写敏感");
                });

                // 搜索 / 停止（互斥）
                if state.is_searching {
                    if ui.button("停止").clicked() {
                        state.pending_stop = true;
                    }
                } else if ui.button("搜索").clicked() && !state.search_pattern.trim().is_empty() {
                    state.pending_search = true;
                }

                // 有结果时可切换「命中视图」
                if !state.search_results.is_empty() {
                    ui.separator();
                    ui.toggle_value(&mut state.in_result_mode, "显示命中");
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
                        ui.label(format!("{count} 个文件"));
                    }
                });
            });

            // 正则编译错误内联提示（不弹模态框，见 §7.5）
            if let Some(err) = &state.search_error {
                ui.label(egui::RichText::new(format!("检索错误：{err}")).color(egui::Color32::RED));
            }
        });
    });
}
