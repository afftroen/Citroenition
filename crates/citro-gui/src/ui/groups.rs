//! The Groups page: create, rename and delete groups. Every row shows how many profiles it holds and jumps to the
//! profile list filtered to that group.

use super::app::{CitroApp, Page};
use super::dialogs::{Confirm, ConfirmAction};
use super::tasks::{Msg, Refresh};
use super::widgets::{self, dim};
use crate::backend::group_label;
use citro_core::profile::Group;
use eframe::egui::{self, Align, Key, Layout, RichText, TextEdit, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::HashMap;

const ROW_H: f32 = 38.0;
/// Gap between the right-aligned profile counts and the remark column.
const COUNT_INSET: f32 = 24.0;

#[derive(Default)]
pub struct State {
    new_name: String,
    new_remark: String,
    rename: Option<Rename>,
}

/// The open "Group settings" window.
struct Rename {
    id: String,
    name: String,
    remark: String,
    tabs: Vec<String>,
    /// The start pages as typed, one per line.
    tabs_text: String,
    /// Extension id -> loaded into this group's profiles.
    extensions: Vec<(String, bool)>,
    /// `Name | URL` per line.
    bookmarks_text: String,
}

impl State {
    /// The group was created: empty the form for the next one.
    pub fn created(&mut self, _g: &Group) {
        self.new_name.clear();
        self.new_remark.clear();
    }
}

enum Act {
    View(String),
    StartRename(Group),
    SaveRename,
    CancelRename,
    Delete(String),
    Create,
}

impl CitroApp {
    pub fn groups_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let counts = self.group_counts();
        let mut acts: Vec<Act> = Vec::new();
        if widgets::tab_header(ui, &p, &["Profiles", "Groups"], 1, |_| {}) == Some(0) {
            self.go(Page::Profiles);
        }
        let n = self.groups.len();
        ui.label(dim(
            &p,
            format!(
                "{n} group{}  \u{b7}  Groups keep related profiles together, for example one per shop, market or account family.",
                if n == 1 { "" } else { "s" }
            ),
        ));
        ui.add_space(10.0);

        // create bar --------------------------------------------------------------------------------------------
        let name = self.groups_ui.new_name.trim().to_owned();
        let dup = !name.is_empty() && self.groups.iter().any(|g| g.group_name.eq_ignore_ascii_case(&name));
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let s = &mut self.groups_ui;
                let name_box =
                    ui.add(TextEdit::singleline(&mut s.new_name).hint_text("New group name").desired_width(230.0));
                ui.add(TextEdit::singleline(&mut s.new_remark).hint_text("Remark (optional)").desired_width(300.0));
                let enter = name_box.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let can = !name.is_empty() && !dup;
                if widgets::primary_button(ui, &p, "+ Create group", can).clicked() || (enter && can) {
                    acts.push(Act::Create);
                }
                if dup {
                    ui.label(RichText::new(format!("\u{201c}{name}\u{201d} already exists")).color(p.err).size(12.5));
                }
            });
        });
        ui.add_space(10.0);

        // table -------------------------------------------------------------------------------------------------
        let mut entries: Vec<Option<&Group>> = vec![None];
        entries.extend(self.groups.iter().map(Some));
        // the settings window replaced editing in the row
        let rename_id: Option<String> = None;
        let avail = ui.available_height();
        egui::Frame::NONE
            .fill(p.surface)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                let st = &mut self.groups_ui;
                TableBuilder::new(ui)
                    .id_salt("groups-table")
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .max_scroll_height((avail - 24.0).max(120.0))
                    .column(Column::initial(250.0).at_least(160.0).clip(true))
                    .column(Column::exact(86.0))
                    .column(Column::remainder().at_least(160.0).clip(true))
                    .column(Column::exact(220.0))
                    .header(30.0, |mut h| {
                        for (i, t) in ["Group", "Profiles", "Remark", ""].into_iter().enumerate() {
                            h.col(|ui| {
                                let text = RichText::new(t).size(12.5).strong().color(p.dim);
                                if i == 1 {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.add_space(COUNT_INSET);
                                        ui.label(text)
                                    });
                                } else {
                                    ui.label(text);
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(ROW_H, entries.len(), |mut row| {
                            let g = entries[row.index()];
                            let id = g.map_or("0".to_owned(), |g| g.group_id.clone());
                            let n = counts.get(&id).copied().unwrap_or(0);
                            let editing = rename_id.as_deref() == Some(id.as_str()) && g.is_some();
                            row.col(|ui| match (g, st.rename.as_mut().filter(|_| editing)) {
                                (_, Some(r)) => {
                                    ui.add(TextEdit::singleline(&mut r.name).desired_width(f32::INFINITY));
                                }
                                (Some(g), None) => {
                                    ui.label(RichText::new(&g.group_name).strong());
                                }
                                (None, None) => {
                                    ui.label(RichText::new("Ungrouped").italics().color(p.dim));
                                }
                            });
                            row.col(|ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add_space(COUNT_INSET);
                                    ui.label(format!("{n}"));
                                });
                            });
                            row.col(|ui| match (g, st.rename.as_mut().filter(|_| editing)) {
                                (_, Some(r)) => {
                                    ui.add(TextEdit::singleline(&mut r.remark).desired_width(f32::INFINITY));
                                }
                                (Some(g), None) => {
                                    ui.label(dim(&p, g.remark.clone()));
                                }
                                (None, None) => {
                                    ui.label(dim(&p, "Profiles that are not in any group"));
                                }
                            });
                            row.col(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                ui.spacing_mut().button_padding = vec2(8.0, 2.0);
                                if editing {
                                    if widgets::primary_button(ui, &p, "Save", true).clicked() {
                                        acts.push(Act::SaveRename);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        acts.push(Act::CancelRename);
                                    }
                                    return;
                                }
                                if ui.button("View").on_hover_text("Show these profiles").clicked() {
                                    acts.push(Act::View(id.clone()));
                                }
                                if let Some(g) = g {
                                    if ui.button("Settings").on_hover_text("Name, start pages and extensions").clicked()
                                    {
                                        acts.push(Act::StartRename(g.clone()));
                                    }
                                    if ui.button(RichText::new("Delete").color(p.err)).clicked() {
                                        acts.push(Act::Delete(g.group_id.clone()));
                                    }
                                }
                            });
                        });
                    });
            });
        if self.groups.is_empty() {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No groups yet").size(16.0).strong());
                ui.label(dim(
                    &p,
                    "Type a name above and press Create group; then move profiles into it from the Profiles page.",
                ));
            });
        }
        if self.groups_ui.rename.is_some() {
            self.group_settings_modal(ui.ctx(), &mut acts);
        }
        for a in acts {
            self.groups_act(a, &counts);
        }
    }

    /// Name, remark, the group's start pages and its extensions.
    fn group_settings_modal(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        let p = self.pal;
        let exts = self.extensions_ui.list.clone().unwrap_or_default();
        let Some(r) = self.groups_ui.rename.as_mut() else { return };
        let mut close = false;
        let resp = widgets::modal(ctx, &p, "group-settings", 560.0, |ui| {
            ui.label(widgets::semibold("Group settings", 18.0));
            ui.add_space(12.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            widgets::field_label(ui, &p, "Name");
            ui.add(
                TextEdit::singleline(&mut r.name).margin(egui::Margin::symmetric(10, 8)).desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Remark");
            ui.add(
                TextEdit::singleline(&mut r.remark).margin(egui::Margin::symmetric(10, 8)).desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Start pages");
            widgets::capped(
                ui,
                "tabs_text-box",
                TextEdit::multiline(&mut r.tabs_text)
                    .hint_text("https://www.tiktok.com/")
                    .margin(egui::Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.label(widgets::small_dim(
                &p,
                "One per line. Every profile of the group opens them, before its own start pages.",
            ));
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Bookmarks");
            widgets::capped(
                ui,
                "bookmarks_text-box",
                TextEdit::multiline(&mut r.bookmarks_text)
                    .hint_text("TikTok Ads | https://ads.tiktok.com/")
                    .margin(egui::Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.label(widgets::small_dim(
                &p,
                "Name | URL, one per line: on the bookmarks bar of every profile in the group.",
            ));
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Extensions");
            if exts.is_empty() {
                ui.label(widgets::small_dim(&p, "No extensions installed yet: add them on the Extensions page."));
            }
            for e in &exts {
                if e.all_profiles {
                    let mut on = true;
                    ui.add_enabled(false, egui::Checkbox::new(&mut on, format!("{}  (every profile)", e.name)));
                } else {
                    let slot = match r.extensions.iter().position(|(id, _)| *id == e.id) {
                        Some(i) => i,
                        None => {
                            r.extensions.push((e.id.clone(), e.groups.contains(&r.id)));
                            r.extensions.len() - 1
                        }
                    };
                    ui.checkbox(&mut r.extensions[slot].1, &e.name);
                }
            }
            widgets::footer(ui, |ui| {
                if widgets::primary_button(ui, &p, "Save", !r.name.trim().is_empty()).clicked() {
                    r.tabs = r.tabs_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
                    acts.push(Act::SaveRename);
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            acts.push(Act::CancelRename);
        }
    }

    fn group_counts(&self) -> HashMap<String, usize> {
        let mut m: HashMap<String, usize> = HashMap::new();
        for r in &self.rows {
            let id = if r.profile.group_id.is_empty() { "0" } else { r.profile.group_id.as_str() };
            *m.entry(id.to_owned()).or_default() += 1;
        }
        m
    }

    fn groups_act(&mut self, act: Act, counts: &HashMap<String, usize>) {
        match act {
            Act::View(id) => {
                self.profiles.query.group = Some(id);
                self.profiles.query.q.clear();
                self.go(Page::Profiles);
            }
            Act::StartRename(g) => {
                self.refresh_extensions();
                let tabs_text = g.tabs.join("\n");
                self.groups_ui.rename = Some(Rename {
                    id: g.group_id,
                    name: g.group_name,
                    remark: g.remark,
                    tabs: g.tabs,
                    tabs_text,
                    extensions: Vec::new(),
                    bookmarks_text: citro_core::bookmarks::to_lines(&g.bookmarks),
                });
            }
            Act::CancelRename => self.groups_ui.rename = None,
            Act::SaveRename => {
                let Some(r) = self.groups_ui.rename.take() else { return };
                let name = r.name.trim().to_owned();
                if name.is_empty() {
                    self.toast(super::app::ToastKind::Warn, "A group needs a name");
                    self.groups_ui.rename = Some(r);
                    return;
                }
                let gid = r.id.clone();
                // the extensions whose box changed: add or drop this group
                let exts = self.extensions_ui.list.clone().unwrap_or_default();
                let changes: Vec<(String, Vec<String>)> = r
                    .extensions
                    .iter()
                    .filter_map(|(id, on)| {
                        let e = exts.iter().find(|e| e.id == *id && !e.all_profiles)?;
                        let has = e.groups.contains(&gid);
                        (has != *on).then(|| {
                            let mut gs: Vec<String> = e.groups.iter().filter(|g| **g != gid).cloned().collect();
                            if *on {
                                gs.push(gid.clone());
                            }
                            (id.clone(), gs)
                        })
                    })
                    .collect();
                let g = Group {
                    group_id: r.id,
                    group_name: name.clone(),
                    remark: r.remark.trim().to_owned(),
                    tabs: r.tabs.clone(),
                    bookmarks: citro_core::bookmarks::parse_lines(&r.bookmarks_text),
                };
                self.run_op(Vec::new(), format!("Saved group {name}"), Refresh::ROWS_GROUPS, move |b| {
                    b.save_group(g)?;
                    for (id, gs) in changes {
                        b.assign_extension(&id, false, gs)?;
                    }
                    Ok(())
                });
                self.refresh_extensions();
            }
            Act::Delete(id) => {
                let name = group_label(&self.groups, &id);
                let n = counts.get(&id).copied().unwrap_or(0);
                let body = match n {
                    0 => "The group is empty; deleting it only removes the label.".to_owned(),
                    1 => "1 profile moves to Ungrouped. The profile itself is kept.".to_owned(),
                    n => format!("{n} profiles move to Ungrouped. The profiles themselves are kept."),
                };
                self.confirm = Some(Confirm {
                    title: format!("Delete group \u{201c}{name}\u{201d}?"),
                    body,
                    button: "Delete group".to_owned(),
                    danger: true,
                    action: ConfirmAction::DeleteGroup(id),
                });
            }
            Act::Create => {
                let s = &self.groups_ui;
                let g = Group {
                    group_id: String::new(),
                    group_name: s.new_name.trim().to_owned(),
                    remark: s.new_remark.trim().to_owned(),
                    tabs: Vec::new(),
                    bookmarks: Vec::new(),
                };
                if g.group_name.is_empty() {
                    return;
                }
                self.tasks.spawn(true, move |w| w.send(Msg::GroupCreated(w.backend.save_group(g))));
            }
        }
    }
}
