//! Shared widgets: buttons, tabs, pills, cards, modal frames and the painter-drawn icon set.

use super::theme::{self, Palette};
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Layout, Margin, Painter,
    Pos2, Rect, Response, RichText, Sense, Shadow, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use std::f32::consts::PI;

pub fn dim(p: &Palette, s: impl Into<String>) -> RichText {
    RichText::new(s.into()).color(p.dim)
}

pub fn small_dim(p: &Palette, s: impl Into<String>) -> RichText {
    RichText::new(s.into()).color(p.dim).size(12.0)
}

pub fn mono(s: impl Into<String>) -> RichText {
    RichText::new(s.into()).monospace()
}

/// White text in the heavier face: titles, names, the active entry.
pub fn semibold(s: impl Into<String>, size: f32) -> RichText {
    RichText::new(s.into()).size(size).family(theme::semibold_family()).strong()
}

pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(18))
        .show(ui, add)
        .inner
}

pub fn section_title(ui: &mut Ui, text: &str) {
    ui.label(semibold(text, 15.0));
}

/// A filled rounded button of our own: egui keeps a `Button::fill` colour on hover, this one reacts.
fn solid_button(
    ui: &mut Ui,
    text: &str,
    fill: Color32,
    hover: Color32,
    fg: Color32,
    height: f32,
    on: bool,
) -> Response {
    let font = FontId::new(14.0, theme::semibold_family());
    let tw = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), fg).size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2((tw + 32.0).max(64.0), height), sense(on));
    if ui.is_rect_visible(rect) {
        let f = if !on {
            fill.gamma_multiply(0.4)
        } else if resp.is_pointer_button_down_on() {
            fill.gamma_multiply(0.85)
        } else if resp.hovered() {
            hover
        } else {
            fill
        };
        ui.painter().rect_filled(rect, CornerRadius::same(8), f);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            font,
            if on { fg } else { fg.gamma_multiply(0.55) },
        );
    }
    hand(resp, on)
}

fn sense(on: bool) -> Sense {
    if on { Sense::click() } else { Sense::hover() }
}

fn hand(resp: Response, on: bool) -> Response {
    if on { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

pub fn primary_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    solid_button(ui, text, p.accent, p.accent_hover, p.on_accent, 34.0, enabled)
}

/// A full-width blue button (the sidebar card's "Install kernel").
pub fn wide_primary_button(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    let font = FontId::new(13.0, theme::semibold_family());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if resp.is_pointer_button_down_on() {
            p.accent.gamma_multiply(0.85)
        } else if resp.hovered() {
            p.accent_hover
        } else {
            p.accent
        };
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, p.on_accent);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn danger_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let fill = if p.dark { Color32::from_rgb(0xc2, 0x3b, 0x34) } else { p.err };
    let hover = if p.dark { Color32::from_rgb(0xd8, 0x4a, 0x42) } else { Color32::from_rgb(0xa8, 0x20, 0x20) };
    solid_button(ui, text, fill, hover, Color32::WHITE, 34.0, enabled)
}

pub fn button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, Button::new(text).min_size(vec2(0.0, 34.0)))
}

/// A small rounded status chip.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    Frame::NONE
        .fill(color.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).color(color).size(12.0)))
        .response
}

/// A fixed-size "shared with the team" mark for table rows: the Teams glyph in a tinted pill. Unlike a text badge
/// it cannot be squeezed to "T..." when the name column is narrow, so the name is what gives way.
pub fn team_mark(ui: &mut Ui, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(8), color.gamma_multiply(0.18));
        paint_icon_scaled(ui.painter(), rect.center(), Icon::Teams, color, 0.7);
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
        ui.label(semibold(title, 18.0));
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
const KEY_W: f32 = 132.0;

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

fn modal_frame(p: &Palette) -> Frame {
    Frame::NONE
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(22))
        .shadow(Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(150) })
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
        .backdrop_color(Color32::from_black_alpha(if p.dark { 170 } else { 90 }))
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

// ------------------------------------------------------------------------------------------------------------------
// the page header: tabs over a hairline

const TAB_H: f32 = 40.0;

/// The strip at the top of every page: tabs on the left (the active one white with an underline), whatever `right`
/// adds on the right, a hairline underneath. Returns the tab that was clicked.
pub fn tab_header(
    ui: &mut Ui,
    p: &Palette,
    tabs: &[&str],
    active: usize,
    right: impl FnOnce(&mut Ui),
) -> Option<usize> {
    let mut clicked = None;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), TAB_H), Sense::hover());
    ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 28.0;
        for (i, label) in tabs.iter().enumerate() {
            if tab(ui, p, label, i == active).clicked() {
                clicked = Some(i);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            right(ui);
        });
    });
    ui.add_space(14.0);
    clicked
}

fn tab(ui: &mut Ui, p: &Palette, label: &str, active: bool) -> Response {
    let family = if active { theme::medium_family() } else { FontFamily::Proportional };
    let font = FontId::new(14.5, family);
    let tw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), p.text).size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2(tw, TAB_H), Sense::click());
    let col = if active || resp.hovered() { p.strong } else { p.text };
    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, label, font, col);
    if active {
        let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), pos2(rect.right(), rect.bottom()));
        ui.painter().rect_filled(bar, CornerRadius::same(1), p.strong);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// ------------------------------------------------------------------------------------------------------------------
// controls

/// A 40 px outlined square with an icon: the toolbar button. `danger` turns the icon red under the pointer.
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: Icon, tip: &str, enabled: bool, danger: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), sense(enabled));
    if ui.is_rect_visible(rect) {
        let hot = enabled && resp.hovered();
        let fill = if enabled && resp.is_pointer_button_down_on() {
            p.active
        } else if hot {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        let outline = if hot { p.hover_border() } else { p.border_strong };
        let r = CornerRadius::same(10);
        ui.painter().rect_filled(rect, r, fill);
        ui.painter().rect_stroke(
            rect,
            r,
            Stroke::new(1.0, if enabled { outline } else { p.border }),
            StrokeKind::Inside,
        );
        let col = match (enabled, hot, danger) {
            (false, ..) => p.dim.gamma_multiply(0.5),
            (_, true, true) => p.err,
            (_, true, false) => p.strong,
            _ => p.text,
        };
        paint_icon_scaled(ui.painter(), rect.center(), icon, col, 1.15);
    }
    hand(resp, enabled).on_hover_text(tip)
}

/// The big blue-tinted button at the head of the toolbar. Muted until there is something to act on.
pub fn open_button(ui: &mut Ui, p: &Palette, icon: Icon, label: &str, enabled: bool) -> Response {
    let font = FontId::new(14.0, theme::semibold_family());
    let tw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), p.on_accent).size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + 62.0, 40.0), sense(enabled));
    if ui.is_rect_visible(rect) {
        let fill = match (enabled, resp.is_pointer_button_down_on(), resp.hovered()) {
            (false, ..) => p.accent_off,
            (_, true, _) => p.accent,
            (_, _, true) => p.accent_hover,
            _ => p.accent,
        };
        let fg = if enabled {
            p.on_accent
        } else if p.dark {
            Color32::from_rgb(0x9a, 0xa1, 0xc4)
        } else {
            Color32::WHITE
        };
        ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
        paint_icon_scaled(ui.painter(), pos2(rect.left() + 24.0, rect.center().y), icon, fg, 1.0);
        ui.painter().text(pos2(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
    }
    hand(resp, enabled)
}

/// An outlined button with an icon and a label (the header's Export / Refresh).
pub fn icon_text_button(ui: &mut Ui, p: &Palette, icon: Icon, label: &str, tip: &str, enabled: bool) -> Response {
    let font = FontId::new(13.5, FontFamily::Proportional);
    let tw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), p.text).size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + 52.0, 36.0), sense(enabled));
    if ui.is_rect_visible(rect) {
        let hot = enabled && resp.hovered();
        let r = CornerRadius::same(10);
        if hot {
            ui.painter().rect_filled(rect, r, p.hover);
        }
        ui.painter().rect_stroke(
            rect,
            r,
            Stroke::new(1.0, if hot { p.hover_border() } else { p.border_strong }),
            StrokeKind::Inside,
        );
        let col = if !enabled {
            p.dim.gamma_multiply(0.5)
        } else if hot {
            p.strong
        } else {
            p.text
        };
        paint_icon_scaled(ui.painter(), pos2(rect.left() + 20.0, rect.center().y), icon, col, 1.0);
        ui.painter().text(pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, label, font, col);
    }
    hand(resp, enabled).on_hover_text(tip)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pill {
    Primary,
    Outline,
}

/// The little button in a table row: a blue pill (Open) or an outlined one (Close).
pub fn pill_button(ui: &mut Ui, p: &Palette, text: &str, icon: Option<Icon>, style: Pill, enabled: bool) -> Response {
    let font = FontId::new(13.0, theme::semibold_family());
    let tw = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), p.on_accent).size().x;
    let icon_w = if icon.is_some() { 18.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + icon_w + 22.0, 26.0), sense(enabled));
    if ui.is_rect_visible(rect) {
        let hot = enabled && resp.hovered();
        let r = CornerRadius::same(7);
        let fg = match (style, enabled) {
            (Pill::Primary, true) => p.on_accent,
            (Pill::Primary, false) => p.on_accent.gamma_multiply(0.55),
            (Pill::Outline, true) => {
                if hot {
                    p.strong
                } else {
                    p.text
                }
            }
            (Pill::Outline, false) => p.dim.gamma_multiply(0.6),
        };
        match style {
            Pill::Primary => {
                let fill = if !enabled {
                    p.accent_off
                } else if hot {
                    p.accent_hover
                } else {
                    p.accent
                };
                ui.painter().rect_filled(rect, r, fill);
            }
            Pill::Outline => {
                if hot {
                    ui.painter().rect_filled(rect, r, p.hover);
                }
                ui.painter().rect_stroke(
                    rect,
                    r,
                    Stroke::new(1.0, if hot { p.hover_border() } else { p.border_strong }),
                    StrokeKind::Inside,
                );
            }
        }
        let mut x = rect.left() + 12.0;
        if let Some(icon) = icon {
            paint_icon_scaled(ui.painter(), pos2(x + 5.0, rect.center().y), icon, fg, 0.75);
            x += icon_w;
        }
        ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, text, font, fg);
    }
    hand(resp, enabled)
}

/// A thin chevron for dropdowns, in place of egui's filled triangle (`ComboBox::icon`).
pub fn chevron_icon(ui: &Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, open: bool) {
    let c = rect.center();
    let (near, far) = if open { (2.2, -2.2) } else { (-2.2, 2.2) };
    let pts = vec![pos2(c.x - 4.5, c.y + near), pos2(c.x, c.y + far), pos2(c.x + 4.5, c.y + near)];
    ui.painter().add(Shape::line(pts, Stroke::new(1.5, visuals.fg_stroke.color)));
}

/// The three dots at the end of a row; the caller attaches the menu to the response.
pub fn more_button(ui: &mut Ui, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(8), p.hover);
        }
        paint_icon_scaled(
            ui.painter(),
            rect.center(),
            Icon::DotsV,
            if resp.hovered() { p.strong } else { p.text },
            1.0,
        );
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// The blue-ringed checkbox of the table. `mixed` is the header's "some, not all" state.
pub fn checkbox(ui: &mut Ui, p: &Palette, checked: bool, mixed: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let r = Rect::from_center_size(rect.center(), vec2(15.0, 15.0));
        let ring = if resp.hovered() { p.accent_hover } else { p.accent };
        if checked || mixed {
            ui.painter().rect_filled(r, CornerRadius::same(4), ring);
            let w = Stroke::new(1.8, Color32::WHITE);
            let c = r.center();
            if checked {
                ui.painter().add(Shape::line(vec![c + vec2(-3.6, 0.2), c + vec2(-1.0, 2.9), c + vec2(3.8, -2.7)], w));
            } else {
                ui.painter().line_segment([c + vec2(-3.4, 0.0), c + vec2(3.4, 0.0)], w);
            }
        } else {
            ui.painter().rect_stroke(r, CornerRadius::same(4), Stroke::new(1.5, ring), StrokeKind::Inside);
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// The wide search box: a magnifier on the left, a clear button on the right once there is text.
pub fn search_field(ui: &mut Ui, p: &Palette, text: &mut String, hint: &str, width: f32) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    let has_text = !text.is_empty();
    let inner =
        Rect::from_min_max(rect.min + vec2(42.0, 0.0), rect.max - vec2(if has_text { 40.0 } else { 14.0 }, 0.0));
    let resp = ui
        .scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .frame(Frame::NONE)
                    .text_color(p.strong)
                    .align(Align2::LEFT_CENTER)
                    .min_size(vec2(inner.width(), 40.0))
                    .desired_width(inner.width()),
            )
        })
        .inner;
    let ring = if resp.has_focus() { p.accent } else { p.border_strong };
    ui.painter().rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, ring), StrokeKind::Inside);
    paint_icon_scaled(ui.painter(), pos2(rect.left() + 22.0, rect.center().y), Icon::Search, p.dim, 1.05);
    if has_text {
        let x = Rect::from_center_size(pos2(rect.right() - 22.0, rect.center().y), vec2(26.0, 26.0));
        let r = ui.interact(x, ui.scope_id().with("search-clear"), Sense::click());
        paint_icon_scaled(ui.painter(), x.center(), Icon::Close, if r.hovered() { p.strong } else { p.dim }, 0.8);
        if r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Clear search").clicked() {
            text.clear();
        }
    }
    resp
}

// ------------------------------------------------------------------------------------------------------------------
// icons

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Profiles,
    Groups,
    Logs,
    Kernel,
    Teams,
    Settings,
    Plus,
    Play,
    Stop,
    MoveTo,
    Tag,
    Share,
    Export,
    Trash,
    Refresh,
    Moon,
    Sun,
    Search,
    Close,
    DotsV,
    Edit,
    Sort,
    SortUp,
    SortDown,
}

fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32, n: usize) -> Vec<(f32, f32)> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// The crescent of the theme toggle, traced on a 16 px grid.
const MOON: [(f32, f32); 30] = [
    (7.8, 0.7),
    (7.5, 2.3),
    (6.9, 3.8),
    (6.0, 5.1),
    (4.8, 6.2),
    (3.4, 7.1),
    (1.9, 7.6),
    (0.3, 7.9),
    (-1.3, 7.7),
    (-2.9, 7.3),
    (-4.3, 6.6),
    (-5.6, 5.6),
    (-6.6, 4.3),
    (-7.3, 2.9),
    (-7.7, 1.3),
    (-7.9, -0.3),
    (-7.6, -1.9),
    (-7.1, -3.4),
    (-6.2, -4.8),
    (-5.1, -6.0),
    (-3.8, -6.9),
    (-2.3, -7.5),
    (-0.7, -7.8),
    (-1.6, -5.9),
    (-1.9, -3.8),
    (-1.3, -1.7),
    (-0.1, 0.1),
    (1.7, 1.3),
    (3.8, 1.9),
    (5.9, 1.6),
];

/// Icons are line art on a 16 px grid centred on `c`, drawn with the painter: the bundled fonts have no symbol
/// coverage worth relying on, and shapes stay crisp and take the theme colour.
pub fn paint_icon_scaled(g: &Painter, c: Pos2, icon: Icon, col: Color32, s: f32) {
    let at = |x: f32, y: f32| c + vec2(x, y) * s;
    let w = (1.5 * s).max(1.2);
    let line = Stroke::new(w, col);
    let poly = |pts: &[(f32, f32)], closed: bool| {
        let v: Vec<Pos2> = pts.iter().map(|&(x, y)| at(x, y)).collect();
        g.add(if closed { Shape::closed_line(v, line) } else { Shape::line(v, line) });
    };
    let seg = |a: (f32, f32), b: (f32, f32)| {
        g.line_segment([at(a.0, a.1), at(b.0, b.1)], line);
    };
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        g.rect_stroke(
            Rect::from_min_max(at(x0, y0), at(x1, y1)),
            CornerRadius::same((r * s).round() as u8),
            line,
            StrokeKind::Middle,
        );
    };
    let ring = |x: f32, y: f32, r: f32| {
        g.circle_stroke(at(x, y), r * s, line);
    };
    let blob = |x: f32, y: f32, r: f32| {
        g.circle_filled(at(x, y), r * s, col);
    };
    let folder: [(f32, f32); 6] = [(-7.0, -4.5), (-2.6, -4.5), (-1.0, -2.6), (7.0, -2.6), (7.0, 5.0), (-7.0, 5.0)];
    match icon {
        Icon::Profiles => {
            rrect(-7.0, -5.5, 7.0, 5.5, 2.0);
            seg((-7.0, -2.0), (7.0, -2.0));
            blob(-4.6, -3.7, 0.5);
        }
        Icon::Groups => poly(&folder, true),
        Icon::Logs => {
            rrect(-5.0, -7.0, 5.0, 7.0, 2.0);
            seg((-2.4, -3.0), (2.4, -3.0));
            seg((-2.4, 0.0), (2.4, 0.0));
            seg((-2.4, 3.0), (0.4, 3.0));
        }
        Icon::Kernel => {
            rrect(-4.5, -4.5, 4.5, 4.5, 1.5);
            rrect(-1.7, -1.7, 1.7, 1.7, 0.5);
            for k in [-2.3_f32, 2.3] {
                seg((k, -7.2), (k, -4.5));
                seg((k, 4.5), (k, 7.2));
                seg((-7.2, k), (-4.5, k));
                seg((4.5, k), (7.2, k));
            }
        }
        Icon::Teams => {
            ring(-2.6, -3.4, 2.7);
            let mut body = vec![(-7.4, 7.0)];
            body.extend(arc(-2.6, 7.0, 4.8, PI * 1.08, PI * 1.92, 10));
            body.push((2.2, 7.0));
            poly(&body, false);
            ring(4.9, -3.9, 2.2);
            poly(&arc(5.4, 7.0, 3.6, PI * 1.22, PI * 1.78, 8), false);
        }
        Icon::Settings => {
            for (y, knob) in [(-4.6, -2.5), (0.0, 3.0), (4.6, -3.5)] {
                seg((-7.2, y), (7.2, y));
                blob(knob, y, 2.4);
            }
        }
        Icon::Plus => {
            seg((-6.0, 0.0), (6.0, 0.0));
            seg((0.0, -6.0), (0.0, 6.0));
        }
        Icon::Play => {
            g.add(Shape::convex_polygon(vec![at(-3.2, -5.4), at(-3.2, 5.4), at(5.6, 0.0)], col, Stroke::NONE));
        }
        Icon::Stop => {
            ring(0.0, 0.0, 6.4);
            seg((-2.5, -2.5), (2.5, 2.5));
            seg((2.5, -2.5), (-2.5, 2.5));
        }
        Icon::MoveTo => {
            poly(&folder, true);
            seg((-3.2, 1.2), (3.2, 1.2));
            poly(&[(1.0, -0.9), (3.2, 1.2), (1.0, 3.3)], false);
        }
        Icon::Tag => {
            poly(&[(-6.5, -6.5), (0.5, -6.5), (7.0, 0.0), (0.0, 7.0), (-6.5, 0.5)], true);
            blob(-3.0, -3.0, 1.2);
        }
        Icon::Share => {
            poly(&[(2.5, -2.0), (2.5, 6.0), (-6.5, 6.0), (-6.5, -5.0), (-0.5, -5.0)], false);
            seg((-1.0, 1.0), (6.0, -6.0));
            poly(&[(1.5, -6.2), (6.2, -6.2), (6.2, -1.5)], false);
        }
        Icon::Export => {
            poly(&[(-6.2, 2.0), (-6.2, 6.2), (6.2, 6.2), (6.2, 2.0)], false);
            seg((0.0, 3.8), (0.0, -6.2));
            poly(&[(-3.5, -2.7), (0.0, -6.2), (3.5, -2.7)], false);
        }
        Icon::Trash => {
            seg((-6.5, -4.2), (6.5, -4.2));
            poly(&[(-2.2, -4.2), (-2.2, -6.4), (2.2, -6.4), (2.2, -4.2)], false);
            poly(&[(-5.0, -4.2), (-4.2, 6.2), (4.2, 6.2), (5.0, -4.2)], false);
            seg((-1.6, -1.0), (-1.6, 3.2));
            seg((1.6, -1.0), (1.6, 3.2));
        }
        Icon::Refresh => {
            poly(&arc(0.0, 0.0, 5.6, -0.12 * PI, 1.55 * PI, 22), false);
            poly(&[(-1.6, -7.6), (1.0, -5.6), (-1.6, -3.6)], false);
        }
        Icon::Moon => poly(&MOON, true),
        Icon::Sun => {
            ring(0.0, 0.0, 3.2);
            for k in 0..8 {
                let a = k as f32 * PI / 4.0;
                seg((5.6 * a.cos(), 5.6 * a.sin()), (7.4 * a.cos(), 7.4 * a.sin()));
            }
        }
        Icon::Search => {
            ring(-1.6, -1.6, 4.8);
            seg((2.0, 2.0), (6.6, 6.6));
        }
        Icon::Close => {
            seg((-4.5, -4.5), (4.5, 4.5));
            seg((4.5, -4.5), (-4.5, 4.5));
        }
        Icon::DotsV => {
            for y in [-4.6, 0.0, 4.6] {
                blob(0.0, y, 1.35);
            }
        }
        Icon::Edit => {
            poly(&[(-6.5, 6.5), (-5.8, 2.6), (3.2, -6.4), (6.4, -3.2), (-2.6, 5.8)], true);
            seg((0.8, -4.4), (4.4, -0.8));
        }
        Icon::Sort => {
            g.add(Shape::convex_polygon(vec![at(-3.6, -1.4), at(3.6, -1.4), at(0.0, -5.4)], col, Stroke::NONE));
            g.add(Shape::convex_polygon(vec![at(-3.6, 1.4), at(3.6, 1.4), at(0.0, 5.4)], col, Stroke::NONE));
        }
        Icon::SortUp => {
            g.add(Shape::convex_polygon(vec![at(-4.0, 2.4), at(4.0, 2.4), at(0.0, -3.0)], col, Stroke::NONE));
        }
        Icon::SortDown => {
            g.add(Shape::convex_polygon(vec![at(-4.0, -2.4), at(4.0, -2.4), at(0.0, 3.0)], col, Stroke::NONE));
        }
    }
}
