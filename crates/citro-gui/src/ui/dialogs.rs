//! The small modal dialogs: confirm, move to group, set remark and the "could not open" report.

use super::app::{CitroApp, Page, ToastKind};
use super::tasks::{Msg, Refresh};
use super::widgets::{self, mono};
use crate::backend::{RunState, group_label};
use citro_core::profile::Group;
use eframe::egui::{self, ComboBox, Key, RichText, ScrollArea, TextEdit};

pub enum ConfirmAction {
    /// `true` = the selection holds team profiles: they go from the hub too, for everybody.
    DeleteProfiles(Vec<String>, bool),
    Share(Vec<String>),
    Disconnect,
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
        } else if self.proxy_dlg.is_some() {
            self.proxy_modal(ctx);
        } else if self.profiles.tags_for.is_some() {
            self.tags_modal(ctx);
        } else if self.update.dialog {
            self.update_modal(ctx);
        } else if self.export.is_some() {
            self.export_modal(ctx);
        }
    }

    /// The window stays open while the team browsers are closed and their data goes up to the hub: nothing else
    /// keeps their locks alive, and the next person must get the newest cookies.
    pub fn guard_quit(&mut self, ctx: &egui::Context) {
        use super::app::Quit;
        if self.quit == Quit::Idle && ctx.input(|i| i.viewport().close_requested()) {
            // every open browser is closed the way Chrome quits (SIGTERM, then it writes its cookies and the open
            // tabs to disk), so quitting really quits and the next launch restores the tabs
            let open: Vec<String> = self
                .rows
                .iter()
                .filter(|r| r.run.is_open() || r.run == RunState::Closing)
                .map(|r| r.profile.profile_id.clone())
                .collect();
            let team_open =
                self.rows.iter().any(|r| r.team.is_some() && (r.run.is_open() || r.run == RunState::Closing));
            if !open.is_empty() || team_open {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit = Quit::Working;
                self.quit_count = open.len();
                self.tasks.spawn(false, move |w| {
                    let mut lines = Vec::new();
                    for id in &open {
                        if let Err(e) = w.backend.close(id) {
                            lines.push(format!("{id}: {e}"));
                        }
                    }
                    lines.extend(w.backend.shutdown());
                    w.send(Msg::QuitDone(lines));
                });
            }
        }
        if self.quit == Quit::Working {
            let p = self.pal;
            let n = self.quit_count;
            widgets::modal(ctx, &p, "quit-modal", 420.0, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0));
                    let title = if n == 1 { "Closing 1 browser".to_owned() } else { format!("Closing {n} browsers") };
                    ui.label(RichText::new(title).size(17.0).strong());
                });
                ui.add_space(4.0);
                ui.label(widgets::dim(
                    &p,
                    "Each browser saves its cookies and open tabs first, so they are back the next time you open the \
                     profile. Team profiles also go up to the hub. Citroenition quits by itself when that is done.",
                ));
            });
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
        let team: Vec<String> = ids
            .iter()
            .filter_map(|id| self.row(id).filter(|r| r.team.is_some()).map(|r| r.profile.name.clone()))
            .collect();
        if team.is_empty() {
            self.confirm = Some(Confirm {
                title: format!("Delete {what}?"),
                body: "The profile, its cookies, cache and saved logins are removed from this device. This cannot be \
                       undone."
                    .to_owned(),
                button: "Delete".to_owned(),
                danger: true,
                action: ConfirmAction::DeleteProfiles(ids, false),
            });
            return;
        }
        // a team profile is the whole team's: deleting it takes it away from every member
        let which =
            if team.len() == 1 { format!("\"{}\" belongs", team[0]) } else { format!("{} of them belong", team.len()) };
        self.confirm = Some(Confirm {
            title: format!("Delete {what} for the whole team?"),
            body: format!(
                "{which} to the team hub. Deleting removes {} from the hub for EVERYBODY in the team: the profile, its \
                 cookies and saved logins are gone for all of them. This cannot be undone.",
                if team.len() == 1 { "it" } else { "them" }
            ),
            button: "Delete for everyone".to_owned(),
            danger: true,
            action: ConfirmAction::DeleteProfiles(ids, true),
        });
    }

    pub fn confirm_share(&mut self, ids: Vec<String>) {
        // only the ones that are not on the hub yet
        let ids: Vec<String> = ids.into_iter().filter(|id| self.row(id).is_some_and(|r| r.team.is_none())).collect();
        if ids.is_empty() {
            self.toast(ToastKind::Info, "These profiles are already shared with the team");
            return;
        }
        let team = if self.hub.team.is_empty() { "the team".to_owned() } else { self.hub.team.clone() };
        let what = if ids.len() == 1 {
            format!("\"{}\"", self.name_of(&ids[0]))
        } else {
            plural(ids.len(), "profile", "profiles")
        };
        self.confirm = Some(Confirm {
            title: format!("Share {what} with {team}?"),
            body:
                "The profile's settings, proxy, cookies and browser data are uploaded to the hub. Every member of the \
                   team sees it and can open it (one device at a time). Deleting a shared profile removes it for \
                   everybody."
                    .to_owned(),
            button: "Share".to_owned(),
            danger: false,
            action: ConfirmAction::Share(ids),
        });
    }

    pub fn confirm_disconnect(&mut self) {
        let open =
            self.rows.iter().filter(|r| r.team.is_some() && (r.run.is_open() || r.run == RunState::Closing)).count();
        if open > 0 {
            self.toast(
                ToastKind::Warn,
                format!(
                    "Close the {} before disconnecting: their data has to reach the hub first",
                    plural(open, "open team browser", "open team browsers")
                ),
            );
            return;
        }
        self.confirm = Some(Confirm {
            title: "Disconnect from the team hub?".to_owned(),
            body: "Team profiles stay on this computer but cannot be opened until you connect again. Nothing is \
                   deleted, here or on the hub."
                .to_owned(),
            button: "Disconnect".to_owned(),
            danger: false,
            action: ConfirmAction::Disconnect,
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

    #[cfg(test)]
    pub fn test_run_confirm(&mut self) {
        self.run_confirm();
    }

    fn run_confirm(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        match c.action {
            ConfirmAction::DeleteProfiles(ids, everywhere) => {
                let ok = format!("Deleted {}", plural(ids.len(), "profile", "profiles"));
                self.profiles.selection.clear();
                let ids2 = ids.clone();
                self.run_op(ids, ok, Refresh::TEAMS, move |b| {
                    if everywhere { b.delete_profiles_everywhere(&ids2) } else { b.delete_profiles(&ids2) }
                });
            }
            ConfirmAction::Share(ids) => {
                let team = if self.hub.team.is_empty() { "the team".to_owned() } else { self.hub.team.clone() };
                let ok = format!("Shared {} with {team}", plural(ids.len(), "profile", "profiles"));
                let ids2 = ids.clone();
                self.run_op(ids, ok, Refresh::TEAMS, move |b| b.share_profiles(&ids2).map(|_| ()));
            }
            ConfirmAction::Disconnect => {
                self.teams_ui.working = true;
                self.tasks.spawn(true, |w| w.send(Msg::HubDisconnected(w.backend.disconnect_hub())));
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
                .icon(widgets::chevron_icon)
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
                                tabs: Vec::new(),
                                bookmarks: Vec::new(),
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
