//! Colours, fonts and spacing. The look is a near-black window, a slightly lighter sidebar and one big rounded content
//! card; neutral greys, white for emphasis and a single vivid blue accent. The status colours are the only other hues
//! (green open, amber busy, red error, purple locked on another device).

use crate::settings::Theme;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, ThemePreference,
    Visuals, vec2,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// The window behind everything: the gutter around the content card and the top bar.
    pub bg: Color32,
    pub sidebar: Color32,
    /// The content card.
    pub surface: Color32,
    /// Table header band, alternate rows.
    pub raised: Color32,
    /// Nested cards, dialogs, menus, the account card in the sidebar.
    pub panel: Color32,
    /// Hovered rows and controls.
    pub hover: Color32,
    /// The selected sidebar entry and pressed controls.
    pub active: Color32,
    /// Hairlines and quiet outlines.
    pub border: Color32,
    /// Outlines of inputs and hovered buttons.
    pub border_strong: Color32,
    /// Emphasis: titles, the active entry, the primary value of a row.
    pub strong: Color32,
    /// Body text.
    pub text: Color32,
    /// Secondary text: column headers, hints, labels.
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    /// The primary button while it cannot be used.
    pub accent_off: Color32,
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

impl Palette {
    /// The outline of a hovered button.
    pub fn hover_border(&self) -> Color32 {
        if self.dark { rgb(0x6a6a6a) } else { rgb(0xa5adbb) }
    }
}

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => Palette {
            dark: true,
            bg: rgb(0x0d0d0d),
            sidebar: rgb(0x1a1a1a),
            surface: rgb(0x212121),
            raised: rgb(0x252525),
            panel: rgb(0x292929),
            hover: rgb(0x2f2f2f),
            active: rgb(0x3c3c3c),
            border: rgb(0x333333),
            border_strong: rgb(0x494949),
            strong: rgb(0xffffff),
            text: rgb(0xadadad),
            dim: rgb(0x868686),
            accent: rgb(0x2b53f8),
            accent_hover: rgb(0x4a6dff),
            accent_soft: Color32::from_rgba_unmultiplied(0x2b, 0x53, 0xf8, 52),
            accent_off: rgb(0x263a8c),
            on_accent: Color32::WHITE,
            ok: rgb(0x3fb950),
            warn: rgb(0xe8a317),
            err: rgb(0xf26b63),
            locked: rgb(0xb28cf7),
            idle: rgb(0x6b6b6b),
        },
        Theme::Light => Palette {
            dark: false,
            bg: rgb(0xe9ebf0),
            sidebar: rgb(0xf7f8fa),
            surface: rgb(0xffffff),
            raised: rgb(0xf4f5f8),
            panel: rgb(0xf6f7f9),
            hover: rgb(0xeceff4),
            active: rgb(0xe1e5ed),
            border: rgb(0xe3e6ec),
            border_strong: rgb(0xc9cfda),
            strong: rgb(0x0f1115),
            text: rgb(0x40454f),
            dim: rgb(0x7a808f),
            accent: rgb(0x2b53f8),
            accent_hover: rgb(0x1f43d8),
            accent_soft: Color32::from_rgba_unmultiplied(0x2b, 0x53, 0xf8, 34),
            accent_off: rgb(0xa9b9fb),
            on_accent: Color32::WHITE,
            ok: rgb(0x1a7f37),
            warn: rgb(0xb06a00),
            err: rgb(0xc62828),
            locked: rgb(0x7c4dcc),
            idle: rgb(0x9aa1ad),
        },
    }
}

const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// The family of the heavier face; `widgets::semibold` and `semibold_font` use it.
pub fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// The middle weight: names, the active tab and the active nav item.
pub fn medium_family() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Inter (a subset: Latin, punctuation, arrows) is the face of the whole UI; the bundled egui faces stay behind it as
/// fallbacks. Ubuntu lacks the geometric shapes and arrows that Hack has, so Hack is the last proportional fallback.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("Inter".to_owned(), Arc::new(FontData::from_static(INTER_REGULAR)));
    fonts.font_data.insert("InterMedium".to_owned(), Arc::new(FontData::from_static(INTER_MEDIUM)));
    fonts.font_data.insert("InterSemiBold".to_owned(), Arc::new(FontData::from_static(INTER_SEMIBOLD)));
    let mut fallbacks: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if fonts.font_data.contains_key("Hack") && !fallbacks.iter().any(|f| f == "Hack") {
        fallbacks.push("Hack".to_owned());
    }
    let chain = |first: &str| -> Vec<String> {
        let mut v = vec![first.to_owned()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, chain("Inter"));
    fonts.families.insert(medium_family(), chain("InterMedium"));
    fonts.families.insert(semibold_family(), chain("InterSemiBold"));
    ctx.set_fonts(fonts);
}

fn shadow(alpha: u8, blur: u8) -> Shadow {
    Shadow { offset: [0, 8], blur, spread: 0, color: Color32::from_black_alpha(alpha) }
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
    v.window_fill = p.panel;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = shadow(if p.dark { 150 } else { 50 }, 32);
    v.popup_shadow = shadow(if p.dark { 140 } else { 45 }, 20);
    v.hyperlink_color = p.accent_hover;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.override_text_color = None;

    let radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    // buttons are outlines on the card: no fill until hovered
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.hover_border());
    w.hovered.fg_stroke = Stroke::new(1.0, p.strong);
    w.active.bg_fill = p.active;
    w.active.weak_bg_fill = p.active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.strong);
    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.strong);
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = radius;
        wv.expansion = 0.0;
    }
    ctx.set_visuals_of(egui_theme, v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size = vec2(40.0, 32.0);
        s.spacing.window_margin = Margin::same(22);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.combo_width = 120.0;
        s.spacing.icon_width = 16.0;
        s.spacing.icon_width_inner = 10.0;
        s.spacing.icon_spacing = 8.0;
        // thin floating bars that only show while scrolling or hovered, in the muted text colour
        s.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 8.0,
            floating_width: 3.0,
            floating_allocated_width: 0.0,
            foreground_color: true,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.12,
            dormant_handle_opacity: 0.16,
            active_handle_opacity: 0.35,
            interact_handle_opacity: 0.6,
            ..egui::style::ScrollStyle::floating()
        };
        s.interaction.selectable_labels = false;
        s.interaction.tooltip_delay = 0.2;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}
