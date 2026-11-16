//! Small shared widgets: cards, buttons, badges, empty states, modal frames.

use super::theme::Palette;
use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, Frame, Layout, Margin, Painter, Pos2, Rect, Response, RichText, Sense,
    Shadow, Stroke, Ui, vec2,
};

pub fn dim(p: &Palette, s: impl Into<String>) -> RichText {
    RichText::new(s.into()).color(p.dim)
}

pub fn small_dim(p: &Palette, s: impl Into<String>) -> RichText {
    RichText::new(s.into()).color(p.dim).size(12.0)
}

pub fn mono(s: impl Into<String>) -> RichText {
    RichText::new(s.into()).monospace()
}

pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, add)
        .inner
}

/// Page title with an optional dim line under it.
pub fn page_header(ui: &mut Ui, p: &Palette, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(22.0).strong());
    if !subtitle.is_empty() {
        ui.label(dim(p, subtitle));
    }
    ui.add_space(8.0);
}

pub fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(15.0).strong());
}

pub fn primary_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(text).color(p.on_accent).strong())
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .min_size(vec2(0.0, 30.0)),
    )
}

pub fn danger_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(if p.dark { Color32::from_rgb(0xc2, 0x3b, 0x34) } else { p.err })
            .stroke(Stroke::NONE)
            .min_size(vec2(0.0, 30.0)),
    )
}

pub fn button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, Button::new(text).min_size(vec2(0.0, 30.0)))
}

/// A small rounded status chip.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    Frame::NONE
        .fill(color.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).color(color).size(12.0).strong()))
        .response
}

/// A fixed-size "shared with the team" mark for table rows: the Teams glyph in a tinted pill. Unlike a text badge
/// it cannot be squeezed to "T..." when the name column is narrow, so the name is what gives way.
pub fn team_mark(ui: &mut Ui, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(8), color.gamma_multiply(0.18));
        paint_icon(ui.painter(), rect.center(), Icon::Teams, color);
    }
    resp
}

/// A filled status dot (and a faint ring when `ring`).
pub fn dot(ui: &mut Ui, color: Color32, ring: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
    let c = rect.center();
    if ring {
        ui.painter().circle_filled(c, 6.0, color.gamma_multiply(0.22));
    }
    ui.painter().circle_filled(c, 3.8, color);
}

/// Centered "nothing here" state with a helpful line; never a blank page.
pub fn empty_state(ui: &mut Ui, p: &Palette, glyph: &str, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).clamp(12.0, 140.0));
        ui.label(RichText::new(glyph).size(40.0).color(p.dim));
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(18.0).strong());
        ui.add_space(2.0);
        ui.label(dim(p, body));
        ui.add_space(10.0);
    });
}

/// `Name` above a field.
pub fn field_label(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(small_dim(p, text));
}

/// Width of the key column of `key   value` rows (Kernel and Settings pages).
pub const KEY_W: f32 = 132.0;

/// A left-aligned key in a fixed-width column, so every `key   value` row of a card lines up.
pub fn key_label(ui: &mut Ui, p: &Palette, text: &str) {
    ui.allocate_ui_with_layout(vec2(KEY_W, 22.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(KEY_W);
        ui.label(dim(p, text));
    });
}

/// `key   value` line used on the Kernel and Settings pages.
pub fn kv(ui: &mut Ui, p: &Palette, key: &str, value: RichText) {
    ui.horizontal(|ui| {
        key_label(ui, p, key);
        ui.add(egui::Label::new(value).truncate());
    });
}

/// Explanation under a checkbox, indented to start under the checkbox's own label.
pub fn check_hint(ui: &mut Ui, p: &Palette, text: &str) {
    Frame::new().inner_margin(Margin { left: 19, right: 0, top: 0, bottom: 0 }).show(ui, |ui| {
        ui.add(egui::Label::new(small_dim(p, text)).wrap());
    });
}

pub fn modal_frame(p: &Palette) -> Frame {
    Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(20))
        .shadow(Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(130) })
}

/// A centered modal of a fixed width. The caller decides what `should_close()` (Esc / backdrop click) means.
pub fn modal<R>(
    ctx: &egui::Context,
    p: &Palette,
    id: &str,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::ModalResponse<R> {
    egui::Modal::new(egui::Id::unique(id))
        .backdrop_color(Color32::from_black_alpha(if p.dark { 160 } else { 90 }))
        .frame(modal_frame(p))
        .show(ctx, |ui| {
            ui.set_width(width);
            add(ui)
        })
}

/// Dialog footer: buttons right aligned, the confirming one is added first (so it is the right-most).
pub fn footer<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(8.0);
    ui.with_layout(Layout::right_to_left(Align::Center), add).inner
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Profiles,
    Groups,
    Logs,
    Kernel,
    Teams,
    Settings,
}

/// Sidebar icons are drawn with the painter: the bundled fonts have no emoji coverage worth relying on, and
/// vector shapes stay crisp and take the theme colour.
pub fn paint_icon(g: &Painter, c: Pos2, icon: Icon, col: Color32) {
    let at = |x: f32, y: f32| c + vec2(x, y);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    let line = Stroke::new(1.6, col);
    match icon {
        Icon::Profiles => {
            g.circle_filled(at(0.0, -3.6), 3.3, col);
            let r = CornerRadius { nw: 6, ne: 6, sw: 2, se: 2 };
            g.rect_filled(rect(-6.5, 1.2, 6.5, 7.0), r, col);
        }
        Icon::Groups => {
            g.rect_filled(rect(-7.5, -6.0, -1.0, -2.0), CornerRadius::same(1), col);
            g.rect_filled(rect(-7.5, -3.5, 7.5, 6.5), CornerRadius::same(2), col);
        }
        Icon::Logs => {
            g.rect_stroke(rect(-5.8, -7.2, 5.8, 7.2), CornerRadius::same(2), line, egui::StrokeKind::Inside);
            for y in [-3.0, 0.2, 3.4] {
                g.line_segment([at(-2.6, y), at(2.6, y)], Stroke::new(1.4, col));
            }
        }
        Icon::Kernel => {
            g.rect_stroke(rect(-4.6, -4.6, 4.6, 4.6), CornerRadius::same(1), line, egui::StrokeKind::Inside);
            g.rect_filled(rect(-1.7, -1.7, 1.7, 1.7), CornerRadius::ZERO, col);
            for k in [-2.3_f32, 2.3] {
                g.line_segment([at(k, -7.4), at(k, -4.6)], Stroke::new(1.4, col));
                g.line_segment([at(k, 4.6), at(k, 7.4)], Stroke::new(1.4, col));
                g.line_segment([at(-7.4, k), at(-4.6, k)], Stroke::new(1.4, col));
                g.line_segment([at(4.6, k), at(7.4, k)], Stroke::new(1.4, col));
            }
        }
        Icon::Teams => {
            g.circle_filled(at(-3.6, -3.2), 2.8, col);
            g.circle_filled(at(4.0, -3.8), 2.4, col.gamma_multiply(0.7));
            g.rect_filled(rect(-8.0, 1.0, 1.0, 6.6), CornerRadius { nw: 5, ne: 5, sw: 1, se: 1 }, col);
            g.rect_filled(
                rect(2.6, 0.6, 8.0, 5.4),
                CornerRadius { nw: 4, ne: 4, sw: 1, se: 1 },
                col.gamma_multiply(0.7),
            );
        }
        Icon::Settings => {
            for (y, knob) in [(-4.6, -2.5), (0.0, 3.0), (4.6, -3.5)] {
                g.line_segment([at(-7.2, y), at(7.2, y)], Stroke::new(1.5, col));
                g.circle_filled(at(knob, y), 2.5, col);
            }
        }
    }
}
