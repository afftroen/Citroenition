//! The Plugins page (new, in progress): install a plugin from a GitHub link or a local file, see what it adds before
//! it is installed, choose which groups get it, check for a newer commit, remove it.

use super::app::{CitroApp, ToastKind};
use super::tasks::{Msg, Refresh};
use super::widgets::{self, dim, semibold, small_dim};
use crate::backend::{BResult, PluginPreview};
use citro_core::plugins::{Manifest, Plugin};
use eframe::egui::{self, Align, CornerRadius, Frame, Key, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit, Ui};
use std::collections::HashMap;

pub const EXAMPLE: &str = "https://github.com/afftroen/Citroenition/tree/main/examples/plugins/tiktok-pack";

/// An update check: running (`None`), or the newest commit and the page with the changes (when it is newer).
type UpdateCheck = Option<Result<Option<(String, String)>, String>>;

#[derive(Default)]
pub struct State {
    loaded: bool,
    pub input: String,
    pub fetching: bool,
    pub preview: Option<PluginPreview>,
    pub installing: bool,
    pub error: Option<String>,
    /// Update checks: running (`None`) or the answer (newest commit and the page with the changes, if newer).
    pub newest: HashMap<String, UpdateCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Groups,
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// What a plugin adds, in words.
fn adds(m: &Manifest) -> Vec<String> {
    let mut v = Vec::new();
    let n = |k: usize, one: &str, many: &str| if k == 1 { format!("1 {one}") } else { format!("{k} {many}") };
    if !m.fields.is_empty() {
        let names: Vec<&str> =
            m.fields.iter().map(|f| if f.label.is_empty() { f.key.as_str() } else { f.label.as_str() }).collect();
        v.push(format!("{} on each profile: {}", n(m.fields.len(), "field", "fields"), names.join(", ")));
    }
    if !m.start_pages.is_empty() {
        v.push(format!("{}: {}", n(m.start_pages.len(), "start page", "start pages"), m.start_pages.join(", ")));
    }
    if !m.bookmarks.is_empty() {
        let names: Vec<&str> =
            m.bookmarks.iter().map(|b| if b.name.is_empty() { b.url.as_str() } else { b.name.as_str() }).collect();
        v.push(format!("{}: {}", n(m.bookmarks.len(), "bookmark", "bookmarks"), names.join(", ")));
    }
    if !m.extensions.is_empty() {
        v.push(format!(
            "{} (they run inside the browser)",
            n(m.extensions.len(), "browser extension", "browser extensions")
        ));
    }
    if v.is_empty() {
        v.push("nothing yet".into());
    }
    v
}

impl CitroApp {
    pub fn refresh_plugins(&mut self) {
        self.tasks.spawn(false, |w| w.send(Msg::Plugins(w.backend.plugins())));
    }

    pub fn plugins_loaded(&mut self, r: BResult<Vec<Plugin>>) {
        self.plugins_ui.loaded = true;
        match r {
            Ok(l) => self.plugins = l,
            Err(e) => self.plugins_ui.error = Some(e.to_string()),
        }
    }

    pub fn plugin_fetched(&mut self, r: BResult<PluginPreview>) {
        let s = &mut self.plugins_ui;
        s.fetching = false;
        match r {
            Ok(p) => {
                s.preview = Some(p);
                s.error = None;
            }
            Err(e) => s.error = Some(e.to_string()),
        }
    }

    pub fn plugin_installed(&mut self, r: BResult<Plugin>) {
        let s = &mut self.plugins_ui;
        s.installing = false;
        match r {
            Ok(p) => {
                s.preview = None;
                s.input.clear();
                s.newest.remove(p.id());
                self.toast(ToastKind::Ok, format!("Installed {} {}", p.manifest.name, p.manifest.version));
                self.refresh_plugins();
            }
            Err(e) => s.error = Some(e.to_string()),
        }
    }

    pub fn plugin_newest(&mut self, id: String, r: BResult<Option<(String, String)>>) {
        self.plugins_ui.newest.insert(id, Some(r.map_err(|e| e.to_string())));
    }

    fn fetch_plugin(&mut self, input: String) {
        let s = &mut self.plugins_ui;
        if input.trim().is_empty() || s.fetching {
            return;
        }
        s.fetching = true;
        s.error = None;
        s.preview = None;
        let input = crate::util::expand_tilde(input.trim(), self.home.as_deref()).display().to_string();
        let input =
            if std::path::Path::new(&input).is_absolute() { input } else { self.plugins_ui.input.trim().to_owned() };
        self.tasks.spawn(true, move |w| w.send(Msg::PluginFetched(w.backend.fetch_plugin(&input))));
    }

    pub fn plugins_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        if !self.plugins_ui.loaded {
            self.plugins_ui.loaded = true;
            self.refresh_plugins();
        }
        let demo = self.settings_path.is_none();
        widgets::tab_header(ui, &p, &["Plugins"], 0, |ui| {
            if widgets::button(ui, "Example plugin", true).on_hover_text(EXAMPLE).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(EXAMPLE));
            }
        });

        // the notice: new, in progress, and nobody reviews plugins
        Frame::NONE
            .fill(p.warn.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, p.warn.gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::badge(ui, "NEW", p.warn);
                    ui.label(semibold("Plugins are new and still in progress", 14.0));
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(dim(
                        &p,
                        "Plugins come straight from GitHub or a file, and nobody checks them. Install only plugins \
                         from people you trust: a plugin's browser extensions run inside your profiles and can read \
                         the sites they are allowed on. In this version plugins add fields, start pages, bookmarks \
                         and extensions; they cannot run programs on this computer yet. Expect changes.",
                    ))
                    .wrap(),
                );
            });
        ui.add_space(14.0);

        // install
        let mut get: Option<String> = None;
        let (mut install, mut cancel) = (false, false);
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Add a plugin");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 150.0).max(200.0);
                let r = ui.add(
                    TextEdit::singleline(&mut self.plugins_ui.input)
                        .hint_text("https://github.com/owner/repo  or a folder / .zip on this computer")
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(w),
                );
                let ready = !self.plugins_ui.input.trim().is_empty() && !self.plugins_ui.fetching && !demo;
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && ready)
                    || widgets::primary_button(ui, &p, "Get plugin", ready).clicked()
                {
                    get = Some(self.plugins_ui.input.clone());
                }
                if self.plugins_ui.fetching {
                    ui.add(egui::Spinner::new().size(16.0));
                }
            });
            ui.add_space(4.0);
            ui.label(small_dim(
                &p,
                "A GitHub link installs the newest commit and stays on it: updates only happen when you press Update. \
                 A plugin in a folder of a bigger repository: use its …/tree/main/folder link.",
            ));
            if demo {
                ui.label(small_dim(&p, "Not available with demo data."));
            }
            if let Some(e) = &self.plugins_ui.error {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(e).color(p.err)).wrap());
            }
            if let Some(pv) = &self.plugins_ui.preview {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                let m = &pv.manifest;
                ui.horizontal(|ui| {
                    ui.label(semibold(&m.name, 16.0));
                    ui.label(small_dim(&p, format!("v{}", m.version)));
                    if !m.author.is_empty() {
                        ui.label(small_dim(&p, format!("by {}", m.author)));
                    }
                });
                if !m.description.trim().is_empty() {
                    ui.add(egui::Label::new(dim(&p, m.description.trim())).wrap());
                }
                let from = if pv.commit.is_empty() {
                    pv.source.clone()
                } else {
                    format!("{} @ {}", pv.source, short(&pv.commit))
                };
                ui.label(small_dim(&p, format!("From {from}")));
                if let Some(old) = &pv.existing {
                    ui.label(
                        RichText::new(format!(
                            "Already installed (v{}): installing replaces it and keeps its groups.",
                            old.manifest.version
                        ))
                        .color(p.warn)
                        .size(12.5),
                    );
                }
                ui.add_space(6.0);
                ui.label(semibold("It adds", 13.0));
                for line in adds(m) {
                    ui.label(dim(&p, format!("\u{2022} {line}")));
                }
                if !m.extensions.is_empty() {
                    ui.label(
                        RichText::new(
                            "Its extensions run inside your profiles' browsers: install only if you trust the author.",
                        )
                        .color(p.warn)
                        .size(12.5),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let busy = self.plugins_ui.installing;
                    if widgets::primary_button(ui, &p, if pv.existing.is_some() { "Update" } else { "Install" }, !busy)
                        .clicked()
                    {
                        install = true;
                    }
                    if widgets::button(ui, "Cancel", !busy).clicked() {
                        cancel = true;
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            }
        });
        if let Some(input) = get {
            self.fetch_plugin(input);
        }
        if cancel {
            self.plugins_ui.preview = None;
        }
        if install && let Some(pv) = self.plugins_ui.preview.clone() {
            self.plugins_ui.installing = true;
            self.tasks.spawn(true, move |w| w.send(Msg::PluginInstalled(w.backend.install_plugin(&pv))));
        }
        ui.add_space(14.0);

        // installed
        let list = self.plugins.clone();
        if list.is_empty() {
            widgets::empty_state(
                ui,
                &p,
                "\u{2726}",
                "No plugins yet",
                "Paste a GitHub link above. The example plugin adds TikTok account fields, the Ads Manager as a start page and bookmarks.",
            );
            return;
        }
        let groups = self.groups.clone();
        let mut scope: Option<(String, bool, Vec<String>)> = None;
        let mut remove: Option<Plugin> = None;
        let mut check: Option<String> = None;
        let mut update: Option<String> = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            for pl in &list {
                let m = &pl.manifest;
                Frame::NONE
                    .fill(p.raised)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            widgets::paint_icon_box(ui, &p, widgets::Icon::Plug);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.horizontal(|ui| {
                                    ui.label(semibold(&m.name, 15.0));
                                    ui.label(small_dim(&p, format!("v{}", m.version)));
                                    if !m.author.is_empty() {
                                        ui.label(small_dim(&p, format!("by {}", m.author)));
                                    }
                                });
                                if !m.description.trim().is_empty() {
                                    ui.add(egui::Label::new(dim(&p, m.description.trim())).truncate());
                                }
                                let from = if pl.commit.is_empty() {
                                    format!("{} (local)", pl.source)
                                } else {
                                    format!("{} @ {}", pl.source, short(&pl.commit))
                                };
                                ui.add(egui::Label::new(small_dim(&p, from)).truncate());
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::button(ui, "Remove", true).clicked() {
                                    remove = Some(pl.clone());
                                }
                                if pl.is_github() {
                                    match self.plugins_ui.newest.get(pl.id()) {
                                        Some(None) => {
                                            ui.add(egui::Spinner::new().size(14.0));
                                        }
                                        Some(Some(Ok(Some((new, url))))) => {
                                            if widgets::primary_button(ui, &p, "Update", true).clicked() {
                                                update = Some(pl.source.clone());
                                            }
                                            if ui
                                                .link(format!(
                                                    "See changes ({} \u{2192} {})",
                                                    short(&pl.commit),
                                                    short(new)
                                                ))
                                                .clicked()
                                            {
                                                ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                                            }
                                        }
                                        Some(Some(Ok(None))) => {
                                            ui.label(small_dim(&p, "Up to date"));
                                        }
                                        Some(Some(Err(e))) => {
                                            ui.label(RichText::new("Check failed").color(p.err).size(12.0))
                                                .on_hover_text(e);
                                        }
                                        None => {
                                            if widgets::button(ui, "Check for update", true).clicked() {
                                                check = Some(pl.id().to_owned());
                                            }
                                        }
                                    }
                                }
                            });
                        });
                        ui.add_space(8.0);
                        for line in adds(m) {
                            ui.label(small_dim(&p, format!("\u{2022} {line}")));
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(dim(&p, "Switched on for"));
                            ui.add_space(8.0);
                            let mut sc = if pl.all_profiles { Scope::All } else { Scope::Groups };
                            if widgets::segmented(
                                ui,
                                &p,
                                &mut sc,
                                &[(Scope::All, "All profiles"), (Scope::Groups, "Chosen groups")],
                            ) {
                                scope = Some((pl.id().to_owned(), sc == Scope::All, pl.groups.clone()));
                            }
                        });
                        if !pl.all_profiles {
                            if groups.is_empty() {
                                ui.label(small_dim(
                                    &p,
                                    "There are no groups yet: create them on the Groups tab of Profiles.",
                                ));
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 18.0;
                                for g in &groups {
                                    let mut on = pl.groups.contains(&g.group_id);
                                    if ui.checkbox(&mut on, &g.group_name).changed() {
                                        let mut gs: Vec<String> =
                                            pl.groups.iter().filter(|x| **x != g.group_id).cloned().collect();
                                        if on {
                                            gs.push(g.group_id.clone());
                                        }
                                        scope = Some((pl.id().to_owned(), false, gs));
                                    }
                                }
                            });
                            if pl.groups.is_empty() && !groups.is_empty() {
                                ui.label(
                                    RichText::new("No group chosen: no profile gets it.").color(p.warn).size(12.0),
                                );
                            }
                        }
                    });
            }
        });
        if let Some((id, all, gs)) = scope {
            if let Some(x) = self.plugins.iter_mut().find(|x| x.id() == id) {
                x.all_profiles = all;
                x.groups = gs.clone();
            }
            self.run_op(Vec::new(), "Saved".to_owned(), Refresh::NONE, move |b| b.set_plugin_scope(&id, all, gs));
        }
        if let Some(pl) = remove {
            let (id, name) = (pl.id().to_owned(), pl.manifest.name.clone());
            self.plugins.retain(|x| x.id() != id);
            self.run_op(Vec::new(), format!("Removed {name}"), Refresh::NONE, move |b| b.remove_plugin(&id));
        }
        if let Some(id) = check {
            self.plugins_ui.newest.insert(id.clone(), None);
            self.tasks.spawn(false, move |w| {
                let result = w.backend.newest_plugin_commit(&id);
                w.send(Msg::PluginNewest { id, result });
            });
        }
        if let Some(src) = update {
            // fetch the newest commit and show it like a new install: the user sees what it adds before it changes
            self.plugins_ui.input = src.clone();
            self.fetch_plugin(src);
        }
    }
}
