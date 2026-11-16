//! The window chrome: sidebar, status bar and toasts.

use super::app::{CitroApp, Page, ToastKind};
use super::theme::{self, Palette};
use super::widgets::{self, Icon, small_dim};
use crate::backend::{BackendKind, KernelStatus};
use crate::settings::Theme;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Panel, Rect, Response, RichText, Sense,
    Stroke, Ui, vec2,
};
use std::time::Duration;

impl CitroApp {
    pub(super) fn sidebar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        Panel::left("sidebar")
            .exact_size(158.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.sidebar).inner_margin(Margin::symmetric(10, 14)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 12.0, Color32::from_rgb(0xf2, 0xc9, 0x4c));
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        "C",
                        FontId::proportional(15.0),
                        Color32::from_rgb(0x3a, 0x2f, 0x00),
                    );
                    ui.label(RichText::new("Citroenition").size(16.5).strong());
                });
                ui.add_space(16.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                for page in Page::ALL {
                    let badge = match page {
                        Page::Profiles if self.rows_loaded => Some((self.rows.len().to_string(), p.dim)),
                        Page::Kernel if self.kernel.as_ref().is_some_and(|k| k.installed.is_none()) => {
                            Some(("!".to_owned(), p.err))
                        }
                        Page::Kernel if self.kernel.as_ref().is_some_and(KernelStatus::update_available) => {
                            Some(("new".to_owned(), p.accent))
                        }
                        _ => None,
                    };
                    if nav_item(ui, &p, page.icon(), page.label(), self.page == page, badge).clicked() {
                        self.go(page);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(small_dim(&p, format!("v{}", env!("CARGO_PKG_VERSION"))));
                    });
                    let label = if p.dark { "Light theme" } else { "Dark theme" };
                    if ui.add(egui::Button::new(RichText::new(label).size(12.5)).frame(false)).clicked() {
                        let t = if p.dark { Theme::Light } else { Theme::Dark };
                        self.set_theme(t);
                    }
                });
            });
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.settings.theme = t;
        self.pal = theme::palette(t);
        theme::apply(&self.ctx, t);
    }

    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        Panel::bottom("status")
            .exact_size(30.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.sidebar).inner_margin(Margin::symmetric(14, 6)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let n = self.rows.len();
                    ui.label(small_dim(&p, format!("{n} profile{}", if n == 1 { "" } else { "s" })));
                    ui.label(small_dim(&p, format!("{} selected", self.profiles.selection.len())));
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        widgets::dot(ui, if self.open_count() > 0 { p.ok } else { p.idle }, false);
                        ui.label(small_dim(&p, format!("{} open", self.open_count())));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        let backend = match self.info.kind {
                            BackendKind::Demo => "Demo data".to_owned(),
                            BackendKind::Local => "Local".to_owned(),
                            BackendKind::Hub => match self.teams.as_ref().and_then(|t| t.first()) {
                                Some(t) => format!("Team: {}", t.name),
                                None => "Team hub".to_owned(),
                            },
                        };
                        let tip = crate::util::tildify(&self.info.data_dir, self.home.as_deref());
                        ui.label(small_dim(&p, backend)).on_hover_text(tip);
                        match self.kernel.as_ref().and_then(|k| k.installed.as_ref()) {
                            Some(k) => ui.label(small_dim(&p, format!("Kernel {}", k.version))),
                            None if self.kernel.is_some() => {
                                ui.label(RichText::new("No kernel found").size(12.0).color(p.err))
                            }
                            None => ui.label(small_dim(&p, "Kernel …")),
                        };
                        if self.tasks.busy() {
                            ui.add(egui::Spinner::new().size(13.0));
                        }
                    });
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
            .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -44.0))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                for (i, t) in self.toasts.iter().enumerate().rev() {
                    let color = match t.kind {
                        ToastKind::Info => p.accent,
                        ToastKind::Ok => p.ok,
                        ToastKind::Warn => p.warn,
                        ToastKind::Err => p.err,
                    };
                    let resp = Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(14, 9))
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 14,
                            spread: 0,
                            color: Color32::from_black_alpha(90),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                widgets::dot(ui, color, false);
                                ui.add(egui::Label::new(RichText::new(&t.text).size(13.5)).wrap());
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

    // ----------------------------------------------------------------------------------------------------------
    // screenshot harness
}

fn nav_item(
    ui: &mut Ui,
    p: &Palette,
    icon: Icon,
    label: &str,
    active: bool,
    badge: Option<(String, Color32)>,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let hovered = resp.hovered();
    if active || hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(8), if active { p.accent_soft } else { p.raised });
    }
    if active {
        let bar = Rect::from_center_size(rect.left_center() + vec2(1.5, 0.0), vec2(3.0, 16.0));
        ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
    }
    let fg = if active || hovered { p.text } else { p.dim };
    widgets::paint_icon(ui.painter(), rect.left_center() + vec2(18.0, 0.0), icon, if active { p.accent } else { fg });
    ui.painter().text(rect.left_center() + vec2(36.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(14.5), fg);
    if let Some((text, color)) = badge {
        ui.painter().text(
            rect.right_center() - vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(12.0),
            color,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
