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

                // 最近文件（M11 / spec Q3）：检索中整体禁用（G1）
                ui.add_enabled_ui(!state.is_searching, |ui| {
                    // 先快照条目，避免在菜单闭包内同时持有 recents 的借用与可变引用。
                    let entries: Vec<std::path::PathBuf> = state.recents.entries().to_vec();
                    ui.menu_button("最近文件", |ui| {
                        if entries.is_empty() {
                            ui.label("（暂无记录）");
                            return;
                        }
                        for p in entries {
                            // 菜单只显示文件名，完整路径放在悬浮提示里。
                            let name = p
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| p.display().to_string());
                            if ui
                                .button(name)
                                .on_hover_text(p.display().to_string())
                                .clicked()
                            {
                                state.pending_open_recent = Some(p);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("清除最近文件").clicked() {
                            state.recents.clear();
                            state.recents.save();
                            ui.close();
                        }
                    });
                });

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

                // 有结果时的命中视图切换与导出（T15：检索中仅禁用 打开/搜索/导出）
                if !state.search_results.is_empty() {
                    ui.separator();
                    ui.toggle_value(&mut state.in_result_mode, "显示命中");

                    if state.is_exporting {
                        if ui.button("取消导出").clicked() {
                            state.pending_export_cancel = true;
                        }
                    } else {
                        let can_export = !state.is_searching;
                        if ui
                            .add_enabled(can_export, egui::Button::new("导出结果"))
                            .clicked()
                        {
                            state.pending_export = true;
                        }
                        if can_export {
                            ui.toggle_value(&mut state.export_with_prefix, "带文件名前缀");
                        }
                    }
                }

                ui.separator();

                // 折行开关：默认关，横向滚动（spec G7）
                let wrap_label = if state.wrap {
                    "折行: 开"
                } else {
                    "折行: 关"
                };
                ui.toggle_value(&mut state.wrap, wrap_label);

                ui.separator();

                // 性能 HUD 开关（spec P4/P5/P6 现场观测；也可经 HYPER_LOG_PERF=1 默认开启）
                ui.toggle_value(&mut state.show_perf, "性能");

                ui.separator();

                // 明暗主题切换：默认暗色（app::setup_theme），点击切到另一套。
                // egui 0.36 的 dark/light 是两套独立 Style，set_theme 只切换当前使用的那套。
                let current = ui.ctx().theme();
                let (icon, tip, next) = match current {
                    egui::Theme::Dark => ("☀", "切换到亮色主题", egui::Theme::Light),
                    egui::Theme::Light => ("🌙", "切换到暗色主题", egui::Theme::Dark),
                };
                if ui
                    .add(egui::Button::new(icon).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    ui.ctx().set_theme(next);
                }

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
