use crate::app::AppState;
use crate::core::search::SearchMode;

/// 检索输入框的稳定 `Id`（⌘F 快捷键据此聚焦）。
pub(crate) fn search_input_id() -> egui::Id {
    egui::Id::new("search_pattern_input")
}
/// 行号跳转输入框的稳定 `Id`（⌘L 快捷键据此聚焦）。
pub(crate) fn line_jump_input_id() -> egui::Id {
    egui::Id::new("line_jump_input")
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        // 窗口变窄时自动换行，而不是把控件挤出可视区
        ui.horizontal_wrapped(|ui| {
            // 应用名：弱化为一个标签，不与日志内容争夺注意力
            ui.label(egui::RichText::new("Hyper Log").strong());
            ui.separator();

            // 侧边栏目录树开关（⌘B 也可切换）
            let sb = ui
                .toggle_value(&mut state.show_sidebar, "☰")
                .on_hover_text("文件目录树（⌘B）");
            if sb.clicked() {
                state.save_prefs();
            }

            // —— 文件组 ——（检索中禁用「打开」，G1）
            if ui
                .add_enabled(!state.is_searching, egui::Button::new("打开"))
                .clicked()
            {
                state.pending_open = true;
            }
            // 打开目录（Q「打开目录」）：递归加载目录下所有日志文件。
            if ui
                .add_enabled(!state.is_searching, egui::Button::new("打开目录"))
                .clicked()
            {
                state.pending_open_dir = true;
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

            // —— 检索组 ——（编辑器的查找栏：输入框 + 模式 + 开关 + 执行）
            ui.add_enabled_ui(!state.is_searching, |ui| {
                let te = egui::TextEdit::singleline(&mut state.search_pattern)
                    .hint_text("查找…")
                    .desired_width(220.0)
                    .id(search_input_id());
                ui.add(te);

                egui::ComboBox::from_id_salt("search_mode")
                    .selected_text(match state.search_mode {
                        SearchMode::Plain => "文本",
                        SearchMode::Regex => "正则",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.search_mode, SearchMode::Plain, "纯文本");
                        ui.selectable_value(&mut state.search_mode, SearchMode::Regex, "正则");
                    });

                // `Aa` 比「大小写敏感」省一半宽度，语义由 tooltip 补充
                ui.toggle_value(&mut state.search_case_sensitive, "Aa")
                    .on_hover_text("大小写敏感");
            });

            // 搜索 / 停止（互斥）
            if state.is_searching {
                if ui.button("停止").clicked() {
                    state.pending_stop = true;
                }
            } else if ui.button("查找").clicked() && !state.search_pattern.trim().is_empty() {
                state.pending_search = true;
            }

            // 查找全部（Q「查找全部」）：对目录递归检索；进行中显示「停止」。
            if state.is_grepping {
                if ui.button("停止查找全部").clicked() {
                    state.pending_grep_stop = true;
                }
            } else if ui
                .add_enabled(
                    !state.is_searching && !state.search_pattern.trim().is_empty(),
                    egui::Button::new("查找全部"),
                )
                .on_hover_text("选择目录，递归检索目录下所有日志文件")
                .clicked()
            {
                state.pending_grep = true;
            }

            // 命中计数与结果视图切换（T15：检索中仅禁用 打开/搜索/导出）
            if !state.search_results.is_empty() {
                ui.separator();
                ui.weak(format!(
                    "{} 命中",
                    crate::util::group_digits(state.search_results.len())
                ));
                ui.toggle_value(&mut state.in_result_mode, "仅命中");

                if state.is_exporting {
                    if ui.button("取消导出").clicked() {
                        state.pending_export_cancel = true;
                    }
                } else {
                    let can_export = !state.is_searching;
                    if ui
                        .add_enabled(can_export, egui::Button::new("导出"))
                        .clicked()
                    {
                        state.pending_export = true;
                    }
                    if can_export {
                        ui.toggle_value(&mut state.export_with_prefix, "带前缀");
                    }
                }
            }
            ui.separator();

            // —— 视图组 ——
            // 行号跳转（⌘L 聚焦，回车跳转）：1-based 全局行号
            ui.label("行:");
            let line_resp = ui.add(
                egui::TextEdit::singleline(&mut state.line_jump)
                    .desired_width(56.0)
                    .id(line_jump_input_id()),
            );
            if line_resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Tab))
            {
                apply_line_jump(state);
            }

            // 折行开关：默认关，横向滚动（spec G7）
            let wrap_label = if state.wrap {
                "折行: 开"
            } else {
                "折行: 关"
            };
            let w = ui.toggle_value(&mut state.wrap, wrap_label);
            if w.clicked() {
                state.save_prefs();
            }

            // 性能 HUD 开关（spec P4/P5/P6 现场观测；也可经 HYPER_LOG_PERF=1 默认开启）
            ui.toggle_value(&mut state.show_perf, "性能");

            // 明暗主题切换：默认暗色（ui::theme::apply），点击切到另一套。
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
                // M17：主题偏好持久化
                state.prefs.theme = next.into();
                state.save_prefs();
            }
        });

        // 快捷键焦点请求：⌘F / ⌘L 在下一帧把焦点移到对应输入框。
        if state.focus_search {
            state.focus_search = false;
            ui.ctx().memory_mut(|m| m.request_focus(search_input_id()));
        }
        if state.focus_line_jump {
            state.focus_line_jump = false;
            ui.ctx()
                .memory_mut(|m| m.request_focus(line_jump_input_id()));
        }

        // 正则编译错误内联提示（不弹模态框，见 §7.5）
        if let Some(err) = &state.search_error {
            ui.label(egui::RichText::new(format!("检索错误：{err}")).color(egui::Color32::RED));
        }
    });
}

/// 解析「行:」输入框的 1-based 全局行号，越界则忽略（不报错）。
fn apply_line_jump(state: &mut AppState) {
    let total = state.fileset.total_lines();
    if total == 0 {
        return;
    }
    if let Ok(n) = state.line_jump.trim().parse::<usize>()
        && n >= 1
        && n <= total
    {
        let row = n - 1;
        state.scroll_target = Some(row);
        state.selected_row = Some(row);
    }
}
