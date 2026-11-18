//! The window chrome: sidebar, top bar and toasts.

use super::app::{CitroApp, Page, ToastKind};
use super::theme::{self, Palette};
use super::widgets::{self, Icon, semibold, small_dim};
use crate::backend::{BackendKind, KernelStatus};
use crate::settings::Theme;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Layout, Margin, Panel, Rect,
    Response, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2,
};
use std::time::Duration;

const SIDEBAR_W: f32 = 220.0;

/// The sidebar's sections: a heading (none for the first) and its pages. Groups lives in the Profiles tab strip.
const NAV: [(&str, &[Page]); 3] = [
    ("", &[Page::Profiles]),
    ("Browser", &[Page::Kernel, Page::Extensions, Page::Plugins, Page::Logs]),
    ("Workspace", &[Page::Teams, Page::Settings]),
];

impl CitroApp {
    pub(super) fn sidebar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        Panel::left("sidebar")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.sidebar).inner_margin(Margin::symmetric(14, 16)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(2.0);
                    widgets::logo(ui, 34.0);
                    ui.label(semibold("Citroenition", 19.0));
                });
                ui.add_space(18.0);
                if new_profile_button(ui, &p).clicked() {
                    self.go(Page::Profiles);
                    self.open_edit(None);
                }
                ui.spacing_mut().item_spacing.y = 2.0;
                for (heading, pages) in NAV {
                    if heading.is_empty() {
                        ui.add_space(12.0);
                    } else {
                        // a hairline and a small heading open each further section
                        ui.add_space(8.0);
                        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                        ui.painter().rect_filled(line, CornerRadius::ZERO, p.border);
                        ui.add_space(10.0);
                        section_label(ui, &p, heading);
                    }
                    for &page in pages {
                        let active = self.page == page || (page == Page::Profiles && self.page == Page::Groups);
                        let badge = match page {
                            Page::Profiles if self.rows_loaded => Some((self.rows.len().to_string(), p.dim)),
                            Page::Kernel if self.kernel.as_ref().is_some_and(|k| k.installed.is_none()) => {
                                Some(("!".to_owned(), p.err))
                            }
                            Page::Kernel if self.kernel.as_ref().is_some_and(KernelStatus::update_available) => {
                                Some(("new".to_owned(), p.accent_hover))
                            }
                            _ => None,
                        };
                        if nav_item(ui, &p, page.icon(), page.label(), active, badge).clicked() {
                            self.go(page);
                        }
                    }
                }
                // the version line and the status card are pinned to the sidebar's foot by hand: inside a
                // bottom-up layout the card's frame stretched over all the free height
                let area = ui.max_rect();
                let nav_bottom = ui.cursor().top();
                let has_update = matches!(
                    self.update.status,
                    super::update::Status::Available(_)
                        | super::update::Status::Downloading { .. }
                        | super::update::Status::Installed(_)
                );
                let foot_h = if has_update { 40.0 } else { 20.0 };
                let foot = Rect::from_min_max(pos2(area.left(), area.bottom() - foot_h), area.right_bottom());
                ui.scope_builder(UiBuilder::new().max_rect(foot).layout(Layout::top_down(Align::Min)), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.label(small_dim(&p, format!("Citroenition v{}", env!("CARGO_PKG_VERSION"))));
                    });
                    if has_update {
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            self.sidebar_update_link(ui);
                        });
                    }
                });
                let card_h = self.side_card_height();
                if foot.top() - nav_bottom > card_h + 24.0 {
                    let card =
                        Rect::from_min_size(pos2(area.left(), foot.top() - 10.0 - card_h), vec2(area.width(), card_h));
                    self.side_card(ui, card);
                }
            });
    }

    /// Where the data comes from and what the kernel is doing: the card at the foot of the sidebar.
    fn side_card(&mut self, ui: &mut Ui, card: Rect) {
        let p = self.pal;
        let (title, tag, tag_col) = match self.info.kind {
            BackendKind::Demo => ("Demo data".to_owned(), "Demo", p.warn),
            BackendKind::Local => ("This device".to_owned(), "Local", p.dim),
            BackendKind::Hub => {
                let team = self.teams.as_ref().and_then(|t| t.first()).map(|t| t.name.clone());
                let (tag, col) = if self.hub.connected { ("Online", p.ok) } else { ("Offline", p.err) };
                (team.unwrap_or_else(|| "Team hub".to_owned()), tag, col)
            }
        };
        let installed = self.kernel.as_ref().and_then(|k| k.installed.as_ref()).map(|k| k.version.clone());
        let known = self.kernel.is_some();
        let update = self.kernel.as_ref().is_some_and(KernelStatus::update_available);
        let (n_rows, n_open) = (self.rows.len(), self.open_count());
        let mut go_kernel = false;
        ui.painter().rect_filled(card, CornerRadius::same(12), p.panel);
        let inner = UiBuilder::new().max_rect(card.shrink(12.0)).layout(Layout::top_down(Align::Min));
        ui.scope_builder(inner, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::right_to_left(Align::Center)), |ui| {
                widgets::badge(ui, tag, tag_col);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(semibold(title, 13.5)).truncate());
                });
            });
            side_kv(ui, &p, "Profiles", RichText::new(n_rows.to_string()).color(p.text));
            let open_col = if n_open > 0 { p.ok } else { p.text };
            side_kv(ui, &p, "Open", RichText::new(n_open.to_string()).color(open_col));
            match &installed {
                Some(v) => side_kv(ui, &p, "Kernel", RichText::new(v).color(p.text)),
                None if known => side_kv(ui, &p, "Kernel", RichText::new("not installed").color(p.err)),
                None => side_kv(ui, &p, "Kernel", small_dim(&p, "…")),
            }
            if known && (installed.is_none() || update) {
                ui.add_space(4.0);
                let label = if installed.is_none() { "Install kernel" } else { "Update kernel" };
                go_kernel = widgets::wide_primary_button(ui, &p, label).clicked();
            }
        });
        if go_kernel {
            self.go(Page::Kernel);
        }
    }

    /// The card's height: margins, the title row, three key/value rows and the optional install/update button.
    fn side_card_height(&self) -> f32 {
        let button = self.kernel.as_ref().is_some_and(|k| k.installed.is_none() || k.update_available());
        if button { 172.0 } else { 124.0 }
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.settings.theme = t;
        self.pal = theme::palette(t);
        theme::apply(&self.ctx, t);
    }

    /// The strip over the content card: busy spinner, reload, theme toggle and who is working.
    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let (name, sub) = match self.info.kind {
            BackendKind::Demo => ("Demo mode".to_owned(), "Sample profiles".to_owned()),
            BackendKind::Local => ("Local".to_owned(), "This device".to_owned()),
            BackendKind::Hub => {
                let user = if self.hub.user.is_empty() { "Team hub".to_owned() } else { self.hub.user.clone() };
                let team = self.teams.as_ref().and_then(|t| t.first()).map(|t| t.name.clone());
                (user, team.unwrap_or_else(|| self.hub.device_name.clone()))
            }
        };
        let tip = crate::util::tildify(&self.info.data_dir, self.home.as_deref());
        Panel::top("topbar")
            .exact_size(64.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.bg).inner_margin(Margin::symmetric(12, 12)))
            .show(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if account_chip(ui, &p, &name, &sub).on_hover_text(tip).clicked() {
                        self.go(Page::Teams);
                    }
                    let (icon, tip) = if p.dark {
                        (Icon::Sun, "Switch to the light theme")
                    } else {
                        (Icon::Moon, "Switch to the dark theme")
                    };
                    if top_button(ui, &p, icon, tip).clicked() {
                        self.set_theme(if p.dark { Theme::Light } else { Theme::Dark });
                    }
                    if top_button(ui, &p, Icon::Refresh, "Reload everything (F5)").clicked() {
                        self.refresh_all();
                        self.logs_ui.mark_dirty();
                    }
                    if self.tasks.busy() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            });
    }

    pub(super) fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let p = self.pal;
        let mut dismiss = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-22.0, -22.0))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                for (i, t) in self.toasts.iter().enumerate().rev() {
                    let color = match t.kind {
                        ToastKind::Info => p.accent_hover,
                        ToastKind::Ok => p.ok,
                        ToastKind::Warn => p.warn,
                        ToastKind::Err => p.err,
                    };
                    let resp = Frame::NONE
                        .fill(p.panel)
                        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(14, 10))
                        .shadow(egui::Shadow {
                            offset: [0, 6],
                            blur: 18,
                            spread: 0,
                            color: Color32::from_black_alpha(110),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                widgets::dot(ui, color, false);
                                ui.add(egui::Label::new(RichText::new(&t.text).size(13.5).color(p.strong)).wrap());
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        dismiss = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
        ctx.request_repaint_after(Duration::from_millis(400));
    }
}

fn new_profile_button(ui: &mut Ui, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        p.accent.gamma_multiply(0.85)
    } else if resp.hovered() {
        p.accent_hover
    } else {
        p.accent
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let font = FontId::new(14.5, theme::semibold_family());
    let label = "New profile";
    let tw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x;
    let x0 = rect.center().x - (16.0 + 8.0 + tw) / 2.0;
    widgets::paint_icon_scaled(ui.painter(), pos2(x0 + 8.0, rect.center().y), Icon::Plus, Color32::WHITE, 1.0);
    ui.painter().text(pos2(x0 + 24.0, rect.center().y), Align2::LEFT_CENTER, label, font, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Create a profile (Ctrl+N)")
}

fn section_label(ui: &mut Ui, p: &Palette, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(RichText::new(text).size(12.5).color(p.dim));
    });
}

fn side_kv(ui: &mut Ui, p: &Palette, key: &str, value: RichText) {
    // a fixed-height row: nested right-to-left layouts would otherwise claim all the height below them
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.add(egui::Label::new(value.size(12.5)).truncate());
    });
    ui.painter().text(row.left_center(), Align2::LEFT_CENTER, key, FontId::proportional(12.5), p.dim);
}

fn nav_item(
    ui: &mut Ui,
    p: &Palette,
    icon: Icon,
    label: &str,
    active: bool,
    badge: Option<(String, Color32)>,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let hovered = resp.hovered();
    if active || hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(8), if active { p.active } else { p.hover });
    }
    let fg = if active || hovered { p.strong } else { p.text };
    widgets::paint_icon_scaled(ui.painter(), rect.left_center() + vec2(22.0, 0.0), icon, fg, 1.1);
    let family = if active { theme::medium_family() } else { FontFamily::Proportional };
    ui.painter().text(rect.left_center() + vec2(44.0, 0.0), Align2::LEFT_CENTER, label, FontId::new(14.0, family), fg);
    if let Some((text, color)) = badge {
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(12.5),
            color,
        );
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A 40 px square on the window background: reload, theme.
fn top_button(ui: &mut Ui, p: &Palette, icon: Icon, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
    let hot = resp.hovered();
    ui.painter().rect_filled(rect, CornerRadius::same(10), if hot { p.hover } else { p.surface });
    widgets::paint_icon_scaled(ui.painter(), rect.center(), icon, if hot { p.strong } else { p.text }, 1.15);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

/// Avatar, name and a dim second line, on the window background.
fn account_chip(ui: &mut Ui, p: &Palette, name: &str, sub: &str) -> Response {
    let name_font = FontId::new(13.0, theme::semibold_family());
    let sub_font = FontId::new(12.0, FontFamily::Proportional);
    let nw = ui.painter().layout_no_wrap(name.to_owned(), name_font.clone(), p.strong).size().x;
    let sw = ui.painter().layout_no_wrap(sub.to_owned(), sub_font.clone(), p.dim).size().x;
    let w = (nw.max(sw) + 8.0 + 28.0 + 10.0 + 16.0).clamp(130.0, 250.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
    let g = ui.painter().with_clip_rect(rect);
    g.rect_filled(rect, CornerRadius::same(10), if resp.hovered() { p.hover } else { p.surface });
    let c = pos2(rect.left() + 8.0 + 14.0, rect.center().y);
    g.circle_filled(c, 14.0, p.accent);
    g.text(
        c,
        Align2::CENTER_CENTER,
        crate::util::initial(name).to_string(),
        FontId::new(13.0, theme::semibold_family()),
        Color32::WHITE,
    );
    let x = rect.left() + 8.0 + 28.0 + 10.0;
    g.text(pos2(x, rect.center().y - 8.0), Align2::LEFT_CENTER, name, name_font, p.strong);
    g.text(pos2(x, rect.center().y + 8.0), Align2::LEFT_CENTER, sub, sub_font, p.dim);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
