//! 日志正文：编辑器风格的行号槽 + 虚拟滚动（spec §7.2 / §7.7）。
//!
//! 每行只创建 1 个 widget（承载整行多色的 `LayoutJob`），行号与行背景用 `Painter`
//! 直接绘制。相比"每行一个行号 Label + 每个着色分段一个 Label"，widget 数量从
//! O(行数 × 分段数) 降到 O(行数)，符合 spec §8.4 的每帧约束。

use std::borrow::Cow;

use eframe::egui::{self, Color32};

use crate::app::{AppState, PaneState};
use crate::core::indexer::FileSet;
use crate::highlight::{Highlighter, Level, Segment};
use crate::ui::theme::{self, Palette};

/// 单行高度（像素）。固定行高是虚拟滚动 O(1) 定位的前提（spec §7.2）。
///
/// 注意：`ScrollArea::show_rows` 还会把 `spacing.item_spacing.y` 累加到实际行距上。
pub const ROW_HEIGHT: f32 = 18.0;
/// 行号槽左右内边距。
const GUTTER_PAD: f32 = 8.0;
/// 行号槽与正文之间的留白。
const TEXT_PAD: f32 = 8.0;
/// 单个行号字符的估算宽度（11 px 等宽字体约 6.6 px）。
const GUTTER_CHAR_W: f32 = 6.8;
/// 单行渲染字符上限（spec Q10 / A9）：超出则截断，避免 egui 对超长行做字形布局而卡死。
const MAX_RENDER_CHARS: usize = 100_000;
/// 「折行」模式下统一行高的行数上限：防止 10 万字符的单行把行高撑到几千像素。
const MAX_WRAP_LINES: usize = 8;
/// 估算最长行时最多采样多少行（详见 [`estimate_content_width`]）。
const SAMPLE_LINES: usize = 20_000;

/// 滚动方向锁定阈值：次要方向的分量需超过主要方向的该比例，才被认为是「双向滚动」。
///
/// 触控板双指上下滑动几乎总带一点水平分量（手势不可能绝对垂直），若照单全收，
/// 正文就会在上下滑动的同时左右漂移。取 0.3 表示「垂直分量比水平大 3 倍以上才算纯垂直」。
const AXIS_LOCK_RATIO: f32 = 0.3;

/// 批量复制的行数上限。
///
/// 日志可达上亿行，⌘A 全选后若整段拼接进剪贴板会瞬间吃光内存（1 亿行 × 100B ≈ 10GB）。
/// 超过上限只复制区间的前 N 行并打 `warn` 日志。VS Code 无此限制（源码行数有限），
/// 但日志查看器必须设防。
const MAX_COPY_ROWS: usize = 100_000;

/// 按主控方向裁剪滚动增量：垂直占优时丢弃水平分量，水平占优时丢弃垂直分量。
///
/// 抽成纯函数以便单测（不依赖 `egui::Ui`）。
fn locked_scroll_delta(d: egui::Vec2) -> egui::Vec2 {
    let (ax, ay) = (d.x.abs(), d.y.abs());
    // 单方向输入（普通鼠标滚轮、纯水平/纯垂直触控板）无需锁定。
    if ax == 0.0 || ay == 0.0 {
        return d;
    }
    let (major, vertical_dominant) = if ay >= ax { (ay, true) } else { (ax, false) };
    let minor = if vertical_dominant { ax } else { ay };
    // 次要方向足够显著才保留（真正的斜向滚动），否则视为手抖丢弃。
    if minor >= major * AXIS_LOCK_RATIO {
        return d;
    }
    if vertical_dominant {
        egui::vec2(0.0, d.y)
    } else {
        egui::vec2(d.x, 0.0)
    }
}

/// 按本帧主控方向锁定滚动：垂直占优时丢弃水平分量，水平占优时丢弃垂直分量。
///
/// egui 的 `ScrollArea` 对 x/y **两个方向独立累加** `smooth_scroll_delta`、没有主控方向判定
/// （`scroll_area.rs` 中 `for d in 0..2` 分别取 `smooth_scroll_delta()[d]`），
/// 因此触控板上下滑动的轻微水平分量会被如实计入横向偏移——表现为「上下滑的时候内容左右跑」。
///
/// 必须在 `ScrollArea` 消费 delta **之前**改写 `InputState`，因为 ScrollArea 会在滚动后
/// 把对应分量清零，事后无法修正。
fn lock_scroll_axis(ui: &mut egui::Ui) {
    let d = ui.input(|i| i.smooth_scroll_delta);
    let locked = locked_scroll_delta(d);
    if locked != d {
        ui.ctx().input_mut(|i| i.smooth_scroll_delta = locked);
    }
}

/// 渲染一个日志面板。
///
/// `pane` 是该面板独立持有的状态（选中行/跳转/折行/横向范围/视图模式），`state` 只提供
/// 共享的只读数据（默认文件集、高亮器、检索结果）。这样两个面板可同时渲染而互不干扰——
/// 若沿用原先单一的 `&mut AppState`，面板 A 会先消费掉 `scroll_target` 和 ⌘C 快捷键，
/// 导致面板 B 永远收不到（spec §7.7.7）。
///
/// `pane_id` 用于区分各面板的 `ScrollArea` Id，并作为 ⌘C 复制的「活动面板」守卫。
///
/// 返回 `Some(word)` 表示用户在右键菜单点了「查找选中词」，由调用方把该词带入检索框
/// （`state` 在此是不可变借用，无法直接写 `search_pattern`，故用返回值上抛）。
pub fn show(
    ui: &mut egui::Ui,
    state: &AppState,
    pane: &mut PaneState,
    pane_id: usize,
) -> Option<String> {
    // 触控板上下滑动常带轻微水平分量，先锁定主控方向再交给 ScrollArea，
    // 避免正文在上下滚动时左右漂移。
    lock_scroll_axis(ui);
    let p = theme::palette(ui.ctx());
    let in_result = pane.in_result_mode && !state.search_results.is_empty();
    // 面板单独打开了文件就用它自己的文件集，否则共享全局文件集。
    // 持有所有权（`FileSet` 是 `Vec<Arc<..>>` 浅克隆，开销可忽略），避免对 `pane` 的不可变借用
    // 与闭包内 `pane.select_*` 等可变借用冲突（E0500）。
    let fileset: FileSet = match &pane.fileset_override {
        Some(fs) => fs.clone(),
        None => state.fileset.clone(),
    };
    // 字号由 ⌘+/⌘-/⌘0 缩放（持久化在 prefs）。行高与行号字号都从它派生，
    // 保证放大后行高、行号槽宽度同步增长，否则文字会挤在一起或被裁切。
    let font_size = state.font_size;
    let gutter_size = font_size * (theme::GUTTER_FONT_SIZE / theme::LOG_FONT_SIZE);
    // 默认字号下行高 18px（12.5px 字 → 1.44 倍行距），缩放时保持同一比例。
    let row_unit = font_size * (ROW_HEIGHT / theme::LOG_FONT_SIZE);

    let total = if in_result {
        state.search_results.len()
    } else {
        fileset.total_lines()
    };

    if total == 0 {
        empty_hint(ui, p);
        pane.selected_row = None;
        return None;
    }
    // 全量 ↔ 命中视图切换后行号语义变化，越界的选中行直接丢弃。
    if pane.selected_row.is_some_and(|r| r >= total) {
        pane.selected_row = None;
    }

    // 结果视图下，若已编译命中正则则复用同一 `Regex` 做命中高亮（G5）。
    let hl = if in_result {
        state
            .hit_regex
            .as_ref()
            .map(|r| state.highlighter.clone().with_hit(r.clone()))
            .unwrap_or_else(|| state.highlighter.clone())
    } else {
        state.highlighter.clone()
    };

    // 行号槽宽度按最大行号的位数自适应（spec §7.7）。命中视图的行号形如 `1.23456`，
    // 位数按「两组数字 + 一个点」估算。
    let digits = if in_result {
        digits_of(total) * 2 + 1
    } else {
        digits_of(total)
    };
    let gutter_char_w = GUTTER_CHAR_W * (gutter_size / theme::GUTTER_FONT_SIZE);
    let gutter_w = digits as f32 * gutter_char_w + GUTTER_PAD * 2.0;
    let avail_text_w = (ui.available_width() - gutter_w - TEXT_PAD * 2.0).max(120.0);

    // 横向滚动范围按「估算的最长行」固定：若跟随当前可见行，滚动条长度会随滚动抖动。
    // 首次（max_line_width==0）惰性估算，避免每帧扫描；`load_paths`/`reload` 清零各面板时也会重算。
    if pane.max_line_width <= 0.0 {
        pane.max_line_width = estimate_content_width(&fileset, font_size);
    }
    let content_w = pane.max_line_width.max(avail_text_w);
    let (row_h, text_w) = if pane.wrap {
        // 折行：行高统一按「最长行需折几行」放大（spec §7.7 的 MVP 方案）。
        // 逐行动态行高需要「行号 → y」的前缀和，与 1 亿行的 O(1) 定位（§7.2）冲突；
        // 同一日志文件的行长通常相近，统一行高的浪费有限。
        let lines = (content_w / avail_text_w)
            .ceil()
            .clamp(1.0, MAX_WRAP_LINES as f32);
        (row_unit * lines, avail_text_w)
    } else {
        (row_unit, content_w)
    };
    ui.style_mut().wrap_mode = Some(if pane.wrap {
        egui::TextWrapMode::Wrap
    } else {
        egui::TextWrapMode::Extend
    });

    let wrap_width = if pane.wrap {
        avail_text_w
    } else {
        f32::INFINITY
    };

    let mut clicked: Option<(usize, bool)> = None;
    let mut copied: Option<String> = None;
    // 右键菜单「查找选中词」上抛的词（不可变借用 `state`，故用返回值传回调用方落实检索）。
    let mut search_word: Option<String> = None;
    // 行号槽拖动多选：记录本次拖动中指针**悬停**到的那一行（闭包内每帧至多命中一个可见行）。
    let mut drag_hover: Option<usize> = None;

    let out = egui::ScrollArea::both()
        // 各面板必须用不同的 id，否则滚动位置会串（egui 按 Id 存 ScrollArea 状态）。
        .id_salt(("log_pane", pane_id))
        .auto_shrink([false; 2])
        .show_rows(ui, row_h, total, |ui, range| {
            for row in range {
                let (line, gutter_text) = row_content(state, &fileset, row, in_result);
                let text = truncate_for_render(&line);
                // 多选时整个区间都高亮（锚点模型见 `PaneState::selection_anchor`）。
                let selected = pane.is_row_selected(row);
                let is_cursor = pane.selected_row == Some(row);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // 行矩形宽度固定（含行号槽与正文留白），使横向滚动范围稳定
                    let row_rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(gutter_w + TEXT_PAD + text_w, row_h),
                    );

                    // 1) 行背景与行号：先画，位于文本之下
                    paint_row_bg(
                        ui,
                        row_rect,
                        gutter_w,
                        &gutter_text,
                        selected,
                        is_cursor,
                        gutter_size,
                    );

                    // 2) 行号槽：可点击选中整行，按住上下拖动 = 连续多选（VS Code 风）。
                    //    `click_and_drag` 同时提供 `drag_started`（按下帧）与 `dragged`/`hovered`（拖动中）。
                    let (_, gutter_resp) = ui.allocate_exact_size(
                        egui::vec2(gutter_w, row_h),
                        egui::Sense::click_and_drag(),
                    );

                    // 行号槽拖拽起点：Shift 时以既有选区为固定端扩展，否则从该行起新建选区。
                    if gutter_resp.drag_started() {
                        let shift = ui.input(|i| i.modifiers.shift);
                        if shift {
                            // 保留既有选中（selection_anchor 或 selected_row）作为固定端。
                            let keep = pane.selection_anchor.or(pane.selected_row);
                            pane.extend_selection_to(row);
                            pane.drag_anchor = keep;
                        } else {
                            pane.select_single(row);
                            pane.drag_anchor = Some(row);
                        }
                    }
                    // 拖动中：记录指针当前悬停的行，闭包结束后据此扩展选区。
                    if pane.drag_anchor.is_some() && gutter_resp.hovered() {
                        drag_hover = Some(row);
                    }

                    // 3) 正文：一个 Label 承载整行的多色分段
                    ui.add_space(TEXT_PAD);
                    let text_resp = ui
                        .allocate_ui_with_layout(
                            egui::vec2(text_w, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(build_job(
                                        &text, &hl, p, wrap_width, font_size,
                                    ))
                                    .selectable(true),
                                )
                            },
                        )
                        .inner;

                    // 4) 点击选中行、右键复制（Shift+点击 = 扩展选区到该行）
                    let resp = gutter_resp.union(text_resp);
                    if resp.clicked() {
                        let shift = ui.input(|i| i.modifiers.shift);
                        clicked = Some((row, shift));
                    }
                    // 双击取词：供 `⌘F` 自动带入检索框。egui 拿不到 `Label` 的选区文本，
                    // 故按点击位置反查字符索引，再向两侧扩到词边界（见 `word_at_offset`）。
                    if resp.double_clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let x_off = pos.x - (row_rect.min.x + gutter_w + TEXT_PAD);
                        if let Some(w) = word_at_offset(ui, &text, font_size, x_off) {
                            pane.selected_word = w;
                        }
                    }
                    resp.context_menu(|ui| {
                        let n = pane.selected_count();
                        // 多行选中时优先提供「复制选中」，单行时只给「复制此行」。
                        if n > 1 {
                            let label = if n > MAX_COPY_ROWS {
                                format!("复制选中行（前 {MAX_COPY_ROWS} 行，共 {n}）")
                            } else {
                                format!("复制选中的 {n} 行")
                            };
                            if ui.button(label).clicked() {
                                if let Some((s, e)) = pane.selected_range() {
                                    copied = Some(collect_selected_rows(
                                        state, &fileset, s, e, in_result,
                                    ));
                                }
                                ui.close();
                            }
                        }
                        if ui.button("复制此行").clicked() {
                            copied = Some(text.to_string());
                            ui.close();
                        }
                        // 带行号复制（`行号: 内容`，VS Code「Copy With Line Numbers」的日志版）：
                        // 多行选中复制整段带行号，单行只复制当前行带行号。
                        if ui.button("复制为带行号").clicked() {
                            let (s, e) = pane.selected_range().unwrap_or((row, row));
                            copied = Some(collect_selected_rows_with_numbers(
                                state, &fileset, s, e, in_result,
                            ));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("全选").clicked() {
                            pane.select_all(total - 1);
                            ui.close();
                        }
                        ui.separator();
                        // 查找选中词：优先用双击取到的词，否则回退整行（去首尾空白）。
                        let word = if pane.selected_word.is_empty() {
                            line.trim().to_string()
                        } else {
                            pane.selected_word.clone()
                        };
                        if ui.button("查找选中词").clicked() && !word.is_empty() {
                            search_word = Some(word);
                            ui.close();
                        }
                    });
                });
            }
        });

    // 行号槽拖动多选收尾：把「起点行 → 悬停行」的区间落实为当前选区。
    // 注意必须在消费 scroll_target **之前**处理，这样下方边缘自动滚动的 scroll_target 才能当帧生效。
    if pane.drag_anchor.is_some() {
        if let Some(hover) = drag_hover {
            let a = pane.drag_anchor.unwrap();
            pane.selection_anchor = Some(a);
            pane.selected_row = Some(hover);
        }
        // 拖动到可视区上下边缘时自动滚屏，以便选中屏幕外的行（VS Code 行为）。
        let vp = out.inner_rect;
        let margin = row_h * 2.0;
        let ptr = ui
            .input(|i| i.pointer.interact_pos())
            .unwrap_or(vp.center());
        if ptr.y <= vp.top() + margin {
            let top_row = (out.state.offset.y / row_h).floor().max(0.0) as usize;
            pane.scroll_target = Some(top_row.saturating_sub(1));
        } else if ptr.y >= vp.bottom() - margin {
            let visible = (vp.height() / row_h).floor() as usize;
            let top_row = (out.state.offset.y / row_h).floor().max(0.0) as usize;
            pane.scroll_target = Some((top_row + visible + 1).min(total - 1));
        }
        // 松手即结束拖拽。
        if ui.input(|i| i.pointer.any_released()) {
            pane.drag_anchor = None;
        }
    }

    // 行号跳转：消费本面板自己的 scroll_target，直接设置滚动区纵向偏移（行高固定 → row*row_h）。
    // 行高固定是虚拟滚动 O(1) 定位的前提，故可用闭式偏移精确跳转（spec §7.2）。
    if let Some(row) = pane.scroll_target.take() {
        let mut st = out.state;
        let content_h = total as f32 * row_h;
        let view_h = out.inner_rect.height().max(row_h);
        let target = (row as f32 * row_h - view_h * 0.5).clamp(0.0, (content_h - view_h).max(0.0));
        st.offset.y = target;
        st.store(ui.ctx(), out.id);
    }

    if let Some((row, shift)) = clicked {
        if shift {
            pane.extend_selection_to(row);
        } else {
            pane.select_single(row);
        }
    }
    if let Some(t) = copied {
        ui.ctx().copy_text(t);
    }

    // 仅**活动面板**响应快捷键：多个面板各自调用 `show`，若不加区分，`consume_shortcut`
    // 会被先渲染的面板一次性消费掉，后面的面板永远收不到（spec §7.7.7「先到先得」问题）。
    let is_active = pane_id == state.active_pane;

    // ⌘A 全选：锚点置 0、主选中行置末行。上亿行也只是两个 `usize`。
    // 检索框聚焦时 TextEdit 会先消费该快捷键（顶栏先渲染），不会走到这里。
    let select_all = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A);
    if is_active && ui.input_mut(|i| i.consume_shortcut(&select_all)) {
        pane.select_all(total - 1);
    }

    // ⌘C / Ctrl+C 复制选中行（编辑器习惯）。检索框获得焦点时由 TextEdit 先消费该快捷键。
    // 注意：`fileset` 在上方闭包里已用过，此处重新派生一份不可变借用，避免与前面的
    // `pane.select_*` 可变借用冲突（Rust 借用检查要求不可变借用不能跨可变借用存活）。
    let copy_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);
    if is_active
        && ui.input_mut(|i| i.consume_shortcut(&copy_shortcut))
        && let Some((s, e)) = pane.selected_range()
    {
        let fs = pane.fileset_override.as_ref().unwrap_or(&state.fileset);
        let text = collect_selected_rows(state, fs, s, e, in_result);
        ui.ctx().copy_text(text);
    }

    // 键盘导航（VS Code 编辑器风格）：
    // - ↑/↓ 移动光标行（清空多选锚点），Shift+↑/↓ 扩展选区；
    // - PageUp/PageDown 整页翻动；Home/End 跳全文首/尾。
    // 仅活动面板响应（多面板「先到先得」问题，同 ⌘C）。移动后借 `scroll_target` 把目标行滚入可视区。
    if is_active {
        let cur = pane.selected_row.unwrap_or(0);
        let page = (out.inner_rect.height() / row_h).max(1.0) as usize;
        let nav = |ui: &mut egui::Ui, m: egui::Modifiers, key: egui::Key| {
            ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(m, key)))
        };
        let (target, extend) = if nav(ui, egui::Modifiers::NONE, egui::Key::ArrowDown) {
            (cur.saturating_add(1).min(total - 1), false)
        } else if nav(ui, egui::Modifiers::NONE, egui::Key::ArrowUp) {
            (cur.saturating_sub(1), false)
        } else if nav(ui, egui::Modifiers::SHIFT, egui::Key::ArrowDown) {
            (cur.saturating_add(1).min(total - 1), true)
        } else if nav(ui, egui::Modifiers::SHIFT, egui::Key::ArrowUp) {
            (cur.saturating_sub(1), true)
        } else if nav(ui, egui::Modifiers::NONE, egui::Key::PageDown) {
            (cur.saturating_add(page).min(total - 1), false)
        } else if nav(ui, egui::Modifiers::NONE, egui::Key::PageUp) {
            (cur.saturating_sub(page), false)
        } else if nav(ui, egui::Modifiers::NONE, egui::Key::Home) {
            (0, false)
        } else if nav(ui, egui::Modifiers::NONE, egui::Key::End) {
            (total - 1, false)
        } else {
            (cur, false)
        };
        if target != cur || extend {
            if extend {
                pane.extend_selection_to(target);
            } else {
                pane.select_single(target);
            }
            pane.scroll_target = Some(target);
        }
    }

    search_word
}

/// 把 `[start, end]` 区间内的行用「`行号: 内容`」格式连接，供「复制为带行号」使用。
///
/// 行号取自 `row_content` 的 `gutter_text`（全量视图是 1 起全局行号，命中视图是
/// `<文件序>.<行号>`），与正文所见一致。同样受 [`MAX_COPY_ROWS`] 上限约束。
fn collect_selected_rows_with_numbers(
    state: &AppState,
    fileset: &FileSet,
    start: usize,
    end: usize,
    in_result: bool,
) -> String {
    let last = end.min(start.saturating_add(MAX_COPY_ROWS).saturating_sub(1));
    let mut out = String::with_capacity((last - start + 1) * 72);
    for r in start..=last {
        if r > start {
            out.push('\n');
        }
        let (line, num) = row_content(state, fileset, r, in_result);
        out.push_str(&num);
        out.push_str(": ");
        out.push_str(&line);
    }
    out
}

/// 把 `[start, end]` 区间内的行文本用 `\n` 连接，供批量复制使用。
///
/// 行数超过 [`MAX_COPY_ROWS`] 时只取前 `MAX_COPY_ROWS` 行（并打 `warn`），
/// 避免 ⌘A 全选上亿行时把内存吃光。
fn collect_selected_rows(
    state: &AppState,
    fileset: &FileSet,
    start: usize,
    end: usize,
    in_result: bool,
) -> String {
    let last = end.min(start.saturating_add(MAX_COPY_ROWS).saturating_sub(1));
    if last < end {
        log::warn!(
            "复制行数 {} 超过上限 {}，只复制前 {} 行",
            end - start + 1,
            MAX_COPY_ROWS,
            MAX_COPY_ROWS
        );
    }
    let mut out = String::with_capacity((last - start + 1) * 64);
    for r in start..=last {
        if r > start {
            out.push('\n');
        }
        out.push_str(&row_content(state, fileset, r, in_result).0);
    }
    out
}

/// 取第 `row` 行的文本与行号槽文本。
///
/// 全量视图下 `row` 是全局行号；命中视图下是命中索引，行号显示为 `<文件序>.<行号>`。
fn row_content<'a>(
    state: &'a AppState,
    fileset: &'a FileSet,
    row: usize,
    in_result: bool,
) -> (Cow<'a, str>, String) {
    if in_result {
        let hit = state.search_results[row];
        let line = fileset
            .file(hit.file_idx as usize)
            .and_then(|f| f.line(hit.line_idx as usize))
            .unwrap_or_default();
        (line, format!("{}.{}", hit.file_idx + 1, hit.line_idx + 1))
    } else {
        let line = fileset.line(row).unwrap_or_default();
        (line, (row + 1).to_string())
    }
}

/// 绘制一行的背景、行号槽与行号。必须在正文 widget 之前调用（Painter 按调用顺序叠放）。
///
/// `selected` = 该行处于多选区间内；`is_cursor` = 该行是「光标行」（`selected_row` 主端点）。
/// 光标行在 VS Code 风里会多一根左侧 accent 竖条、且行号加亮，便于一眼定位。
fn paint_row_bg(
    ui: &mut egui::Ui,
    row_rect: egui::Rect,
    gutter_w: f32,
    gutter_text: &str,
    selected: bool,
    is_cursor: bool,
    gutter_size: f32,
) {
    // 色板是进程级常量，从 ctx 取即可，不必每行额外传参。
    let p = theme::palette(ui.ctx());
    if selected {
        ui.painter().rect_filled(row_rect, 0.0, p.row_active);
    } else if ui.rect_contains_pointer(row_rect) {
        ui.painter().rect_filled(row_rect, 0.0, p.row_hover);
    }
    // 光标行：左侧 accent 竖条（VS Code 当前行指示条，~2px）。必须画在 bg 之上、正文之下。
    if is_cursor {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(row_rect.min, egui::vec2(2.0, row_rect.height())),
            0.0,
            p.accent,
        );
    }

    // 行号槽背景 + 与正文之间的竖线
    let gutter_rect =
        egui::Rect::from_min_size(row_rect.min, egui::vec2(gutter_w, row_rect.height()));
    ui.painter().rect_filled(gutter_rect, 0.0, p.gutter);
    let x = row_rect.min.x + gutter_w;
    ui.painter()
        .vline(x, row_rect.y_range(), egui::Stroke::new(1.0, p.gutter_line));

    // 行号：右对齐到行号槽内边距，随行高垂直居中。光标行加亮为 text_strong。
    ui.painter().text(
        egui::pos2(x - GUTTER_PAD, row_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        gutter_text,
        egui::FontId::monospace(gutter_size),
        if is_cursor {
            p.text_strong
        } else if selected {
            p.text
        } else {
            p.text_dim
        },
    );
}

/// 把一行的着色分段打包成单个 `LayoutJob`，供一个 `Label` 渲染整行。
fn build_job(
    line: &str,
    hl: &Highlighter,
    p: &Palette,
    wrap_width: f32,
    font_size: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let font = egui::FontId::monospace(font_size);
    for seg in crate::highlight::segments(line, hl) {
        let (text, color, background) = match seg {
            Segment::Plain(t) => (t, p.text, Color32::TRANSPARENT),
            Segment::Timestamp(t) => (t, p.timestamp, Color32::TRANSPARENT),
            Segment::Level(t, lvl) => (t, level_color(lvl, p), Color32::TRANSPARENT),
            // 命中：保持正常文字色，仅加背景高亮（VS Code 查找命中的观感），
            // 避免低对比的「高亮文字」盖住内容导致看不清。
            Segment::Hit(t) => (t, p.text, p.hit_bg),
        };
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                background,
                ..Default::default()
            },
        );
    }
    job
}

/// 级别 → 颜色。错误/致命为红，警告为黄，Info 蓝，Debug 绿，Trace/Verbose 灰。
fn level_color(lvl: Level, p: &Palette) -> Color32 {
    match lvl {
        Level::Fatal | Level::Error => p.level_error,
        Level::Warn => p.level_warn,
        Level::Info => p.level_info,
        Level::Debug => p.level_debug,
        Level::Trace | Level::Verbose => p.level_trace,
    }
}

/// 空状态：居中提示，避免一个纯色空面板让人误以为程序卡住。
fn empty_hint(ui: &mut egui::Ui, p: &Palette) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Hyper Log")
                    .size(24.0)
                    .color(p.text_dim),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("尚未打开日志文件")
                    .size(14.0)
                    .color(p.text),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("点击工具栏「打开」选择文件，或从「最近文件」中选取")
                    .size(11.5)
                    .color(p.text_dim),
            );
        });
    });
}

/// 十进制位数（用于行号槽宽度）。
fn digits_of(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

/// 超过上限的字符截断，附加提示后缀，避免极端长行拖垮渲染（spec Q10）。
fn truncate_for_render(line: &str) -> Cow<'_, str> {
    if line.chars().count() <= MAX_RENDER_CHARS {
        Cow::Borrowed(line)
    } else {
        let truncated: String = line.chars().take(MAX_RENDER_CHARS).collect();
        Cow::Owned(format!("{truncated} …(已截断，完整内容见导出)"))
    }
}

/// 双击时按点击位置（相对行文本起点的 x 偏移）反查字符索引，再取出所在的词。
///
/// egui 不暴露 `Label` 的选区文本，故走「排版一次 → 用光标位置求字符索引」的路子：
/// `Painter::layout_no_wrap` 只需 `&self`（不必 `fonts_mut`），单行排版开销可忽略。
fn word_at_offset(ui: &mut egui::Ui, line: &str, font_size: f32, x_offset: f32) -> Option<String> {
    if x_offset < 0.0 {
        return None;
    }
    let galley = ui.painter().layout_no_wrap(
        line.to_string(),
        egui::FontId::monospace(font_size),
        // 只为测量位置，颜色不参与渲染
        egui::Color32::WHITE,
    );
    // `cursor_from_pos` 返回 `CCursor`，其 `.index` 是 `CharIndex`（字符索引），`word_at`
    // 内部用 `chars[char_idx]` 按字符下标访问，二者一致；`CharIndex` 可 `.into()` 转 usize。
    let cursor = galley.cursor_from_pos(egui::vec2(x_offset, 0.0));
    word_at(line, cursor.index.into())
}

/// 取 `char_idx` 所在的词（向两侧扩到词边界）；落在分隔符上或越界时返回 `None`。
fn word_at(line: &str, char_idx: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if char_idx >= chars.len() || !is_word_char(chars[char_idx]) {
        return None;
    }
    let mut s = char_idx;
    while s > 0 && is_word_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = char_idx;
    while e < chars.len() && is_word_char(chars[e]) {
        e += 1;
    }
    let w: String = chars[s..e].iter().collect();
    (!w.is_empty()).then_some(w)
}

/// 词字符：排除空白与常见分隔符，其余（含 `.` `:` `-` `_`）都算词内字符，
/// 这样 `com.example.Foo`、`12:00:03.123` 这类日志 token 会被整体取到。
fn is_word_char(c: char) -> bool {
    !(c.is_whitespace()
        || matches!(
            c,
            ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '<' | '>' | '|'
        ))
}

/// 估算最长行的渲染宽度（像素），用于固定横向滚动范围。
///
/// 只采样前 [`SAMPLE_LINES`] 行：对上亿行的文件全量扫描不可接受，而日志的行宽分布
/// 通常稳定。取最大值而非平均值——横向滚动必须能覆盖最长的那一行。
///
/// CJK 字形约为等宽拉丁字符的两倍宽，因此按「ASCII 记 1、非 ASCII 记 1.7」加权。
pub fn estimate_content_width(fileset: &crate::core::indexer::FileSet, font_size: f32) -> f32 {
    let lines = fileset.total_lines().min(SAMPLE_LINES);
    let mut max = 0.0_f32;
    for i in 0..lines {
        if let Some(line) = fileset.line(i) {
            let w = estimate_text_width(&line, font_size);
            if w > max {
                max = w;
            }
        }
    }
    max
}

/// 按字符类别加权的宽度估算（单位：像素）。
///
/// 基准常量是 `theme::LOG_FONT_SIZE`（12.5px）下的实测字宽，故按当前字号等比缩放——
/// 否则 `⌘+` 放大后估算值偏小，横向滚动条会滚不到行尾。
///
/// 常量随 Monospace 主字体（SauceCodePro Nerd Font）与中文兜底（NotoSerifSC）校准：
/// SauceCodePro 拉丁 advance = 0.6em → 12.5px 下 7.5px；NotoSerifSC 中文 advance = 1.0em
/// → 12.5px 下 12.5px（满宽）。旧值 7.2/12.2 是 Hack + MiSans 的实测值。
fn estimate_text_width(line: &str, font_size: f32) -> f32 {
    const ASCII_W: f32 = 7.5; // 12.5px 等宽拉丁字符字宽（SauceCodePro 0.6em）
    const WIDE_W: f32 = 12.5; // CJK 全角字符满宽（NotoSerifSC 1.0em）
    let scale = font_size / theme::LOG_FONT_SIZE;
    let mut w = 0.0_f32;
    for c in line.chars() {
        w += if c.is_ascii() { ASCII_W } else { WIDE_W };
    }
    w * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_of_matches_decimal_width() {
        assert_eq!(digits_of(0), 1);
        assert_eq!(digits_of(9), 1);
        assert_eq!(digits_of(10), 2);
        assert_eq!(digits_of(99_999_999), 8);
    }

    /// 触控板上下滑动带轻微水平分量时，水平分量应被丢弃，避免正文左右漂移。
    #[test]
    fn lock_drops_horizontal_when_vertical_dominant() {
        let v = egui::vec2(3.0, 100.0);
        assert_eq!(locked_scroll_delta(v), egui::vec2(0.0, 100.0));
        // 向上滑（负 y）同样只保留垂直分量
        assert_eq!(
            locked_scroll_delta(egui::vec2(-4.0, -80.0)),
            egui::vec2(0.0, -80.0)
        );
    }

    /// 反之：水平滑动时的轻微垂直分量也应丢弃。
    #[test]
    fn lock_drops_vertical_when_horizontal_dominant() {
        assert_eq!(
            locked_scroll_delta(egui::vec2(100.0, 2.0)),
            egui::vec2(100.0, 0.0)
        );
        assert_eq!(
            locked_scroll_delta(egui::vec2(-90.0, -5.0)),
            egui::vec2(-90.0, 0.0)
        );
    }

    /// 真正的斜向滚动（次要方向足够显著）应原样保留，不能锁死双向滚动。
    #[test]
    fn lock_keeps_diagonal_when_minor_is_significant() {
        let v = egui::vec2(80.0, 100.0);
        assert_eq!(locked_scroll_delta(v), v);
        let h = egui::vec2(100.0, 70.0);
        assert_eq!(locked_scroll_delta(h), h);
    }

    /// 单方向输入（普通鼠标滚轮）不受影响，避免误伤纯垂直/纯水平滚动。
    #[test]
    fn lock_leaves_single_axis_untouched() {
        assert_eq!(
            locked_scroll_delta(egui::vec2(0.0, 40.0)),
            egui::vec2(0.0, 40.0)
        );
        assert_eq!(
            locked_scroll_delta(egui::vec2(40.0, 0.0)),
            egui::vec2(40.0, 0.0)
        );
        assert_eq!(locked_scroll_delta(egui::Vec2::ZERO), egui::Vec2::ZERO);
    }

    #[test]
    fn truncate_keeps_short_lines_borrowed() {
        let line = "short";
        assert!(matches!(truncate_for_render(line), Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_cuts_very_long_lines() {
        let line = "x".repeat(MAX_RENDER_CHARS + 10);
        let out = truncate_for_render(&line);
        assert!(out.len() > MAX_RENDER_CHARS);
        assert!(out.ends_with("…(已截断，完整内容见导出)"));
    }

    #[test]
    fn wide_chars_count_more_than_ascii() {
        // CJK 字形约为拉丁字符的两倍宽，估算必须体现这一点，否则中文日志会被横向截断
        let ascii = estimate_text_width("aaaa", theme::LOG_FONT_SIZE);
        let cjk = estimate_text_width("中中中中", theme::LOG_FONT_SIZE);
        assert!(cjk > ascii * 1.5, "ascii={ascii}, cjk={cjk}");
    }

    // —— 双击取词（word_at / is_word_char） ——

    #[test]
    fn word_at_extracts_log_token_with_punctuation() {
        // 日志 token 里 `.` `:` `-` 都算词内字符，双击 `com.example.Foo` 应整体取到
        let line = "error at com.example.Foo:42 failed";
        let start = line.find("com.example").unwrap();
        assert_eq!(word_at(line, start).as_deref(), Some("com.example.Foo:42"));
    }

    #[test]
    fn word_at_rejects_delimiters_and_out_of_range() {
        let line = "a, b";
        // 逗号是分隔符（不在词内），落在它上面返回 None
        assert_eq!(word_at(line, 1), None);
        // 越界返回 None
        assert_eq!(word_at(line, 99), None);
        // 空白也返回 None
        let s = "foo bar";
        assert_eq!(word_at(s, 3), None);
    }

    #[test]
    fn word_at_handles_cjk_and_leading_boundary() {
        // 词首/词尾的边界字符都能正确取词（空格是分隔符，`=` 按日志 token 语义算词内字符）
        let line = "错误码 42";
        assert_eq!(word_at(line, 0).as_deref(), Some("错误码"));
        assert_eq!(
            word_at(line, line.chars().count() - 1).as_deref(),
            Some("42")
        );
        // `=` 不算分隔符：key=value 是完整日志 token，双击应整体取到
        assert_eq!(word_at("code=42", 0).as_deref(), Some("code=42"));
    }

    #[test]
    fn level_colors_cover_all_levels() {
        // 各级别必须映射到互不相同的颜色，否则着色等于没做
        let p = &theme::DARK;
        let all = [
            level_color(Level::Fatal, p),
            level_color(Level::Error, p),
            level_color(Level::Warn, p),
            level_color(Level::Info, p),
            level_color(Level::Debug, p),
            level_color(Level::Trace, p),
        ];
        let distinct: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(distinct.len(), 5, "Fatal/Error 同色，其余应各不同：{all:?}");
    }

    // —— 带行号复制（collect_selected_rows_with_numbers） ——

    /// 在临时目录建一个含 3 行内容的日志文件并索引，返回其 `FileSet`。
    fn tmp_fileset(name: &str) -> (crate::core::indexer::FileSet, std::path::PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join("hyper-log-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{seq}_{name}"));
        std::fs::write(&p, "alpha\nbeta gamma\ndelta\n").unwrap();
        let idx = crate::core::indexer::LogFileIndex::open(&p).unwrap();
        let mut fs = crate::core::indexer::FileSet::new();
        fs.push(std::sync::Arc::new(idx));
        (fs, p)
    }

    #[test]
    fn collect_with_numbers_prefixes_each_line_with_1_based_number() {
        let (fs, p) = tmp_fileset("numbered.log");
        let s = AppState::default();
        let out = collect_selected_rows_with_numbers(&s, &fs, 0, 2, false);
        // 行号从 1 起，`行号: 内容` 逐行拼接
        assert_eq!(out, "1: alpha\n2: beta gamma\n3: delta");
        drop(fs);
        let _ = std::fs::remove_file(&p);
    }
}
