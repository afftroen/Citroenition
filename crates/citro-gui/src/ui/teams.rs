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
    /// A pull or a disconnect is running.
    pub working: bool,
    pub error: Option<String>,
    url: String,
    /// Only needed when the account is in several teams.
    team: String,
    token: String,
    device: String,
    seeded: bool,
    tried: bool,
    invite_name: String,
    invite_role: crate::backend::Role,
    inviting: bool,
    invite_error: Option<String>,
    /// The token to hand over, shown once: (name, token, a new account?).
    pub handover: Option<(String, String, bool)>,
    /// The hub's version and update state (asked when the page opens), and when it was last asked.
    upgrade: Option<Result<Option<crate::backend::HubUpgrade>, String>>,
    upgrade_asked: Option<std::time::Instant>,
    /// "Update hub" was pressed: the page asks again every few seconds until it is done.
    upgrading: bool,
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
        let (mut pull, mut disconnect) = (false, false);
        widgets::tab_header(ui, &p, &["Teams"], 0, |ui| {
            if widgets::button(ui, "Disconnect", !self.teams_ui.working).clicked() {
                disconnect = true;
            }
            if widgets::button(ui, "Pull now", !self.teams_ui.working)
                .on_hover_text("Fetch the team's newest profiles, remarks and groups")
                .clicked()
            {
                pull = true;
            }
            if self.teams_ui.working {
                ui.add(egui::Spinner::new().size(16.0));
            }
        });
        ui.horizontal(|ui| {
            widgets::dot(ui, p.ok, true);
            ui.label(RichText::new("Connected").color(p.ok));
            ui.label(small_dim(&p, format!("{}  \u{b7}  {} on {}", hub.url, hub.user, hub.device_name)));
        });
        if pull {
            self.teams_ui.working = true;
            self.tasks.spawn(true, |w| w.send(Msg::Pulled(w.backend.pull_team())));
        }
        if disconnect {
            self.confirm_disconnect();
        }
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
                    for team in t.clone() {
                        self.team_card(ui, &p, &team, &hub.user);
                        ui.add_space(12.0);
                    }
                }
            }
            unlock = self.locks_card(ui, &p, can_unlock);
            ui.add_space(12.0);
            self.hub_card(ui, &p);
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

    pub fn hub_upgrade_loaded(&mut self, r: crate::backend::BResult<Option<crate::backend::HubUpgrade>>) {
        let s = &mut self.teams_ui;
        if s.upgrading
            && let Ok(Some(u)) = &r
        {
            let at_latest = u.latest.as_deref().is_some_and(|l| l == u.version);
            if u.state == "failed" || (at_latest && !u.requested && u.state != "running") {
                s.upgrading = false;
            }
        }
        s.upgrade = Some(r.map_err(|e| e.to_string()));
    }

    fn ask_hub_upgrade(&mut self) {
        self.teams_ui.upgrade_asked = Some(std::time::Instant::now());
        self.tasks.spawn(false, |w| w.send(Msg::HubUpgrade(w.backend.hub_upgrade())));
    }

    /// The sync server: its version, the newest release and (for hub operators) the update button.
    fn hub_card(&mut self, ui: &mut Ui, p: &Palette) {
        let every = if self.teams_ui.upgrading { 4 } else { 300 };
        if self.teams_ui.upgrade_asked.is_none_or(|t| t.elapsed().as_secs() >= every) {
            self.ask_hub_upgrade();
        }
        if self.teams_ui.upgrading {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        let mut press = false;
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Sync server").size(17.0).strong());
            ui.add_space(6.0);
            match &self.teams_ui.upgrade {
                None | Some(Ok(None)) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(dim(p, "Asking the hub\u{2026}"));
                    });
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("The hub did not answer: {e}")).color(p.warn));
                }
                Some(Ok(Some(u))) => {
                    let newer = u
                        .latest
                        .as_deref()
                        .is_some_and(|l| citro_install::compare_versions(l, &u.version) == std::cmp::Ordering::Greater);
                    ui.horizontal(|ui| {
                        ui.label(dim(p, "Hub version"));
                        ui.label(RichText::new(if u.version.is_empty() { "unknown" } else { &u.version }).strong());
                        match &u.latest {
                            Some(l) if newer => {
                                widgets::badge(ui, &format!("{l} is out"), p.warn);
                            }
                            Some(_) => {
                                widgets::badge(ui, "up to date", p.ok);
                            }
                            None => {
                                ui.label(small_dim(p, "(could not ask GitHub for the newest release)"));
                            }
                        }
                    });
                    if self.teams_ui.upgrading || u.requested || u.state == "running" {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(dim(
                                p,
                                "Updating: the hub restarts and is back in a minute; team members stay signed in.",
                            ));
                        });
                    } else if u.state == "failed" {
                        ui.label(
                            RichText::new(format!("The last update failed: {}", u.message)).color(p.err).size(12.5),
                        );
                    } else if u.state == "done" && !u.message.is_empty() {
                        ui.label(small_dim(p, format!("Last update: {}", u.message)));
                    }
                    if !u.operator {
                        ui.label(small_dim(p, "Only the person who runs the hub (a hub operator) can update it."));
                    } else if !u.helper {
                        ui.label(small_dim(
                            p,
                            "This server was set up before the hub could update itself. Run deploy/install-hub.sh \
                             on it once (no arguments); after that this button updates it.",
                        ));
                    } else if newer && !self.teams_ui.upgrading && !u.requested && u.state != "running" {
                        let label = format!("Update hub to {}", u.latest.clone().unwrap_or_default());
                        if widgets::primary_button(ui, p, &label, true).clicked() {
                            press = true;
                        }
                        ui.label(small_dim(
                            p,
                            "The server downloads the release, checks it against its published checksums and restarts \
                             the hub; its data stays.",
                        ));
                    }
                }
            }
        });
        if press {
            self.teams_ui.upgrading = true;
            self.tasks.spawn(true, |w| {
                let r = w.backend.request_hub_upgrade();
                w.send(Msg::HubUpgrade(r.and_then(|_| w.backend.hub_upgrade())));
            });
        }
    }

    pub fn member_token(&mut self, name: String, invited: bool, r: crate::backend::BResult<String>) {
        let s = &mut self.teams_ui;
        s.inviting = false;
        match r {
            Ok(token) => {
                s.handover = Some((name, token, invited));
                s.invite_name.clear();
                s.invite_error = None;
                self.refresh_teams();
            }
            Err(e) => s.invite_error = Some(e.to_string()),
        }
    }

    /// The token, once, with everything the new person fills in.
    pub fn handover_modal(&mut self, ctx: &egui::Context) {
        let Some((name, token, invited)) = self.teams_ui.handover.clone() else { return };
        let p = self.pal;
        let team = self.teams.as_ref().and_then(|t| t.first()).map(|t| t.name.clone()).unwrap_or_default();
        let url = self.hub.url.clone();
        let text = format!("Hub address: {url}\nToken: {token}\nTeam: {team}");
        let mut close = false;
        let r = widgets::modal(ctx, &p, "member-token", 560.0, |ui| {
            ui.label(widgets::semibold(
                if invited { format!("{name} is invited") } else { format!("A new token for {name}") },
                18.0,
            ));
            ui.add_space(6.0);
            ui.label(dim(
                &p,
                format!(
                    "Send this to {name} privately: the token works like a password and is shown only now. They \
                     fill it in on Citroenition's Teams page.{}",
                    if invited { "" } else { " Their old token no longer works." }
                ),
            ));
            ui.add_space(10.0);
            egui::Frame::NONE
                .fill(p.raised)
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    Grid::new("handover").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                        for (k, v) in
                            [("Hub address", url.as_str()), ("Token", token.as_str()), ("Team", team.as_str())]
                        {
                            ui.label(dim(&p, k));
                            ui.add(egui::Label::new(mono(v)).selectable(true).wrap());
                            ui.end_row();
                        }
                    });
                });
            widgets::footer(ui, |ui| {
                if widgets::primary_button(ui, &p, "Copy and close", true).clicked() {
                    ui.ctx().copy_text(text.clone());
                    close = true;
                }
                if widgets::button(ui, "Copy", true).clicked() {
                    ui.ctx().copy_text(text.clone());
                }
            });
        });
        if close || r.should_close() {
            self.teams_ui.handover = None;
        }
    }

    fn team_card(&mut self, ui: &mut Ui, p: &Palette, team: &TeamInfo, me: &str) {
        use crate::backend::Role;
        let admin = matches!(team.role, Role::Owner | Role::Admin);
        let owner = team.role == Role::Owner;
        let mut act: Option<(&'static str, crate::backend::Member)> = None;
        let mut role_change: Option<(String, Role)> = None;
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
                        // owners and admins manage the others (only owners touch owners)
                        let manage = admin && m.name != me && !m.id.is_empty() && (owner || m.role != Role::Owner);
                        if manage {
                            ui.horizontal(|ui| {
                                let mut r = m.role;
                                egui::ComboBox::from_id_salt(("member-role", &m.id))
                                    .width(110.0)
                                    .icon(widgets::chevron_icon)
                                    .selected_text(r.label())
                                    .show_ui(ui, |ui| {
                                        for x in [Role::Viewer, Role::Member, Role::Admin, Role::Owner] {
                                            if x != Role::Owner || owner {
                                                ui.selectable_value(&mut r, x, x.label());
                                            }
                                        }
                                    });
                                if r != m.role {
                                    role_change = Some((m.name.clone(), r));
                                }
                                if ui.small_button("New token").on_hover_text("For someone who lost theirs").clicked() {
                                    act = Some(("token", m.clone()));
                                }
                                if ui.small_button(RichText::new("Remove").color(p.err)).clicked() {
                                    act = Some(("remove", m.clone()));
                                }
                            });
                        } else {
                            widgets::badge(ui, m.role.label(), col);
                        }
                        ui.end_row();
                    }
                },
            );
            if admin {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(widgets::semibold("Invite someone", 14.0));
                let s = &mut self.teams_ui;
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(&mut s.invite_name)
                            .hint_text("their name, e.g. ben or ben@shop.com")
                            .margin(egui::Margin::symmetric(10, 7))
                            .desired_width(240.0),
                    );
                    egui::ComboBox::from_id_salt("invite-role")
                        .width(110.0)
                        .icon(widgets::chevron_icon)
                        .selected_text(s.invite_role.label())
                        .show_ui(ui, |ui| {
                            for x in [Role::Viewer, Role::Member, Role::Admin, Role::Owner] {
                                if x != Role::Owner || owner {
                                    ui.selectable_value(&mut s.invite_role, x, x.label());
                                }
                            }
                        });
                    let ready = !s.invite_name.trim().is_empty() && !s.inviting;
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if widgets::primary_button(ui, p, "Invite", ready).clicked() || (enter && ready) {
                        s.inviting = true;
                        s.invite_error = None;
                        let (team_id, name, role) = (team.id.clone(), s.invite_name.trim().to_owned(), s.invite_role);
                        self.tasks.spawn(true, move |w| {
                            let result = w.backend.invite_member(&team_id, &name, role);
                            w.send(Msg::MemberToken { name, invited: true, result });
                        });
                    }
                    if s.inviting {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                });
                if let Some(e) = &self.teams_ui.invite_error {
                    ui.label(RichText::new(e).color(p.err).size(12.5));
                } else {
                    ui.label(small_dim(
                        p,
                        "Makes their account on the hub and shows the token to send them. Viewers look, members \
                         open and edit, admins also manage people and group rules, owners everything.",
                    ));
                }
            }
        });
        if let Some((name, role)) = role_change {
            let team_id = team.id.clone();
            self.run_op(
                Vec::new(),
                format!("{name} is now {}", role.label()),
                super::tasks::Refresh::TEAMS,
                move |b| b.set_member_role(&team_id, &name, role),
            );
        }
        if let Some((what, m)) = act {
            let (tid, uid, name) = (team.id.clone(), m.id.clone(), m.name.clone());
            self.confirm = Some(if what == "remove" {
                Confirm {
                    title: format!("Remove {name} from the team?"),
                    body: format!("{name} no longer sees the team's profiles; profiles they have open are freed."),
                    button: "Remove".to_owned(),
                    danger: true,
                    action: ConfirmAction::RemoveMember(tid, uid, name),
                }
            } else {
                Confirm {
                    title: format!("A new token for {name}?"),
                    body: format!("{name}'s current token stops working at once; you get a new one to send them."),
                    button: "New token".to_owned(),
                    danger: false,
                    action: ConfirmAction::NewMemberToken(tid, uid, name),
                }
            });
        }
    }

    fn locks(&self) -> Vec<Lock> {
        let mut out: Vec<Lock> = self
            .rows
            .iter()
            .filter_map(|r| match &r.run {
                RunState::Open { since, .. } if r.team.is_some() => Some(Lock {
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
        widgets::tab_header(ui, &p, &["Teams"], 0, |_| {});
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
                        ui.add_space(8.0);
                        widgets::field_label(ui, &p, "Team (only if you are in several)");
                        ui.add(
                            TextEdit::singleline(&mut st.team)
                                .hint_text("leave empty for your only team")
                                .desired_width(f32::INFINITY),
                        );
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
                    ui.label(small_dim(
                        &p,
                        "The token is kept in this computer's Citroenition folder, readable only by you.",
                    ));
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
        let (url, token, device, team) =
            (st.url.trim().to_owned(), st.token.clone(), st.device.trim().to_owned(), st.team.trim().to_owned());
        self.tasks.spawn(true, move |w| {
            w.send(Msg::HubConnected(w.backend.connect_hub(&url, &token, &device, &team)));
        });
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
