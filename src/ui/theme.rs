//! 编辑器风格主题（spec §7.7 / Q7）。
//!
//! 目标观感：像 VS Code 那样的日志编辑器——低饱和背景、无描边的静态文本、
//! 紧凑的控件与行高、独立的行号槽、语义化的级别着色。
//!
//! egui 0.36 同时维护 `dark_style` / `light_style` 两套 `Style`，由
//! `Options::theme_preference` 决定当前使用哪一套。这里两套都覆盖，
//! 并把默认固定为暗色（工具栏 ☀/🌙 可切换）。
//!
//! 调色板同时供日志区自绘使用（行号槽背景、行高亮、时间戳/级别/命中着色），
//! 因为那部分用 `Painter` 与 `LayoutJob` 直接绘制，读不到 `egui::Style`。

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Stroke, TextStyle,
};

/// 日志正文（含行号）的字号。编辑器正文通常 12–13 px，这里取中间值。
pub const LOG_FONT_SIZE: f32 = 12.5;
/// 行号字号：略小于正文，弱化其视觉权重，避免与日志内容争夺注意力。
pub const GUTTER_FONT_SIZE: f32 = 11.0;

/// 一套主题的全部色值。既驱动 `egui::Style`，也驱动日志区自绘。
pub struct Palette {
    /// 中央区（日志正文）背景。
    pub bg: Color32,
    /// 顶栏 / 底栏 / 菜单背景。
    pub panel: Color32,
    /// "下沉"元素背景：输入框、下拉框。
    pub surface: Color32,
    /// 行号槽背景。
    pub gutter: Color32,
    /// 行号槽与正文之间的分隔线。
    pub gutter_line: Color32,
    /// 指针所在行背景。
    pub row_hover: Color32,
    /// 选中行背景。
    pub row_active: Color32,
    /// 控件描边与分隔线。
    pub border: Color32,
    /// 控件 hover 背景（比 `surface` 亮一档）。
    pub control_hover: Color32,
    /// 正文。
    pub text: Color32,
    /// 强调文本：应用名、目录树根节点等比正文更醒目一档的文字。
    ///
    /// 注意：**不要**用 `RichText::strong()` 取强调色——egui 会把它解析为
    /// `Visuals::strong_text_color()`，而后者等于 `widgets.active.text_color()`。
    /// 本主题把 active 的前景色固定为白色（accent 蓝底按钮需要白字），
    /// 于是 `.strong()` 在亮色主题下会渲染成白字白底、完全不可见。
    /// 因此强调色一律从 `Palette` 显式取值，按主题分档。
    pub text_strong: Color32,
    /// 次要文本：行号、状态栏信息。
    pub text_dim: Color32,
    /// 强调色（焦点环、进度条、链接）。
    pub accent: Color32,
    /// 文本选区。
    pub selection: Color32,

    // —— 日志语义色（§7.6 的级别配色在此按主题分套）——
    /// 时间戳：弱化，属于"结构"而非"内容"。
    pub timestamp: Color32,
    pub level_error: Color32,
    pub level_warn: Color32,
    pub level_info: Color32,
    pub level_debug: Color32,
    pub level_trace: Color32,
    /// 检索命中背景（荧光笔效果；文字保持正常色，确保高亮后内容仍清晰可读）。
    pub hit_bg: Color32,
}

/// 暗色（默认）：参照 VS Code Dark+。
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x1E, 0x1E, 0x1E),
    panel: Color32::from_rgb(0x25, 0x25, 0x26),
    surface: Color32::from_rgb(0x33, 0x33, 0x33),
    gutter: Color32::from_rgb(0x1B, 0x1B, 0x1B),
    gutter_line: Color32::from_rgb(0x2E, 0x2E, 0x2E),
    row_hover: Color32::from_rgb(0x28, 0x2B, 0x2C),
    row_active: Color32::from_rgb(0x33, 0x38, 0x3D),
    border: Color32::from_rgb(0x3C, 0x3C, 0x3C),
    control_hover: Color32::from_rgb(0x3F, 0x43, 0x47),
    text: Color32::from_rgb(0xD4, 0xD4, 0xD4),
    // 暗色主题：正文浅灰 → 强调取纯白，一档更亮。
    text_strong: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    text_dim: Color32::from_rgb(0x85, 0x85, 0x85),
    accent: Color32::from_rgb(0x0E, 0x63, 0x9C),
    selection: Color32::from_rgb(0x26, 0x4F, 0x78),

    // 语义色须同时能在「普通背景 / 行悬停 / 选中行 / 文字选区」四种底色上读清：
    // 双击会触发 egui 的文字选区（铺 `selection` 底色），此前时间戳在其上仅 1.88:1 近乎隐形。
    // 下列取值经 `log_text_colors_stay_readable` 用例校验，四种底色下均 ≥ 3.0:1。
    timestamp: Color32::from_rgb(0x9C, 0x9C, 0x9C),
    level_error: Color32::from_rgb(0xF4, 0x73, 0x73),
    level_warn: Color32::from_rgb(0xD7, 0xBA, 0x7D),
    level_info: Color32::from_rgb(0x4F, 0xC1, 0xFF),
    level_debug: Color32::from_rgb(0x82, 0xAC, 0x6E),
    level_trace: Color32::from_rgb(0x9C, 0x9C, 0x9C),
    // 暗色主题命中高亮：清晰的琥珀底色，文字沿用正常浅灰，对比充足。
    hit_bg: Color32::from_rgb(0x53, 0x49, 0x12),
};

/// 亮色：参照 VS Code Light+。
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    panel: Color32::from_rgb(0xF3, 0xF3, 0xF3),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    gutter: Color32::from_rgb(0xFA, 0xFA, 0xFA),
    gutter_line: Color32::from_rgb(0xE5, 0xE5, 0xE5),
    row_hover: Color32::from_rgb(0xEC, 0xEC, 0xEC),
    row_active: Color32::from_rgb(0xDA, 0xDE, 0xE4),
    border: Color32::from_rgb(0xCE, 0xCE, 0xCE),
    control_hover: Color32::from_rgb(0xE8, 0xE8, 0xE8),
    text: Color32::from_rgb(0x33, 0x33, 0x33),
    // 亮色主题：正文深灰 → 强调取近黑，一档更暗（注意不能沿用 active 的白色）。
    text_strong: Color32::from_rgb(0x1A, 0x1A, 0x1A),
    text_dim: Color32::from_rgb(0x99, 0x99, 0x99),
    accent: Color32::from_rgb(0x00, 0x7A, 0xCC),
    selection: Color32::from_rgb(0xAD, 0xD6, 0xFF),

    // 同暗色：四种底色下均需 ≥ 3.0:1（见 `log_text_colors_stay_readable`）。
    timestamp: Color32::from_rgb(0x74, 0x74, 0x74),
    level_error: Color32::from_rgb(0xC5, 0x1E, 0x1E),
    level_warn: Color32::from_rgb(0x96, 0x6C, 0x00),
    level_info: Color32::from_rgb(0x00, 0x5F, 0xA8),
    level_debug: Color32::from_rgb(0x23, 0x79, 0x93),
    level_trace: Color32::from_rgb(0x6E, 0x6E, 0x6E),
    // 亮色主题命中高亮：柔和的黄色荧光笔，文字沿用正常深灰，对比充足。
    hit_bg: Color32::from_rgb(0xFF, 0xE8, 0x80),
};

/// 取当前主题对应的调色板（日志区自绘用）。
pub fn palette(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

/// 安装两套 `Style` 并按 `theme` 固定当前使用的那一套。
///
/// egui 0.36 的 `theme_preference` 默认是 `System`（跟随系统），这里显式固定为给定主题；
/// `sync_window_theme` 默认为 true，会一并同步 macOS 原生窗口标题栏。
pub fn apply(ctx: &egui::Context, theme: egui::Theme) {
    ctx.options_mut(|opt| {
        opt.dark_style = Arc::new(style_for(&DARK, true));
        opt.light_style = Arc::new(style_for(&LIGHT, false));
    });
    ctx.set_theme(theme);
}

/// 核心层主题偏好（`core::prefs::ThemePref`，与 egui 解耦）↔ `egui::Theme`。
impl From<crate::core::prefs::ThemePref> for egui::Theme {
    fn from(t: crate::core::prefs::ThemePref) -> Self {
        match t {
            crate::core::prefs::ThemePref::Dark => egui::Theme::Dark,
            crate::core::prefs::ThemePref::Light => egui::Theme::Light,
        }
    }
}

impl From<egui::Theme> for crate::core::prefs::ThemePref {
    fn from(t: egui::Theme) -> Self {
        match t {
            egui::Theme::Dark => crate::core::prefs::ThemePref::Dark,
            egui::Theme::Light => crate::core::prefs::ThemePref::Light,
        }
    }
}

/// 由调色板生成一套完整的 `egui::Style`。
fn style_for(p: &Palette, dark: bool) -> egui::Style {
    let mut style = egui::Style {
        text_styles: text_styles(),
        spacing: spacing(),
        // 日志区默认不换行（长行横向滚动，spec G7）；「折行」开关在日志区局部覆盖。
        wrap_mode: Some(egui::TextWrapMode::Extend),
        ..Default::default()
    };

    let v = &mut style.visuals;
    *v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.dark_mode = dark;
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.text_dim);
    v.panel_fill = p.panel;
    v.window_fill = p.bg;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.row_hover;
    v.text_edit_bg_color = Some(p.surface);
    v.code_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.level_warn;
    v.error_fg_color = p.level_error;
    v.selection = egui::style::Selection {
        bg_fill: p.selection,
        stroke: Stroke::new(1.0, p.accent),
    };
    // 编辑器没有圆角窗口与阴影，只有菜单保留少量圆角。
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::same(4);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    v.interact_cursor = Some(CursorIcon::PointingHand);

    // 静态文本（日志行、标签）：无背景无描边，直接铺在主题背景上。
    v.widgets.noninteractive.bg_fill = p.bg;
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);

    // 可交互控件：扁平化，靠背景深浅而非描边区分状态（编辑器工具栏观感）。
    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);

    v.widgets.hovered.bg_fill = p.control_hover;
    v.widgets.hovered.weak_bg_fill = p.control_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);

    v.widgets.active.bg_fill = p.accent;
    v.widgets.active.weak_bg_fill = p.accent;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    // 两套主题下 accent 都是中等亮度的蓝，白字在两者上都可读。
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    v.widgets.open.bg_fill = p.surface;
    v.widgets.open.weak_bg_fill = p.surface;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(3);
        // expansion 会让控件绘制越界，行内小控件一律取消。
        w.expansion = 0.0;
    }

    style
}

/// 字号阶梯：正文 12.5、按钮 12、行号/次要信息 11、等宽 12.5。
fn text_styles() -> BTreeMap<TextStyle, FontId> {
    [
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(LOG_FONT_SIZE, FontFamily::Monospace),
        ),
    ]
    .into_iter()
    .collect()
}

/// 紧凑间距：编辑器顶栏/底栏比常规 GUI 更薄，控件之间留白更小。
fn spacing() -> egui::style::Spacing {
    egui::style::Spacing {
        item_spacing: egui::vec2(6.0, 4.0),
        button_padding: egui::vec2(8.0, 3.0),
        window_margin: egui::Margin::symmetric(8, 6),
        menu_margin: egui::Margin::symmetric(6, 4),
        // 按钮高度 22px，兼顾密度与可点击性
        interact_size: egui::vec2(40.0, 22.0),
        combo_width: 72.0,
        text_edit_width: 200.0,
        extra_text_line_spacing: 0.0,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.1 相对亮度。
    fn luminance(c: Color32) -> f32 {
        let chan = |v: u8| {
            let s = f32::from(v) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * chan(c.r()) + 0.7152 * chan(c.g()) + 0.0722 * chan(c.b())
    }

    /// WCAG 对比度（1.0 ~ 21.0）。
    fn contrast(fg: Color32, bg: Color32) -> f32 {
        let (a, b) = (luminance(fg), luminance(bg));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// 正文与强调文字在顶栏背景上必须清晰可读（≥ 4.5:1，WCAG AA）。
    ///
    /// 回归守卫：应用名曾用 `RichText::strong()`，而 egui 把它解析为
    /// `widgets.active` 的**白色**前景；暗色顶栏上尚可读，亮色顶栏（近白）上则
    /// 白字白底完全不可见。故强调色改由 `Palette::text_strong` 显式提供，并用本
    /// 用例锁住「两套主题下前景与背景都有足够对比」这一不变量。
    #[test]
    fn foreground_colors_are_readable_on_panel() {
        for p in [&DARK, &LIGHT] {
            for (name, fg, min) in [
                ("text", p.text, 4.5),
                ("text_strong", p.text_strong, 4.5),
                // 次要文本（行号/状态栏）本就刻意弱化，只做「不能退化到看不见」的下限守卫。
                ("text_dim", p.text_dim, 2.0),
            ] {
                let ratio = contrast(fg, p.panel);
                assert!(
                    ratio >= min,
                    "{name} 在顶栏背景上对比度仅 {ratio:.2}:1，低于下限 {min}:1"
                );
            }
        }
    }

    /// 强调色必须与正文字色不同，否则「强调」失去意义（且难以肉眼验证）。
    #[test]
    fn text_strong_differs_from_text() {
        assert_ne!(DARK.text_strong, DARK.text);
        assert_ne!(LIGHT.text_strong, LIGHT.text);
    }

    /// 日志正文的所有语义色，在四种行底色上都必须保持可读。
    ///
    /// 回归守卫：日志行的文字色（正文/时间戳/各级级别）会铺在四种底色之上——
    /// 普通背景 `bg`、悬停 `row_hover`、**选中行 `row_active`**、以及双击触发的
    /// **文字选区 `selection`**（egui 的 `Label::selectable` 双击选词后铺 `selection` 底色）。
    /// 此前时间戳在亮色选区上仅 1.88:1、暗色选区上 2.15:1，双击后几乎看不见。
    /// 这里锁住：正文 ≥ 4.5:1，语义色（刻意弱化的时间戳/级别）≥ 3.0:1。
    #[test]
    fn log_text_colors_stay_readable() {
        for p in [&DARK, &LIGHT] {
            let backgrounds = [
                ("bg", p.bg),
                ("row_hover", p.row_hover),
                ("row_active", p.row_active),
                ("selection", p.selection),
            ];
            // （名称，前景色，对比度下限）
            let foregrounds: [(&str, Color32, f32); 7] = [
                ("text", p.text, 4.5),
                ("timestamp", p.timestamp, 3.0),
                ("level_error", p.level_error, 3.0),
                ("level_warn", p.level_warn, 3.0),
                ("level_info", p.level_info, 3.0),
                ("level_debug", p.level_debug, 3.0),
                ("level_trace", p.level_trace, 3.0),
            ];
            for (fg_name, fg, min) in foregrounds {
                for (bg_name, bg) in backgrounds {
                    let ratio = contrast(fg, bg);
                    assert!(
                        ratio >= min,
                        "{fg_name} 在 {bg_name} 上对比度仅 {ratio:.2}:1，低于下限 {min}:1"
                    );
                }
            }
        }
    }

    /// 命中高亮背景上的正文必须可读（`Segment::Hit` 只叠底色、不改文字色）。
    #[test]
    fn hit_highlight_background_keeps_text_readable() {
        for p in [&DARK, &LIGHT] {
            let ratio = contrast(p.text, p.hit_bg);
            assert!(
                ratio >= 4.5,
                "正文在命中高亮底色上对比度仅 {ratio:.2}:1，低于下限 4.5:1"
            );
        }
    }
}
