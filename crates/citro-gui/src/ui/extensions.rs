//! The Extensions page: add Chrome extensions from the Chrome Web Store and choose which profiles load them (every
//! profile, or the profiles of chosen groups), like other antidetect browsers' team extensions.

use super::app::{CitroApp, ToastKind};
use super::tasks::{Msg, Refresh};
use super::widgets::{self, dim, semibold, small_dim};
use crate::backend::{BResult, Extension};
use eframe::egui::{self, Align, CornerRadius, Frame, Key, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit, Ui};

pub const STORE_URL: &str = "https://chromewebstore.google.com/category/extensions";

#[derive(Default)]
pub struct State {
    pub list: Option<Vec<Extension>>,
    pub error: Option<String>,
    pub input: String,
    pub adding: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Groups,
}

impl CitroApp {
    pub fn refresh_extensions(&mut self) {
        self.tasks.spawn(false, |w| w.send(Msg::Extensions(w.backend.extensions())));
    }

    pub fn extensions_loaded(&mut self, r: BResult<Vec<Extension>>) {
        match r {
            Ok(l) => {
                self.extensions_ui.list = Some(l);
                self.extensions_ui.error = None;
            }
            Err(e) => self.extensions_ui.error = Some(e.to_string()),
        }
    }

    pub fn extension_added(&mut self, r: BResult<Extension>) {
        self.extensions_ui.adding = false;
        match r {
            Ok(e) => {
                self.extensions_ui.input.clear();
                self.extensions_ui.error = None;
                self.toast(ToastKind::Ok, format!("Added {} {}", e.name, e.version));
                self.refresh_extensions();
            }
            Err(e) => self.extensions_ui.error = Some(e.to_string()),
        }
    }

    fn add_extension(&mut self) {
        let input = self.extensions_ui.input.trim().to_owned();
        if input.is_empty() || self.extensions_ui.adding {
            return;
        }
        self.extensions_ui.adding = true;
        self.extensions_ui.error = None;
        self.tasks.spawn(true, move |w| w.send(Msg::ExtensionAdded(w.backend.add_extension(&input))));
    }

    pub fn extensions_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        if self.extensions_ui.list.is_none() && self.extensions_ui.error.is_none() {
            self.extensions_ui.list = Some(Vec::new());
            self.refresh_extensions();
        }
        widgets::tab_header(ui, &p, &["Extensions"], 0, |ui| {
            if widgets::button(ui, "Open Chrome Web Store", true).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(STORE_URL));
            }
        });
        ui.label(dim(&p, "Chrome extensions for your profiles. A change applies the next time a profile opens."));
        ui.add_space(14.0);

        // add
        let mut add = false;
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Add from the Chrome Web Store");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 130.0).max(200.0);
                let r = ui.add(
                    TextEdit::singleline(&mut self.extensions_ui.input)
                        .hint_text("https://chromewebstore.google.com/detail/…/<id>  or the extension id")
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(w),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    add = true;
                }
                let ready = !self.extensions_ui.input.trim().is_empty() && !self.extensions_ui.adding;
                if widgets::primary_button(ui, &p, "Add", ready).clicked() {
                    add = true;
                }
                if self.extensions_ui.adding {
                    ui.add(egui::Spinner::new().size(16.0));
                }
            });
            ui.add_space(4.0);
            match &self.extensions_ui.error {
                Some(e) => {
                    ui.add(egui::Label::new(RichText::new(e).color(p.err).size(12.5)).wrap());
                }
                None => {
                    ui.label(small_dim(
                        &p,
                        "Open an extension in the Chrome Web Store, copy the address from the address bar and paste it \
                         here. It is downloaded once and loaded into every profile until you choose groups below.",
                    ));
                }
            }
        });
        if add {
            self.add_extension();
        }
        ui.add_space(14.0);

        let list = self.extensions_ui.list.clone().unwrap_or_default();
        if list.is_empty() {
            widgets::empty_state(
                ui,
                &p,
                "\u{29c9}",
                "No extensions yet",
                "Add one from the Chrome Web Store above: password managers, wallets, ad blockers…",
            );
            return;
        }
        let groups = self.groups.clone();
        let mut change: Option<(String, bool, Vec<String>)> = None;
        let mut remove: Option<Extension> = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            for e in &list {
                Frame::NONE
                    .fill(p.raised)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            widgets::paint_icon_box(ui, &p, widgets::Icon::Puzzle);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.horizontal(|ui| {
                                    ui.label(semibold(&e.name, 15.0));
                                    ui.label(small_dim(&p, format!("v{}", e.version)));
                                });
                                if !e.description.trim().is_empty() {
                                    ui.add(egui::Label::new(dim(&p, e.description.trim())).truncate());
                                }
                                ui.label(small_dim(&p, &e.id));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::button(ui, "Remove", true).clicked() {
                                    remove = Some(e.clone());
                                }
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(dim(&p, "Loaded into"));
                            ui.add_space(8.0);
                            let mut scope = if e.all_profiles { Scope::All } else { Scope::Groups };
                            if widgets::segmented(
                                ui,
                                &p,
                                &mut scope,
                                &[(Scope::All, "All profiles"), (Scope::Groups, "Chosen groups")],
                            ) {
                                change = Some((e.id.clone(), scope == Scope::All, e.groups.clone()));
                            }
                        });
                        if !e.all_profiles {
                            ui.add_space(6.0);
                            if groups.is_empty() {
                                ui.label(small_dim(
                                    &p,
                                    "There are no groups yet: create them on the Groups tab of Profiles.",
                                ));
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 18.0;
                                for g in &groups {
                                    let mut on = e.groups.contains(&g.group_id);
                                    if ui.checkbox(&mut on, &g.group_name).changed() {
                                        let mut gs: Vec<String> =
                                            e.groups.iter().filter(|x| **x != g.group_id).cloned().collect();
                                        if on {
                                            gs.push(g.group_id.clone());
                                        }
                                        change = Some((e.id.clone(), false, gs));
                                    }
                                }
                            });
                            if e.groups.is_empty() && !groups.is_empty() {
                                ui.label(
                                    RichText::new("No group chosen: no profile loads it.").color(p.warn).size(12.0),
                                );
                            }
                        }
                    });
            }
        });
        if let Some((id, all, gs)) = change {
            // show the choice at once; the backend follows
            if let Some(l) = self.extensions_ui.list.as_mut()
                && let Some(x) = l.iter_mut().find(|x| x.id == id)
            {
                x.all_profiles = all;
                x.groups = gs.clone();
            }
            self.run_op(Vec::new(), "Saved".to_owned(), Refresh::NONE, move |b| b.assign_extension(&id, all, gs));
        }
        if let Some(e) = remove {
            let (id, name) = (e.id.clone(), e.name.clone());
            if let Some(l) = self.extensions_ui.list.as_mut() {
                l.retain(|x| x.id != id);
            }
            self.run_op(Vec::new(), format!("Removed {name}"), Refresh::NONE, move |b| b.remove_extension(&id));
        }
    }
}
