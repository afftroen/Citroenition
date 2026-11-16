//! The small modal dialogs: confirm, move to group, set remark and the "could not open" report.

use super::app::{CitroApp, Page, ToastKind};
use super::tasks::Refresh;
use super::widgets::{self, mono};
use crate::backend::group_label;
use citro_core::profile::Group;
use eframe::egui::{self, ComboBox, Key, RichText, ScrollArea, TextEdit};

pub enum ConfirmAction {
    DeleteProfiles(Vec<String>),
    DeleteGroup(String),
    ClearCache(Vec<String>),
    ForceUnlock(String),
}

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub button: String,
    pub danger: bool,
    pub action: ConfirmAction,
}

pub struct MoveDialog {
    pub ids: Vec<String>,
    /// Group id; `"0"` = Ungrouped.
    pub target: String,
    pub new_name: String,
}

impl MoveDialog {
    pub fn new(ids: Vec<String>) -> MoveDialog {
        MoveDialog { ids, target: "0".to_owned(), new_name: String::new() }
    }
}

pub struct RemarkDialog {
    pub ids: Vec<String>,
    pub text: String,
}

/// A refused or failed open: the headline plus the tail of the kernel log that came with it.
pub struct OpenFailure {
    pub id: String,
    pub name: String,
    pub message: String,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

impl CitroApp {
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        if !self.failures.is_empty() {
            self.failure_modal(ctx);
        } else if self.confirm.is_some() {
            self.confirm_modal(ctx);
        } else if self.move_dlg.is_some() {
            self.move_modal(ctx);
        } else if self.remark_dlg.is_some() {
            self.remark_modal(ctx);
        } else if self.edit.is_some() {
            self.edit_modal(ctx);
        } else if self.export.is_some() {
            self.export_modal(ctx);
        }
    }

    pub fn confirm_delete_profiles(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let what = if ids.len() == 1 {
            format!("\"{}\"", self.name_of(&ids[0]))
        } else {
            plural(ids.len(), "profile", "profiles")
        };
        self.confirm = Some(Confirm {
            title: format!("Delete {what}?"),
            body:
                "The profile, its cookies, cache and saved logins are removed from this device. This cannot be undone."
                    .to_owned(),
            button: "Delete".to_owned(),
            danger: true,
            action: ConfirmAction::DeleteProfiles(ids),
        });
    }

    pub fn confirm_clear_cache(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let what = if ids.len() == 1 {
            format!("\"{}\"", self.name_of(&ids[0]))
        } else {
            plural(ids.len(), "profile", "profiles")
        };
        self.confirm = Some(Confirm {
            title: format!("Clear the cache of {what}?"),
            body: "Cached files are removed so pages load fresh. Close the browser first; the profile itself stays."
                .to_owned(),
            button: "Clear cache".to_owned(),
            danger: false,
            action: ConfirmAction::ClearCache(ids),
        });
    }

    fn run_confirm(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        match c.action {
            ConfirmAction::DeleteProfiles(ids) => {
                let ok = format!("Deleted {}", plural(ids.len(), "profile", "profiles"));
                self.profiles.selection.clear();
                let ids2 = ids.clone();
                self.run_op(ids, ok, Refresh::ROWS_GROUPS, move |b| b.delete_profiles(&ids2));
            }
            ConfirmAction::ClearCache(ids) => {
                let ok = format!("Cleared the cache of {}", plural(ids.len(), "profile", "profiles"));
                let ids2 = ids.clone();
                self.run_op(ids, ok, Refresh::ROWS, move |b| ids2.iter().try_for_each(|id| b.clear_cache(id)));
            }
            ConfirmAction::DeleteGroup(id) => {
                let name = group_label(&self.groups, &id);
                if self.profiles.query.group.as_deref() == Some(id.as_str()) {
                    self.profiles.query.group = None;
                }
                self.run_op(Vec::new(), format!("Deleted group {name}"), Refresh::ROWS_GROUPS, move |b| {
                    b.delete_group(&id)
                });
            }
            ConfirmAction::ForceUnlock(id) => {
                let name = self.name_of(&id);
                let id2 = id.clone();
                self.run_op(vec![id], format!("Unlocked {name}"), Refresh::TEAMS, move |b| b.force_unlock(&id2));
            }
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let p = self.pal;
        let (title, body, button, danger) = (c.title.clone(), c.body.clone(), c.button.clone(), c.danger);
        let mut decision = None;
        let r = widgets::modal(ctx, &p, "confirm-modal", 420.0, |ui| {
            ui.label(RichText::new(&title).size(17.0).strong());
            ui.add_space(4.0);
            ui.label(widgets::dim(&p, body));
            widgets::footer(ui, |ui| {
                let go = if danger {
                    widgets::danger_button(ui, &p, &button, true)
                } else {
                    widgets::primary_button(ui, &p, &button, true)
                };
                if go.clicked() {
                    decision = Some(true);
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    decision = Some(false);
                }
            });
        });
        if r.should_close() {
            decision = decision.or(Some(false));
        }
        match decision {
            Some(true) => self.run_confirm(),
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn move_modal(&mut self, ctx: &egui::Context) {
        let Some(d) = self.move_dlg.take() else { return };
        let p = self.pal;
        let mut d = d;
        let groups: Vec<Group> = self.groups.clone();
        let mut decision = None;
        let r = widgets::modal(ctx, &p, "move-modal", 400.0, |ui| {
            ui.label(
                RichText::new(format!("Move {} to a group", plural(d.ids.len(), "profile", "profiles")))
                    .size(17.0)
                    .strong(),
            );
            ui.add_space(6.0);
            widgets::field_label(ui, &p, "Group");
            ComboBox::from_id_salt("move-target")
                .width(360.0)
                .selected_text(if d.target == "0" { "Ungrouped".to_owned() } else { group_label(&groups, &d.target) })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.target, "0".to_owned(), "Ungrouped");
                    for g in &groups {
                        ui.selectable_value(&mut d.target, g.group_id.clone(), &g.group_name);
                    }
                });
            ui.add_space(4.0);
            widgets::field_label(ui, &p, "…or create a new group");
            let new = ui.add(TextEdit::singleline(&mut d.new_name).hint_text("New group name").desired_width(360.0));
            let creating = !d.new_name.trim().is_empty();
            let enter = new.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            widgets::footer(ui, |ui| {
                let label = if creating { "Create & move" } else { "Move" };
                if widgets::primary_button(ui, &p, label, true).clicked() || enter {
                    decision = Some(true);
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    decision = Some(false);
                }
            });
        });
        if r.should_close() {
            decision = decision.or(Some(false));
        }
        match decision {
            Some(true) => {
                let ids = d.ids.clone();
                let n = ids.len();
                if d.new_name.trim().is_empty() {
                    let label = group_label(&self.groups, &d.target);
                    let target = d.target.clone();
                    let ids2 = ids.clone();
                    self.run_op(
                        ids,
                        format!("Moved {} to {label}", plural(n, "profile", "profiles")),
                        Refresh::ROWS_GROUPS,
                        move |b| b.move_to_group(&ids2, &target),
                    );
                } else {
                    let name = d.new_name.trim().to_owned();
                    let ids2 = ids.clone();
                    let name2 = name.clone();
                    self.run_op(
                        ids,
                        format!("Moved {} to new group {name}", plural(n, "profile", "profiles")),
                        Refresh::ROWS_GROUPS,
                        move |b| {
                            let g = b.save_group(Group {
                                group_id: String::new(),
                                group_name: name2,
                                remark: String::new(),
                            })?;
                            b.move_to_group(&ids2, &g.group_id)
                        },
                    );
                }
            }
            Some(false) => {}
            None => self.move_dlg = Some(d),
        }
    }

    fn remark_modal(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.remark_dlg.take() else { return };
        let p = self.pal;
        let mut decision = None;
        let r = widgets::modal(ctx, &p, "remark-modal", 440.0, |ui| {
            ui.label(
                RichText::new(format!("Remark for {}", plural(d.ids.len(), "profile", "profiles"))).size(17.0).strong(),
            );
            ui.add_space(6.0);
            let resp = ui.add(
                TextEdit::multiline(&mut d.text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("What is this profile for?"),
            );
            if !resp.has_focus() && ui.memory(|m| m.focused().is_none()) {
                resp.request_focus();
            }
            if d.ids.len() > 1 {
                ui.label(widgets::small_dim(&p, "The same remark is set on every selected profile."));
            }
            widgets::footer(ui, |ui| {
                if widgets::primary_button(ui, &p, "Save", true).clicked() {
                    decision = Some(true);
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    decision = Some(false);
                }
            });
        });
        if r.should_close() {
            decision = decision.or(Some(false));
        }
        match decision {
            Some(true) => {
                let (ids, text) = (d.ids.clone(), d.text.trim().to_owned());
                let n = ids.len();
                let ids2 = ids.clone();
                self.run_op(
                    ids,
                    format!("Remark saved for {}", plural(n, "profile", "profiles")),
                    Refresh::ROWS,
                    move |b| ids2.iter().try_for_each(|id| b.set_remark(id, &text)),
                );
            }
            Some(false) => {}
            None => self.remark_dlg = Some(d),
        }
    }

    fn failure_modal(&mut self, ctx: &egui::Context) {
        let p = self.pal;
        let f = &self.failures[0];
        let (id, name, message) = (f.id.clone(), f.name.clone(), f.message.clone());
        let more = self.failures.len() - 1;
        let headline = crate::util::first_line(&message).to_owned();
        let tail: String = message.lines().skip(1).collect::<Vec<_>>().join("\n");
        let mut decision = None;
        let mut copy = false;
        let r = widgets::modal(ctx, &p, "failure-modal", 640.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠").size(22.0).color(p.err));
                ui.label(RichText::new(format!("Could not open \"{name}\"")).size(17.0).strong());
            });
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&headline).color(p.text)).wrap());
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Browser kernel log (last lines)");
            let body =
                if tail.trim().is_empty() { "(the kernel wrote nothing to its log)".to_owned() } else { tail.clone() };
            egui::Frame::NONE
                .fill(if p.dark { egui::Color32::from_rgb(0x10, 0x12, 0x16) } else { p.raised })
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ScrollArea::both().max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                        ui.add(egui::Label::new(mono(&body).size(12.0).color(p.dim)).extend());
                    });
                });
            if more > 0 {
                ui.add_space(4.0);
                ui.label(widgets::small_dim(&p, format!("{more} more failure(s) waiting")));
            }
            widgets::footer(ui, |ui| {
                if widgets::primary_button(ui, &p, "Close", true).clicked() {
                    decision = Some(false);
                }
                if widgets::button(ui, "Open browser log", true).clicked() {
                    decision = Some(true);
                }
                if widgets::button(ui, "Copy details", true).clicked() {
                    copy = true;
                }
            });
        });
        if copy {
            ctx.copy_text(format!("{name} ({id}): {message}"));
            self.toast(ToastKind::Info, "Copied the error details");
        }
        if r.should_close() {
            decision = decision.or(Some(false));
        }
        if let Some(show_log) = decision {
            self.failures.remove(0);
            if show_log {
                self.logs_ui.show_browser_for(&id);
                self.go(Page::Logs);
            }
        }
    }
}
