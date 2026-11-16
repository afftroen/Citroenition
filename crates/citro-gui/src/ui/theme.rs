//! Colours, fonts and spacing. One neutral palette per theme and a single accent colour; status colours are the
//! only other hues (green open, amber busy, red error, purple locked on another device).

use crate::settings::Theme;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, ThemePreference, Visuals, vec2,
};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Page background.
    pub bg: Color32,
    pub sidebar: Color32,
    /// Cards, dialogs, table stripes.
    pub surface: Color32,
    /// Hover / header / inactive buttons.
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub locked: Color32,
    pub idle: Color32,
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => Palette {
            dark: true,
            bg: rgb(0x14161b),
            sidebar: rgb(0x0f1115),
            surface: rgb(0x1b1e25),
            raised: rgb(0x252932),
            border: rgb(0x2d323d),
            text: rgb(0xe4e7ec),
            dim: rgb(0x8f97a6),
            accent: rgb(0x5aa0ff),
            accent_soft: Color32::from_rgba_unmultiplied(0x5a, 0xa0, 0xff, 46),
            on_accent: rgb(0x08111f),
            ok: rgb(0x3fb950),
            warn: rgb(0xe8a317),
            err: rgb(0xf26b63),
            locked: rgb(0xb28cf7),
            idle: rgb(0x6f7785),
        },
        Theme::Light => Palette {
            dark: false,
            bg: rgb(0xf3f4f7),
            sidebar: rgb(0xe8eaef),
            surface: rgb(0xffffff),
            raised: rgb(0xeceef3),
            border: rgb(0xd3d7df),
            text: rgb(0x1b1f27),
            dim: rgb(0x626b7a),
            accent: rgb(0x2563eb),
            accent_soft: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 36),
            on_accent: rgb(0xffffff),
            ok: rgb(0x1a7f37),
            warn: rgb(0xb06a00),
            err: rgb(0xc62828),
            locked: rgb(0x7c4dcc),
            idle: rgb(0x8a93a2),
        },
    }
}

/// The bundled Ubuntu face lacks the geometric shapes and arrows; Hack has them, so it is the proportional fallback.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if fonts.font_data.contains_key("Hack")
        && let Some(family) = fonts.families.get_mut(&FontFamily::Proportional)
    {
        family.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn shadow(alpha: u8, blur: u8) -> Shadow {
    Shadow { offset: [0, 6], blur, spread: 0, color: Color32::from_black_alpha(alpha) }
}

pub fn apply(ctx: &egui::Context, theme: Theme) {
    let p = palette(theme);
    let (egui_theme, pref) = match theme {
        Theme::Dark => (egui::Theme::Dark, ThemePreference::Dark),
        Theme::Light => (egui::Theme::Light, ThemePreference::Light),
    };
    ctx.set_theme(pref);

    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = if p.dark { rgb(0x101216) } else { rgb(0xffffff) };
    v.faint_bg_color = if p.dark { rgb(0x181b21) } else { rgb(0xf7f8fa) };
    v.code_bg_color = p.raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = shadow(if p.dark { 120 } else { 50 }, 28);
    v.popup_shadow = shadow(if p.dark { 110 } else { 45 }, 18);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    let lift = |c: Color32| if p.dark { c.gamma_multiply(1.0).linear_multiply(1.0) } else { c };
    let hover = if p.dark { rgb(0x2e3340) } else { rgb(0xe0e4ec) };
    w.hovered.bg_fill = lift(hover);
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.8));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    let active = if p.dark { rgb(0x38404f) } else { rgb(0xd2d8e3) };
    w.active.bg_fill = active;
    w.active.weak_bg_fill = active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.open.bg_fill = hover;
    w.open.weak_bg_fill = hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = radius;
    }
    ctx.set_visuals_of(egui_theme, v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(11.0, 5.0);
        s.spacing.interact_size = vec2(40.0, 28.0);
        s.spacing.window_margin = Margin::same(16);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.combo_width = 120.0;
        s.spacing.scroll.floating = false;
        s.spacing.scroll.bar_width = 9.0;
        s.interaction.selectable_labels = false;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}
