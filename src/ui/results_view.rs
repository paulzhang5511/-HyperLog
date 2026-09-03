//! 独立结果页（"查找全部"）：展示目录检索的命中结果。
//!
//! 与日志正文视图（[`crate::ui::log_view`]）分离：顶部是结果页头（命中计数 +
//! 保存/复制全部/返回日志），下方是虚拟滚动的命中列表，每行「文件路径:行号 + 内容」。
//!
//! 结果已内联行文本（[`crate::core::grepdir::GrepHit`]），无需回查临时索引。

use eframe::egui;

use crate::app::AppState;
use crate::ui::theme::{self, Palette};

/// 命中行高（结果页同样固定行高以支持虚拟滚动）。
const ROW_HEIGHT: f32 = 18.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let p = theme::palette(ui.ctx());
    let hits = &state.grep_hits;

    // —— 结果页头 ——
    ui.horizontal(|ui| {
        ui.label(
            // 显式取 Palette 颜色，不用 `.strong()`（会被解析成 active 前景白色，亮色主题不可见）。
            egui::RichText::new(format!("{} 条命中", crate::util::group_digits(hits.len())))
                .color(p.text),
        );
        if state.grep_truncated {
            ui.colored_label(p.level_warn, "（已截断）");
        }
        ui.separator();

        if ui.button("保存结果…").clicked() {
            state.pending_grep_save = true;
        }
        if ui
            .add_enabled(!hits.is_empty(), egui::Button::new("复制全部"))
            .clicked()
        {
            let text = hits_to_text(hits);
            ui.ctx().copy_text(text);
        }
        if ui.button("返回日志").clicked() {
            state.show_results = false;
        }
    });
    ui.separator();

    if hits.is_empty() {
        empty_result_hint(ui, p);
        return;
    }

    // —— 命中列表（虚拟滚动）——
    let total = hits.len();
    let mut copied: Option<String> = None;

    egui::ScrollArea::both().auto_shrink([false; 2]).show_rows(
        ui,
        ROW_HEIGHT,
        total,
        |ui, range| {
            for row in range {
                let hit = &hits[row];
                let selected = state.grep_selected_row == Some(row);

                let row_rect = egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(ui.available_width(), ROW_HEIGHT),
                );

                // 行背景
                if selected {
                    ui.painter().rect_filled(row_rect, 0.0, p.row_active);
                } else if ui.rect_contains_pointer(row_rect) {
                    ui.painter().rect_filled(row_rect, 0.0, p.row_hover);
                }

                // 单 Label 承载「路径:行号 + 内容」，路径用主题色、行号用 dim
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = f32::INFINITY;
                let mono = egui::FontId::monospace(theme::LOG_FONT_SIZE);
                job.append(
                    &format!("{}:{}", hit.display_path, hit.line_number),
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: p.accent,
                        ..Default::default()
                    },
                );
                job.append(
                    "  ",
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: p.text_dim,
                        ..Default::default()
                    },
                );
                job.append(
                    &hit.line,
                    0.0,
                    egui::TextFormat {
                        font_id: mono,
                        color: p.text,
                        ..Default::default()
                    },
                );

                let resp = ui.add(egui::Label::new(job).selectable(true));
                if resp.clicked() {
                    state.grep_selected_row = Some(row);
                    // 单击命中 → 跳转原文对应行（notepad++ 风格）：打开目标文件并定位。
                    state.pending_grep_jump = Some((hit.abs_path.clone(), hit.line_number));
                }
                resp.context_menu(|ui| {
                    if ui.button("复制此行").clicked() {
                        copied = Some(hit.line.clone());
                        ui.close();
                    }
                    if ui.button("跳转到原文").clicked() {
                        state.pending_grep_jump = Some((hit.abs_path.clone(), hit.line_number));
                        ui.close();
                    }
                });
            }
        },
    );

    if let Some(t) = copied {
        ui.ctx().copy_text(t);
    }

    // ⌘C / Ctrl+C 复制选中行
    let copy_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);
    if ui.input_mut(|i| i.consume_shortcut(&copy_shortcut))
        && let Some(row) = state.grep_selected_row
        && let Some(hit) = hits.get(row)
    {
        ui.ctx().copy_text(hit.line.clone());
    }
}

/// 命中列表 → 纯文本（复制全部 / 保存），每行「路径:行号: 内容」。
pub fn hits_to_text(hits: &[crate::core::grepdir::GrepHit]) -> String {
    let mut s = String::with_capacity(hits.len() * 80);
    for h in hits {
        s.push_str(&h.display_path);
        s.push(':');
        s.push_str(&h.line_number.to_string());
        s.push_str(": ");
        s.push_str(&h.line);
        s.push('\n');
    }
    s
}

/// 结果页空态提示。
fn empty_result_hint(ui: &mut egui::Ui, p: &Palette) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new("没有命中").size(14.0).color(p.text_dim));
    });
}
