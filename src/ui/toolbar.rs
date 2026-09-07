use crate::app::{AppState, HitDir, MAX_PANES, SplitDir};
use crate::core::search::SearchMode;

/// 检索输入框的稳定 `Id`（⌘F 快捷键据此聚焦）。
pub(crate) fn search_input_id() -> egui::Id {
    egui::Id::new("search_pattern_input")
}
/// 行号跳转输入框的稳定 `Id`（⌘L 快捷键据此聚焦）。
pub(crate) fn line_jump_input_id() -> egui::Id {
    egui::Id::new("line_jump_input")
}

/// 顶栏：双行分组布局（VS Code 风）。
///
/// 第一行（主操作栏）：品牌 + 侧栏开关 | 打开/打开目录/最近文件 | 居中放大查找栏 | 性能 + 主题。
/// 第二行（上下文/视图栏）：行号跳转 + 折行 + 拆分面板 + 命中结果（命中数/仅命中/导出/带前缀）。
/// 双行比原单行 `horizontal_wrapped` 更稳定：控件不再因窗口变窄而互相挤压换行。
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        // 第一行：主操作栏（左组 + 居中查找栏 + 右组）。
        //
        // 不用 egui 内置弹性空间（需要手动算像素），而用「定长左组、定长查找栏、
        // 剩余宽度让右组吸到右端 + 顶部行内 right_to_left」的组合：
        // 把整行视为「左段 → 中段(查找栏) → 右段(右对齐)」，但 egui 的水平布局天然从
        // 左到右推进，所以采用「左段正常布局 → 中段查找栏 → 余下空间全部 `add_space`
        // 推掉，让右段单独占据右侧」的三段写法。
        let row_h = ui.spacing().interact_size.y.max(22.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                // —— 左段：品牌 + 侧栏开关 + 文件组（图标化按钮）。
                let brand = crate::ui::theme::palette(ui.ctx()).text_strong;
                ui.label(egui::RichText::new("Hyper Log").color(brand));
                ui.add_space(4.0);

                // 侧边栏目录树开关（⌘B 也可切换）
                let sb = ui
                    .toggle_value(&mut state.show_sidebar, "☰")
                    .on_hover_text("文件目录树（⌘B）");
                if sb.clicked() {
                    state.save_prefs();
                }

                ui.separator();

                // —— 文件组 ——（检索中禁用「打开」，G1）
                let can_open = !state.is_searching;
                if icon_button(ui, "📂", "打开文件（⌘O）", can_open).clicked() {
                    state.pending_open = true;
                }
                if icon_button(ui, "📁", "打开目录（递归加载日志）", can_open).clicked()
                {
                    state.pending_open_dir = true;
                }

                // 最近文件（M11 / spec Q3）：检索中整体禁用（G1）
                ui.add_enabled_ui(can_open, |ui| {
                    // 先快照条目，避免在菜单闭包内同时持有 recents 的借用与可变引用。
                    let entries: Vec<std::path::PathBuf> = state.recents.entries().to_vec();
                    ui.menu_button("🕘 最近", |ui| {
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

                // —— 中段：居中放大的查找栏。
                // 计算查找栏宽度：根据窗口宽度在 [220, 420] 之间伸缩，留出左右段空间。
                let row_w = ui.available_width();
                let find_w = 380.0_f32.clamp(160.0, (row_w - 220.0).max(160.0));
                // 左侧额外留 12px 让左段与查找栏不至于贴在一起。
                ui.add_space(12.0);
                search_bar(ui, state, find_w);
                // 把剩余宽度全部吃掉，让右段被推到最右端。
                let leftover = ui.available_width();
                if leftover > 0.0 {
                    ui.add_space(leftover);
                }

                // —— 右段：性能 + 主题切换（右对齐靠 allocate_ui 内嵌 right_to_left）。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 性能 HUD 开关（spec P4/P5/P6 现场观测；也可经 HYPER_LOG_PERF=1 默认开启）
                    ui.toggle_value(&mut state.show_perf, "性能")
                        .on_hover_text("性能 HUD（FPS/帧耗时/p95）");

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
            },
        );

        // 顶栏底部分隔线（编辑器观感：顶栏与正文用一条 1px 浅色线分开，
        // 比 egui 默认的零分隔更具结构感）。
        ui.painter().hline(
            ui.available_rect_before_wrap().x_range(),
            ui.available_rect_before_wrap().top(),
            egui::Stroke::new(1.0, crate::ui::theme::palette(ui.ctx()).border),
        );

        // 第二行：上下文 / 视图栏。
        ui.horizontal_wrapped(|ui| {
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

            // 折行开关：默认关，横向滚动（spec G7）。每面板独立，切换写回活动面板。
            let wrap_label = if state.wrap {
                "折行: 开"
            } else {
                "折行: 关"
            };
            let w = ui.toggle_value(&mut state.wrap, wrap_label);
            if w.clicked() {
                state.apply_wrap_to_active_pane();
                state.save_prefs();
            }

            ui.separator();

            // 拆分面板（VSCode 风格，最多 4 个，spec §7.7.7）
            let can_split = state.pane_layout.count < MAX_PANES;
            if ui
                .add_enabled(can_split, egui::Button::new("右拆"))
                .on_hover_text("向右拆分出一个新面板")
                .clicked()
            {
                state.split_pane(SplitDir::Horizontal);
                state.save_prefs();
            }
            if ui
                .add_enabled(can_split, egui::Button::new("下拆"))
                .on_hover_text("向下拆分出一个新面板")
                .clicked()
            {
                state.split_pane(SplitDir::Vertical);
                state.save_prefs();
            }
            if ui
                .add_enabled(state.pane_layout.count > 1, egui::Button::new("关闭面板"))
                .on_hover_text("关闭当前活动面板")
                .clicked()
            {
                state.close_pane(state.active_pane);
                state.save_prefs();
            }

            // 命中结果组：仅在存在检索结果时出现（T15：检索中仅禁用 打开/搜索/导出）
            if !state.search_results.is_empty() {
                ui.separator();
                // 命中计数：导航过就显示「第 N / 共 M 处」（VS Code 的查找计数观感），
                // 否则退回原来的「N 命中」，避免无导航时凭空多出「第 0 处」的怪异文案。
                let hits = state.search_results.len();
                let label = match state.hit_cursor {
                    Some(c) if hits > 0 => format!(
                        "第 {} / 共 {} 处",
                        crate::util::group_digits(c + 1),
                        crate::util::group_digits(hits)
                    ),
                    _ => format!("{} 命中", crate::util::group_digits(hits)),
                };
                ui.weak(label);
                // 命中导航按钮（等价 F3 / ⇧F3）。图标用中文单字而非 Unicode 箭头：
                // egui 字体链对几何/箭头符号覆盖不可靠，中文由内嵌 NotoSerifSC 确定覆盖。
                if hits > 0 {
                    if ui
                        .small_button("上")
                        .on_hover_text("上一处 (Shift+F3)")
                        .clicked()
                    {
                        state.goto_hit(HitDir::Prev);
                    }
                    if ui.small_button("下").on_hover_text("下一处 (F3)").clicked() {
                        state.goto_hit(HitDir::Next);
                    }
                }
                // 「仅命中」是每面板独立的视图模式：切换只作用于活动面板（spec §7.7.7）。
                let mut in_res = state
                    .active_pane()
                    .map(|p| p.in_result_mode)
                    .unwrap_or(false);
                ui.toggle_value(&mut in_res, "仅命中");
                state.active_pane_mut().in_result_mode = in_res;
                state.in_result_mode = in_res; // 顶层镜像

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

            // 正则编译错误内联提示（不弹模态框，见 §7.5）
            if let Some(err) = &state.search_error {
                ui.separator();
                ui.label(egui::RichText::new(format!("检索错误：{err}")).color(egui::Color32::RED));
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
    });
}

/// 第一行的查找栏：放大输入框 + 模式下拉 + `Aa` 开关 + 查找/停止 + 查找全部。
fn search_bar(ui: &mut egui::Ui, state: &mut AppState, find_w: f32) {
    ui.add_enabled_ui(!state.is_searching, |ui| {
        let te = egui::TextEdit::singleline(&mut state.search_pattern)
            .hint_text("🔍 查找…")
            .desired_width(find_w)
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
}

/// 图标按钮：`label` 作图标，`tip` 作悬浮提示；`enabled` 为 false 时禁用。
fn icon_button(ui: &mut egui::Ui, label: &str, tip: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(label))
        .on_hover_text(tip)
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
        // 跳转只作用于活动面板（spec §7.7.7）。
        state.jump_to_row(row);
        state.active_pane_mut().selected_row = Some(row);
        state.selected_row = Some(row);
    }
}
