//! 日志正文：编辑器风格的行号槽 + 虚拟滚动（spec §7.2 / §7.7）。
//!
//! 每行只创建 1 个 widget（承载整行多色的 `LayoutJob`），行号与行背景用 `Painter`
//! 直接绘制。相比"每行一个行号 Label + 每个着色分段一个 Label"，widget 数量从
//! O(行数 × 分段数) 降到 O(行数)，符合 spec §8.4 的每帧约束。

use std::borrow::Cow;

use eframe::egui::{self, Color32};

use crate::app::AppState;
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

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let p = theme::palette(ui.ctx());
    let in_result = state.in_result_mode && !state.search_results.is_empty();
    let total = if in_result {
        state.search_results.len()
    } else {
        state.fileset.total_lines()
    };

    if total == 0 {
        empty_hint(ui, p);
        state.selected_row = None;
        return;
    }
    // 全量 ↔ 命中视图切换后行号语义变化，越界的选中行直接丢弃。
    if state.selected_row.is_some_and(|r| r >= total) {
        state.selected_row = None;
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
    let gutter_w = digits as f32 * GUTTER_CHAR_W + GUTTER_PAD * 2.0;
    let avail_text_w = (ui.available_width() - gutter_w - TEXT_PAD * 2.0).max(120.0);

    // 横向滚动范围按「估算的最长行」固定：若跟随当前可见行，滚动条长度会随滚动抖动。
    let content_w = state.max_line_width.max(avail_text_w);
    let (row_h, text_w) = if state.wrap {
        // 折行：行高统一按「最长行需折几行」放大（spec §7.7 的 MVP 方案）。
        // 逐行动态行高需要「行号 → y」的前缀和，与 1 亿行的 O(1) 定位（§7.2）冲突；
        // 同一日志文件的行长通常相近，统一行高的浪费有限。
        let lines = (content_w / avail_text_w)
            .ceil()
            .clamp(1.0, MAX_WRAP_LINES as f32);
        (ROW_HEIGHT * lines, avail_text_w)
    } else {
        (ROW_HEIGHT, content_w)
    };
    ui.style_mut().wrap_mode = Some(if state.wrap {
        egui::TextWrapMode::Wrap
    } else {
        egui::TextWrapMode::Extend
    });

    let wrap_width = if state.wrap {
        avail_text_w
    } else {
        f32::INFINITY
    };

    let mut clicked: Option<usize> = None;
    let mut copied: Option<String> = None;

    let out = egui::ScrollArea::both().auto_shrink([false; 2]).show_rows(
        ui,
        row_h,
        total,
        |ui, range| {
            for row in range {
                let (line, gutter_text) = row_content(state, row, in_result);
                let text = truncate_for_render(&line);
                let selected = state.selected_row == Some(row);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // 行矩形宽度固定（含行号槽与正文留白），使横向滚动范围稳定
                    let row_rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(gutter_w + TEXT_PAD + text_w, row_h),
                    );

                    // 1) 行背景与行号：先画，位于文本之下
                    paint_row_bg(ui, row_rect, gutter_w, &gutter_text, selected, p);

                    // 2) 行号槽：可点击选中整行
                    let (_, gutter_resp) =
                        ui.allocate_exact_size(egui::vec2(gutter_w, row_h), egui::Sense::click());

                    // 3) 正文：一个 Label 承载整行的多色分段
                    ui.add_space(TEXT_PAD);
                    let text_resp = ui
                        .allocate_ui_with_layout(
                            egui::vec2(text_w, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(build_job(&text, &hl, p, wrap_width))
                                        .selectable(true),
                                )
                            },
                        )
                        .inner;

                    // 4) 点击选中行、右键复制该行
                    let resp = gutter_resp.union(text_resp);
                    if resp.clicked() {
                        clicked = Some(row);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("复制此行").clicked() {
                            copied = Some(text.to_string());
                            ui.close();
                        }
                    });
                });
            }
        },
    );

    // 行号跳转：消费 scroll_target，直接设置滚动区纵向偏移（行高固定 → row*row_h）。
    // 行高固定是虚拟滚动 O(1) 定位的前提，故可用闭式偏移精确跳转（spec §7.2）。
    if let Some(row) = state.scroll_target.take() {
        let mut st = out.state;
        let content_h = total as f32 * row_h;
        let view_h = out.inner_rect.height().max(row_h);
        let target = (row as f32 * row_h - view_h * 0.5).clamp(0.0, (content_h - view_h).max(0.0));
        st.offset.y = target;
        st.store(ui.ctx(), out.id);
    }

    if let Some(r) = clicked {
        state.selected_row = Some(r);
    }
    if let Some(t) = copied {
        ui.ctx().copy_text(t);
    }

    // ⌘C / Ctrl+C 复制选中行（编辑器习惯）。检索框获得焦点时由 TextEdit 先消费该快捷键。
    let copy_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);
    if ui.input_mut(|i| i.consume_shortcut(&copy_shortcut))
        && let Some(row) = state.selected_row
    {
        ui.ctx()
            .copy_text(row_content(state, row, in_result).0.into_owned());
    }
}

/// 取第 `row` 行的文本与行号槽文本。
///
/// 全量视图下 `row` 是全局行号；命中视图下是命中索引，行号显示为 `<文件序>.<行号>`。
fn row_content(state: &AppState, row: usize, in_result: bool) -> (Cow<'_, str>, String) {
    if in_result {
        let hit = state.search_results[row];
        let line = state
            .fileset
            .file(hit.file_idx as usize)
            .and_then(|f| f.line(hit.line_idx as usize))
            .unwrap_or_default();
        (line, format!("{}.{}", hit.file_idx + 1, hit.line_idx + 1))
    } else {
        let line = state.fileset.line(row).unwrap_or_default();
        (line, (row + 1).to_string())
    }
}

/// 绘制一行的背景、行号槽与行号。必须在正文 widget 之前调用（Painter 按调用顺序叠放）。
fn paint_row_bg(
    ui: &mut egui::Ui,
    row_rect: egui::Rect,
    gutter_w: f32,
    gutter_text: &str,
    selected: bool,
    p: &Palette,
) {
    if selected {
        ui.painter().rect_filled(row_rect, 0.0, p.row_active);
    } else if ui.rect_contains_pointer(row_rect) {
        ui.painter().rect_filled(row_rect, 0.0, p.row_hover);
    }

    // 行号槽背景 + 与正文之间的竖线
    let gutter_rect =
        egui::Rect::from_min_size(row_rect.min, egui::vec2(gutter_w, row_rect.height()));
    ui.painter().rect_filled(gutter_rect, 0.0, p.gutter);
    let x = row_rect.min.x + gutter_w;
    ui.painter()
        .vline(x, row_rect.y_range(), egui::Stroke::new(1.0, p.gutter_line));

    // 行号：右对齐到行号槽内边距，随行高垂直居中
    ui.painter().text(
        egui::pos2(x - GUTTER_PAD, row_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        gutter_text,
        egui::FontId::monospace(theme::GUTTER_FONT_SIZE),
        if selected { p.text } else { p.text_dim },
    );
}

/// 把一行的着色分段打包成单个 `LayoutJob`，供一个 `Label` 渲染整行。
fn build_job(line: &str, hl: &Highlighter, p: &Palette, wrap_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let font = egui::FontId::monospace(theme::LOG_FONT_SIZE);
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

/// 估算最长行的渲染宽度（像素），用于固定横向滚动范围。
///
/// 只采样前 [`SAMPLE_LINES`] 行：对上亿行的文件全量扫描不可接受，而日志的行宽分布
/// 通常稳定。取最大值而非平均值——横向滚动必须能覆盖最长的那一行。
///
/// CJK 字形约为等宽拉丁字符的两倍宽，因此按「ASCII 记 1、非 ASCII 记 1.7」加权。
pub fn estimate_content_width(fileset: &crate::core::indexer::FileSet) -> f32 {
    let lines = fileset.total_lines().min(SAMPLE_LINES);
    let mut max = 0.0_f32;
    for i in 0..lines {
        if let Some(line) = fileset.line(i) {
            let w = estimate_text_width(&line);
            if w > max {
                max = w;
            }
        }
    }
    max
}

/// 按字符类别加权的宽度估算（单位：像素）。
fn estimate_text_width(line: &str) -> f32 {
    const ASCII_W: f32 = 7.2; // 12.5px 等宽拉丁字符的实测字宽
    const WIDE_W: f32 = 12.2; // CJK 等全角字符约为一个 em
    let mut w = 0.0_f32;
    for c in line.chars() {
        w += if c.is_ascii() { ASCII_W } else { WIDE_W };
    }
    w
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
        let ascii = estimate_text_width("aaaa");
        let cjk = estimate_text_width("中中中中");
        assert!(cjk > ascii * 1.5, "ascii={ascii}, cjk={cjk}");
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
}
