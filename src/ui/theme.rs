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
    /// 检索命中背景（荧光笔效果）。
    pub hit_bg: Color32,
    /// 检索命中文字。
    pub hit_fg: Color32,
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
    text_dim: Color32::from_rgb(0x85, 0x85, 0x85),
    accent: Color32::from_rgb(0x0E, 0x63, 0x9C),
    selection: Color32::from_rgb(0x26, 0x4F, 0x78),

    timestamp: Color32::from_rgb(0x80, 0x80, 0x80),
    level_error: Color32::from_rgb(0xF1, 0x4C, 0x4C),
    level_warn: Color32::from_rgb(0xD7, 0xBA, 0x7D),
    level_info: Color32::from_rgb(0x4F, 0xC1, 0xFF),
    level_debug: Color32::from_rgb(0x6A, 0x99, 0x55),
    level_trace: Color32::from_rgb(0x80, 0x80, 0x80),
    hit_bg: Color32::from_rgb(0x5C, 0x4A, 0x16),
    hit_fg: Color32::from_rgb(0xFF, 0xD8, 0x66),
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
    text_dim: Color32::from_rgb(0x99, 0x99, 0x99),
    accent: Color32::from_rgb(0x00, 0x7A, 0xCC),
    selection: Color32::from_rgb(0xAD, 0xD6, 0xFF),

    timestamp: Color32::from_rgb(0x99, 0x99, 0x99),
    level_error: Color32::from_rgb(0xC5, 0x1E, 0x1E),
    level_warn: Color32::from_rgb(0xA0, 0x73, 0x00),
    level_info: Color32::from_rgb(0x00, 0x5F, 0xA8),
    level_debug: Color32::from_rgb(0x23, 0x79, 0x93),
    level_trace: Color32::from_rgb(0x76, 0x76, 0x76),
    hit_bg: Color32::from_rgb(0xFF, 0xE0, 0x80),
    hit_fg: Color32::from_rgb(0x3A, 0x2B, 0x00),
};

/// 取当前主题对应的调色板（日志区自绘用）。
pub fn palette(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

/// 安装两套 `Style` 并把默认主题固定为暗色。
///
/// egui 0.36 的 `theme_preference` 默认是 `System`（跟随系统），这里显式固定为暗色；
/// `sync_window_theme` 默认为 true，会一并同步 macOS 原生窗口标题栏。
pub fn apply(ctx: &egui::Context) {
    ctx.options_mut(|opt| {
        opt.dark_style = Arc::new(style_for(&DARK, true));
        opt.light_style = Arc::new(style_for(&LIGHT, false));
    });
    ctx.set_theme(egui::Theme::Dark);
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
