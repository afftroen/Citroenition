//! The Teams page. Connected to a hub it lists the team's members with their roles and every profile lock (who has
//! what open, since when) with a force-unlock for owners and admins. Without a hub it is a clean "not connected"
//! state with the connect form; the access token is typed here, used once and never written to disk.

use super::app::CitroApp;
use super::dialogs::{Confirm, ConfirmAction};
use super::tasks::Msg;
use super::theme::Palette;
use super::widgets::{self, dim, mono, small_dim};
use crate::backend::{RunState, TeamInfo};
use crate::{timefmt, util};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Grid, Key, Layout, RichText, ScrollArea, Sense, TextEdit, Ui, vec2,
};
use egui_extras::{Column, TableBuilder};

#[derive(Default)]
pub struct State {
    pub connecting: bool,
    pub error: Option<String>,
    url: String,
    token: String,
    device: String,
    seeded: bool,
    tried: bool,
}

impl State {
    /// The token is dropped as soon as the hub accepted it.
    pub fn connected(&mut self) {
        self.token.clear();
        self.tried = false;
    }

    fn url_problem(&self) -> Option<&'static str> {
        let u = self.url.trim();
        let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://"));
        match rest {
            None => Some("The address must start with https:// (or http:// on a local network)."),
            Some(r) if r.trim_matches('/').is_empty() || r.contains(char::is_whitespace) => {
                Some("Add the hub's host name after https://.")
            }
            Some(_) => None,
        }
    }

    fn problem(&self) -> Option<&'static str> {
        if let Some(p) = self.url_problem() {
            return Some(p);
        }
        if self.token.trim().is_empty() {
            return Some("Paste the access token your hub admin gave you.");
        }
        if self.device.trim().is_empty() {
            return Some("Give this device a name teammates will recognise.");
        }
        None
    }
}

fn role_color(p: &Palette, r: crate::backend::Role) -> Color32 {
    use crate::backend::Role;
    match r {
        Role::Owner => p.accent,
        Role::Admin => p.locked,
        Role::Member => p.ok,
        Role::Viewer => p.idle,
    }
}

/// A round avatar with the first letter of the name.
fn avatar(ui: &mut Ui, p: &Palette, name: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 13.0, color.gamma_multiply(if p.dark { 0.28 } else { 0.18 }));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, util::initial(name), FontId::proportional(13.0), color);
}

struct Lock {
    id: String,
    name: String,
    holder: String,
    device: String,
    since: i64,
    mine: bool,
}

impl CitroApp {
    pub fn teams_page(&mut self, ui: &mut Ui) {
        if !self.teams_ui.seeded {
            let s = &self.settings;
            self.teams_ui.url = if self.hub.url.is_empty() { s.hub_url.clone() } else { self.hub.url.clone() };
            self.teams_ui.device = s.device_name.clone();
            self.teams_ui.seeded = true;
        }
        if self.hub.connected {
            self.teams_connected(ui);
        } else {
            self.teams_form(ui);
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // connected

    fn teams_connected(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let hub = self.hub.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Teams").size(22.0).strong());
            ui.add_space(8.0);
            widgets::dot(ui, p.ok, true);
            ui.label(RichText::new("Connected").color(p.ok));
            ui.label(small_dim(&p, format!("{}  \u{b7}  {} on {}", hub.url, hub.user, hub.device_name)));
        });
        ui.label(dim(&p, "Shared profiles, who may do what, and who has a profile open right now."));
        ui.add_space(10.0);
        let teams = self.teams.clone();
        let can_unlock = teams.as_ref().is_some_and(|t| t.iter().any(|t| t.role.can_force_unlock()));
        let mut unlock: Option<Lock> = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            match (&teams, &self.teams_error) {
                (_, Some(e)) => {
                    ui.label(RichText::new(format!("Could not load the team: {e}")).color(p.err));
                }
                (None, None) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0));
                        ui.label(dim(&p, "Loading the team\u{2026}"));
                    });
                }
                (Some(t), None) if t.is_empty() => {
                    widgets::empty_state(
                        ui,
                        &p,
                        "\u{1f465}",
                        "You are not in a team yet",
                        "Ask a team owner to invite this account, then press F5.",
                    );
                }
                (Some(t), None) => {
                    for team in t {
                        self.team_card(ui, &p, team, &hub.user);
                        ui.add_space(12.0);
                    }
                }
            }
            unlock = self.locks_card(ui, &p, can_unlock);
        });
        if let Some(l) = unlock {
            self.confirm = Some(Confirm {
                title: format!("Force-unlock {}?", l.name),
                body: format!(
                    "{} on {} has this profile open. Unlocking it lets you open it here, but their browser may \
                     still be running and two sessions can overwrite each other's cookies. Only continue when you \
                     know their browser is closed.",
                    l.holder, l.device
                ),
                button: "Force unlock".to_owned(),
                danger: true,
                action: ConfirmAction::ForceUnlock(l.id),
            });
        }
    }

    fn team_card(&self, ui: &mut Ui, p: &Palette, team: &TeamInfo, me: &str) {
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&team.name).size(17.0).strong());
                widgets::badge(ui, &format!("Your role: {}", team.role.label()), role_color(p, team.role));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let n = team.members.len();
                    let profiles = team.profile_count;
                    ui.label(dim(
                        p,
                        format!("{n} member{}  \u{b7}  {profiles} profiles", if n == 1 { "" } else { "s" }),
                    ));
                });
            });
            ui.add_space(8.0);
            Grid::new(("team-members", &team.id)).num_columns(3).spacing([14.0, 8.0]).min_col_width(26.0).show(
                ui,
                |ui| {
                    for m in &team.members {
                        let col = role_color(p, m.role);
                        avatar(ui, p, &m.name, col);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&m.name).strong());
                            if m.name == me {
                                ui.label(small_dim(p, "you"));
                            }
                        });
                        widgets::badge(ui, m.role.label(), col);
                        ui.end_row();
                    }
                },
            );
        });
    }

    fn locks(&self) -> Vec<Lock> {
        let mut out: Vec<Lock> = self
            .rows
            .iter()
            .filter_map(|r| match &r.run {
                RunState::Open { since, .. } => Some(Lock {
                    id: r.id().to_owned(),
                    name: r.profile.name.clone(),
                    holder: self.hub.user.clone(),
                    device: self.hub.device_name.clone(),
                    since: *since,
                    mine: true,
                }),
                RunState::LockedElsewhere { user, device, since } => Some(Lock {
                    id: r.id().to_owned(),
                    name: r.profile.name.clone(),
                    holder: user.clone(),
                    device: device.clone(),
                    since: *since,
                    mine: false,
                }),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| a.mine.cmp(&b.mine).then(b.since.cmp(&a.since)).then(a.name.cmp(&b.name)));
        out
    }

    fn locks_card(&self, ui: &mut Ui, p: &Palette, can_unlock: bool) -> Option<Lock> {
        let locks = self.locks();
        let mut chosen: Option<Lock> = None;
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Profile locks").size(17.0).strong());
                ui.label(dim(p, locks.len().to_string()));
            });
            ui.label(small_dim(p, "A profile can be open on one device at a time; everybody else sees it as locked."));
            ui.add_space(8.0);
            if locks.is_empty() {
                ui.label(dim(p, "No profile is open anywhere right now."));
                return;
            }
            let row_h = 38.0;
            TableBuilder::new(ui)
                .id_salt("team-locks")
                .striped(true)
                .vscroll(false)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(150.0))
                .column(Column::exact(160.0))
                .column(Column::exact(130.0))
                .column(Column::exact(170.0))
                .header(26.0, |mut h| {
                    for t in ["Profile", "Held by", "Device", "Open since", ""] {
                        h.col(|ui| {
                            ui.label(small_dim(p, t));
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, locks.len(), |mut row| {
                        let l = &locks[row.index()];
                        row.col(|ui| {
                            ui.label(RichText::new(&l.name).strong());
                        });
                        row.col(|ui| {
                            if l.mine {
                                ui.label(format!("{} (you)", l.holder));
                            } else {
                                ui.label(RichText::new(&l.holder).color(p.locked));
                            }
                        });
                        row.col(|ui| {
                            ui.label(mono(&l.device));
                        });
                        row.col(|ui| {
                            ui.label(timefmt::since(self.now, l.since, self.offset))
                                .on_hover_text(timefmt::absolute(l.since, self.offset));
                        });
                        row.col(|ui| {
                            if l.mine {
                                ui.label(small_dim(p, "close it from Profiles"));
                                return;
                            }
                            let r = ui.add_enabled(can_unlock, egui::Button::new("Force unlock"));
                            let r = if can_unlock {
                                r
                            } else {
                                r.on_disabled_hover_text("Only team owners and admins can do this")
                            };
                            if r.clicked() {
                                chosen = Some(Lock {
                                    id: l.id.clone(),
                                    name: l.name.clone(),
                                    holder: l.holder.clone(),
                                    device: l.device.clone(),
                                    since: l.since,
                                    mine: false,
                                });
                            }
                        });
                    });
                });
        });
        chosen
    }

    // ----------------------------------------------------------------------------------------------------------
    // not connected

    fn teams_form(&mut self, ui: &mut Ui) {
        let p = self.pal;
        ui.label(RichText::new("Teams").size(22.0).strong());
        ui.add_space(6.0);
        let width = 520.0_f32.min(ui.available_width());
        let pad = ((ui.available_width() - width) / 2.0).max(0.0);
        let mut connect = false;
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.add_space(28.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Not connected to a team hub").size(20.0).strong());
                });
                ui.add_space(4.0);
                ui.label(dim(
                    &p,
                    "A hub shares profiles between devices, locks a profile while somebody has it open and keeps one \
                     history of who did what. Until you connect, everything stays on this computer.",
                ));
                ui.add_space(14.0);
                widgets::card(ui, &p, |ui| {
                    ui.set_width(ui.available_width());
                    let st = &mut self.teams_ui;
                    let busy = st.connecting;
                    ui.add_enabled_ui(!busy, |ui| {
                        widgets::field_label(ui, &p, "Hub address");
                        ui.add(
                            TextEdit::singleline(&mut st.url)
                                .hint_text("https://hub.example.net")
                                .desired_width(f32::INFINITY),
                        );
                        if st.tried
                            && let Some(m) = st.url_problem()
                        {
                            ui.label(RichText::new(m).color(p.err).size(12.0));
                        }
                        ui.add_space(8.0);
                        widgets::field_label(ui, &p, "Access token");
                        let r = ui.add(
                            TextEdit::singleline(&mut st.token)
                                .password(true)
                                .hint_text("paste the token from your hub admin")
                                .desired_width(f32::INFINITY),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            connect = true;
                        }
                        ui.add_space(8.0);
                        widgets::field_label(ui, &p, "This device is called");
                        ui.add(TextEdit::singleline(&mut st.device).desired_width(f32::INFINITY));
                    });
                    if st.tried
                        && !busy
                        && let Some(m) = st.problem()
                        && st.url_problem().is_none()
                    {
                        ui.add_space(4.0);
                        ui.label(RichText::new(m).color(p.err).size(12.0));
                    }
                    if let Some(e) = &st.error {
                        ui.add_space(6.0);
                        ui.label(RichText::new(e).color(p.err));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, &p, "Connect", !busy).clicked() {
                            connect = true;
                        }
                        if busy {
                            ui.add(egui::Spinner::new().size(16.0));
                            ui.label(dim(&p, "Connecting\u{2026}"));
                        }
                    });
                    ui.add_space(2.0);
                    ui.label(small_dim(&p, "This app does not save the access token."));
                });
            });
        });
        if connect {
            self.teams_connect();
        }
    }

    fn teams_connect(&mut self) {
        let st = &mut self.teams_ui;
        st.tried = true;
        if st.connecting || st.problem().is_some() {
            return;
        }
        st.connecting = true;
        st.error = None;
        let (url, token, device) = (st.url.trim().to_owned(), st.token.clone(), st.device.trim().to_owned());
        self.tasks.spawn(true, move |w| w.send(Msg::HubConnected(w.backend.connect_hub(&url, &token, &device))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_names_what_is_missing() {
        let mut st = State::default();
        assert!(st.url_problem().unwrap().contains("https://"));
        st.url = "hub.example.net".into();
        assert!(st.url_problem().is_some());
        st.url = "https://".into();
        assert!(st.url_problem().unwrap().contains("host"));
        st.url = "https://hub.example.net".into();
        assert!(st.url_problem().is_none());
        assert!(st.problem().unwrap().contains("token"));
        st.token = "tok".into();
        assert!(st.problem().unwrap().contains("device"));
        st.device = "MIA-LAPTOP".into();
        assert!(st.problem().is_none());
        st.connected();
        assert!(st.token.is_empty());
    }
}
