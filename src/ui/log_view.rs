use std::borrow::Cow;

use eframe::egui;

use crate::app::AppState;
use crate::highlight::{Level, Segment};

/// 每行渲染高度（像素，不含间距）。固定行高使虚拟滚动可 O(1) 定位（spec §7.2）。
const ROW_HEIGHT: f32 = 16.0;
/// 单行渲染字符上限：超过则截断，避免 egui 对 1 MB 单行做字形布局而卡死（spec A9/R3）。
const MAX_RENDER_CHARS: usize = 100_000;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    let total = state.fileset.total_lines();
    if total == 0 {
        ui.centered_and_justified(|ui| {
            ui.label("尚未打开任何日志文件（点击左上角「打开」）");
        });
        return;
    }

    // 折行开关：默认关闭，长行横向滚动。开启时启用 wrap，长行被裁剪到单行高度（MVP 限制）。
    if state.wrap {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    }

    let hl = &state.highlighter;

    egui::ScrollArea::both().auto_shrink([false; 2]).show_rows(
        ui,
        ROW_HEIGHT,
        total,
        |ui, range| {
            for row in range {
                let line: Cow<'_, str> = state.fileset.line(row).unwrap_or_default();
                let text = truncate_for_render(&line);

                ui.horizontal(|ui| {
                    // 行号列：等宽、右对齐、弱化显示
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{:>6}", row + 1))
                                .monospace()
                                .weak(),
                        )
                        .selectable(false),
                    );
                    ui.separator();
                    // 仅对可视区约 60 行做着色（spec A8），复用已编译的 level_re
                    render_line(ui, &text, hl);
                });
            }
        },
    );
}

/// 渲染一行：按分段着色（级别 / 命中 / 普通）。
fn render_line(ui: &mut egui::Ui, line: &str, hl: &crate::highlight::Highlighter) {
    for seg in crate::highlight::segments(line, hl) {
        match seg {
            Segment::Plain(t) => {
                ui.label(egui::RichText::new(t).monospace());
            }
            Segment::Level(t, lvl) => {
                ui.label(egui::RichText::new(t).monospace().color(level_color(lvl)));
            }
            Segment::Hit(t) => {
                ui.label(
                    egui::RichText::new(t)
                        .monospace()
                        .color(egui::Color32::YELLOW),
                );
            }
        }
    }
}

/// 级别 → 颜色。错误/致命为红，警告为橙，Info 默认，Debug 灰蓝，Trace/Verbose 更弱。
fn level_color(lvl: Level) -> egui::Color32 {
    match lvl {
        Level::Fatal | Level::Error => egui::Color32::from_rgb(0xE5, 0x4A, 0x4A),
        Level::Warn => egui::Color32::from_rgb(0xE0, 0x9F, 0x3E),
        Level::Info => egui::Color32::from_rgb(0x4A, 0x9E, 0x4A),
        Level::Debug => egui::Color32::from_rgb(0x6B, 0x8A, 0xB0),
        Level::Trace | Level::Verbose => egui::Color32::from_rgb(0x8A, 0x8A, 0x8A),
    }
}

/// 超过上限的字符截断，附加提示后缀，避免极端长行拖垮渲染。
fn truncate_for_render(line: &str) -> Cow<'_, str> {
    if line.chars().count() <= MAX_RENDER_CHARS {
        Cow::Borrowed(line)
    } else {
        let truncated: String = line.chars().take(MAX_RENDER_CHARS).collect();
        Cow::Owned(format!("{truncated} …(已截断，完整内容见导出)"))
    }
}
