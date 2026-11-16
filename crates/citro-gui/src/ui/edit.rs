//! The New / Edit profile dialog: name, group, remark, proxy (one URL field kept in sync with type / host / port /
//! user / password), OS persona, country, start URLs and the read-only fingerprint seed.
//!
//! Secrets: the dialog only ever holds the *redacted* profile. A stored proxy password is a flag (`pass_stored`),
//! not text; saving sends the mask back and the backend restores the real value. `Check proxy` resolves the
//! stored password on the worker thread.

use super::app::{CitroApp, ToastKind};
use super::tasks::Msg;
use super::theme::Palette;
use super::widgets::{self, dim, small_dim};
use crate::backend::{MASK, ProxyCheck, group_label, redact};
use crate::countries;
use crate::proxy::{self, ProxyKind};
use citro_core::profile::{Group, Profile, ProxyConfig};
use eframe::egui::{self, ComboBox, Grid, Key, RichText, ScrollArea, TextEdit, Ui};

const OS_CHOICES: [(&str, &str); 4] =
    [("", "Auto (this computer's OS)"), ("windows", "Windows"), ("macos", "macOS"), ("linux", "Linux")];

const OS_HINT: &str =
    "Linux and macOS personas pass iphey.com at 100 %; the Windows persona is currently flagged on Linux hosts.";

const URL_HINT: &str = "Paste socks5://user:pass@host:port, host:port or host:port:user:pass";

pub struct EditDialog {
    /// Empty = a profile that does not exist yet.
    pub id: String,
    pub saving: bool,
    pub loading: bool,
    pub error: Option<String>,
    serial: u64,
    name: String,
    group: String,
    remark: String,
    url: String,
    url_err: Option<String>,
    kind: ProxyKind,
    host: String,
    port: String,
    user: String,
    pass: String,
    /// The profile already has a password; `pass` stays empty unless the user types a new one.
    pass_stored: bool,
    os: String,
    country: String,
    tabs: String,
    seed: String,
    original: Option<Profile>,
    creating_group: bool,
    new_group: String,
    check_token: u64,
    checking: bool,
    check: Option<Result<ProxyCheck, String>>,
    /// Save was pressed once: show the validation messages.
    tried: bool,
    /// The user typed something: a late answer from the backend must not overwrite it.
    touched: bool,
    focus_name: bool,
}

enum Do {
    Nothing,
    Cancel,
    Save,
    Check,
    CreateGroup(String),
}

impl EditDialog {
    fn blank() -> EditDialog {
        EditDialog {
            id: String::new(),
            saving: false,
            loading: false,
            error: None,
            serial: 0,
            name: String::new(),
            group: "0".to_owned(),
            remark: String::new(),
            url: String::new(),
            url_err: None,
            kind: ProxyKind::Direct,
            host: String::new(),
            port: String::new(),
            user: String::new(),
            pass: String::new(),
            pass_stored: false,
            os: String::new(),
            country: String::new(),
            tabs: String::new(),
            seed: String::new(),
            original: None,
            creating_group: false,
            new_group: String::new(),
            check_token: 0,
            checking: false,
            check: None,
            tried: false,
            touched: false,
            focus_name: false,
        }
    }

    /// Load a (possibly unredacted) profile into the form; only the redacted copy is kept.
    pub fn fill(&mut self, p: &Profile) {
        if self.touched || self.saving {
            self.loading = false;
            return;
        }
        let p = redact(p);
        self.id = p.profile_id.clone();
        self.serial = p.serial_number;
        self.name = p.name.clone();
        self.group = if p.group_id.is_empty() { "0".to_owned() } else { p.group_id.clone() };
        self.remark = p.remark.clone();
        match p.user_proxy_config.hop() {
            None => {
                self.kind = ProxyKind::Direct;
                (self.host, self.port, self.user) = Default::default();
                self.pass_stored = false;
            }
            Some(h) => {
                self.kind = ProxyKind::from_scheme(&h.scheme).unwrap_or(ProxyKind::Http);
                self.host = h.host;
                self.port = h.port;
                self.user = h.account;
                self.pass_stored = !h.password.is_empty();
            }
        }
        self.pass.clear();
        self.os = p.persona_os.clone();
        self.country = p.country.trim().to_ascii_lowercase();
        self.tabs = p.tabs.join("\n");
        self.seed = p.profile_id.clone();
        self.original = Some(p);
        self.loading = false;
        self.sync_to_url();
    }

    pub fn group_created(&mut self, g: &Group) {
        self.group = g.group_id.clone();
        self.creating_group = false;
        self.new_group.clear();
        self.error = None;
    }

    pub fn proxy_checked(&mut self, token: u64, r: Result<ProxyCheck, String>) {
        if token == self.check_token {
            self.checking = false;
            self.check = Some(r);
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // proxy fields <-> URL

    fn sync_from_url(&mut self) {
        let text = self.url.trim().to_owned();
        if text.is_empty() {
            self.url_err = None;
            return;
        }
        match proxy::parse(&text) {
            Ok(pp) => {
                self.url_err = None;
                self.kind = match (pp.kind, self.kind) {
                    (Some(k), _) => k,
                    (None, ProxyKind::Direct) => ProxyKind::Http,
                    (None, k) => k,
                };
                self.host = pp.host;
                self.port = pp.port.to_string();
                self.user = pp.user;
                if !(pp.password == MASK || pp.password == "***") {
                    self.pass = pp.password;
                    self.pass_stored = false;
                }
                self.check = None;
            }
            Err(e) => self.url_err = Some(e),
        }
    }

    /// Rebuild the URL box from the fields. The password is always shown as `***`.
    fn sync_to_url(&mut self) {
        self.url_err = None;
        self.url = self.masked_url();
    }

    fn masked_url(&self) -> String {
        if self.kind == ProxyKind::Direct || self.host.trim().is_empty() {
            return String::new();
        }
        let enc = |s: &str| s.replace('%', "%25").replace('@', "%40").replace(':', "%3A").replace('/', "%2F");
        let has_pass = self.pass_stored || !self.pass.is_empty();
        let creds = match (self.user.trim(), has_pass) {
            ("", false) => String::new(),
            (u, false) => format!("{}@", enc(u)),
            (u, true) => format!("{}:***@", enc(u)),
        };
        let host = self.host.trim();
        let host = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
        let port = match self.port.trim() {
            "" => String::new(),
            p => format!(":{p}"),
        };
        format!("{}://{creds}{host}{port}", self.kind.scheme())
    }

    fn proxy_config(&self) -> ProxyConfig {
        let mut c = self.original.as_ref().map(|p| p.user_proxy_config.clone()).unwrap_or_default();
        if self.kind == ProxyKind::Direct {
            c.proxy_soft = Some("no_proxy".to_owned());
            (c.proxy_type, c.proxy_host, c.proxy_port) = (None, None, None);
            (c.proxy_user, c.proxy_password, c.proxy_url) = (None, None, None);
            return c;
        }
        c.proxy_soft = Some("other".to_owned());
        c.proxy_type = Some(self.kind.scheme().to_owned());
        c.proxy_host = Some(self.host.trim().to_owned());
        c.proxy_port = Some(self.port.trim().to_owned());
        c.proxy_user = Some(self.user.trim().to_owned()).filter(|u| !u.is_empty());
        c.proxy_password = if !self.pass.is_empty() {
            Some(self.pass.clone())
        } else if self.pass_stored {
            Some(MASK.to_owned())
        } else {
            None
        };
        c.proxy_url = None;
        c
    }

    // ----------------------------------------------------------------------------------------------------------
    // validation

    fn name_problem(&self) -> Option<String> {
        let n = self.name.trim();
        if n.is_empty() {
            Some("Give the profile a name.".to_owned())
        } else if n.chars().count() > 100 {
            Some("Keep the name under 100 characters.".to_owned())
        } else {
            None
        }
    }

    fn proxy_problem(&self) -> Option<String> {
        if self.kind == ProxyKind::Direct {
            return None;
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Some("Enter the proxy host, or choose \u{201c}No proxy (direct)\u{201d}.".to_owned());
        }
        if host.contains(|c: char| c.is_whitespace() || matches!(c, '/' | '@')) {
            return Some("The host must be a name or an IP address, without spaces or slashes.".to_owned());
        }
        match self.port.trim().parse::<u16>() {
            Ok(p) if p != 0 => None,
            _ => Some("The port must be a number between 1 and 65535.".to_owned()),
        }
    }

    fn tabs_problem(&self) -> Option<String> {
        let lines = self.tabs.lines().map(str::trim).filter(|l| !l.is_empty());
        lines
            .enumerate()
            .find(|(_, l)| l.contains(char::is_whitespace))
            .map(|(i, _)| format!("Start URL {} contains a space; put one address on each line.", i + 1))
    }

    fn has_problems(&self) -> bool {
        self.name_problem().is_some() || self.proxy_problem().is_some() || self.tabs_problem().is_some()
    }

    fn build(&self) -> Option<Profile> {
        if self.has_problems() {
            return None;
        }
        let mut p = self.original.clone().unwrap_or_default();
        p.name = self.name.trim().to_owned();
        p.group_id = self.group.clone();
        p.remark = self.remark.trim_end().to_owned();
        p.user_proxy_config = self.proxy_config();
        p.persona_os = self.os.clone();
        p.country = self.country.clone();
        p.tabs = self
            .tabs
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| if l.contains("://") || l.starts_with("about:") { l.to_owned() } else { format!("https://{l}") })
            .collect();
        Some(p)
    }

    // ----------------------------------------------------------------------------------------------------------
    // drawing

    fn show(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], max_h: f32) -> Do {
        let mut act = Do::Nothing;
        let mut changed = false;
        let existing = !self.id.is_empty();
        ui.horizontal(|ui| {
            ui.label(RichText::new(if existing { "Edit profile" } else { "New profile" }).size(18.0).strong());
            if existing {
                ui.label(dim(p, format!("#{}", self.serial)));
            }
            if self.loading {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        ui.add_space(2.0);
        ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            changed |= self.general(ui, p, groups, &mut act);
            changed |= self.proxy_block(ui, p, &mut act);
            changed |= self.identity(ui, p);
        });
        if changed {
            self.touched = true;
        }
        ui.add_space(6.0);
        widgets::footer(ui, |ui| {
            let label = if existing { "Save changes" } else { "Create profile" };
            if widgets::primary_button(ui, p, label, !self.saving && !self.loading).clicked() {
                act = Do::Save;
            }
            if widgets::button(ui, "Cancel", !self.saving).clicked() {
                act = Do::Cancel;
            }
            if self.saving {
                ui.add(egui::Spinner::new().size(16.0));
            }
            let message = match &self.error {
                Some(e) => Some(e.clone()),
                None if self.tried && self.has_problems() => Some("Fix the highlighted fields to continue.".to_owned()),
                None => None,
            };
            if let Some(m) = message {
                ui.add(egui::Label::new(RichText::new(m).color(p.err).size(13.0)).wrap());
            }
        });
        if ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command) {
            act = Do::Save;
        }
        act
    }

    fn general(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], act: &mut Do) -> bool {
        let mut ch = false;
        section(ui, p, "General");
        Grid::new("edit-general").num_columns(2).spacing([14.0, 9.0]).min_col_width(100.0).show(ui, |ui| {
            ui.label(dim(p, "Name"));
            ui.vertical(|ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut self.name)
                        .hint_text("e.g. Vinted FR - Camille")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_name) {
                    r.request_focus();
                }
                ch |= r.changed();
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    *act = Do::Save;
                }
                if self.tried
                    && let Some(m) = self.name_problem()
                {
                    ui.label(RichText::new(m).color(p.err).size(12.0));
                }
            });
            ui.end_row();

            ui.label(dim(p, "Group"));
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt("edit-group")
                        .width(240.0)
                        .selected_text(group_label(groups, &self.group))
                        .show_ui(ui, |ui| {
                            ch |= ui.selectable_value(&mut self.group, "0".to_owned(), "Ungrouped").changed();
                            for g in groups {
                                ch |= ui.selectable_value(&mut self.group, g.group_id.clone(), &g.group_name).changed();
                            }
                        });
                    if ui.button(if self.creating_group { "Cancel" } else { "+ New group" }).clicked() {
                        self.creating_group = !self.creating_group;
                    }
                });
                if self.creating_group {
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            TextEdit::singleline(&mut self.new_group).hint_text("New group name").desired_width(240.0),
                        );
                        let name = self.new_group.trim().to_owned();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        let clicked = ui.add_enabled(!name.is_empty(), egui::Button::new("Create")).clicked();
                        if (clicked || enter) && !name.is_empty() {
                            *act = Do::CreateGroup(name);
                        }
                    });
                }
            });
            ui.end_row();

            ui.label(dim(p, "Remark"));
            let r = ui.add(
                TextEdit::singleline(&mut self.remark)
                    .hint_text("Optional note, shown in the list")
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            ui.end_row();
        });
        ch
    }

    fn proxy_block(&mut self, ui: &mut Ui, p: &Palette, act: &mut Do) -> bool {
        let (mut ch, mut fields) = (false, false);
        section(ui, p, "Proxy");
        Grid::new("edit-proxy").num_columns(2).spacing([14.0, 9.0]).min_col_width(100.0).show(ui, |ui| {
            ui.label(dim(p, "Proxy URL"));
            ui.vertical(|ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut self.url)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("socks5://user:pass@host:port")
                        .desired_width(f32::INFINITY),
                );
                if r.changed() {
                    self.sync_from_url();
                    ch = true;
                }
                if r.lost_focus() && self.url_err.is_none() {
                    self.sync_to_url();
                }
                match &self.url_err {
                    Some(e) if !self.url.trim().is_empty() => {
                        ui.add(egui::Label::new(RichText::new(e).color(p.err).size(12.0)).wrap());
                    }
                    _ => {
                        ui.label(small_dim(p, URL_HINT));
                    }
                }
            });
            ui.end_row();

            ui.label(dim(p, "Type"));
            ComboBox::from_id_salt("edit-proxy-kind").width(200.0).selected_text(self.kind.label()).show_ui(ui, |ui| {
                for k in ProxyKind::ALL {
                    fields |= ui.selectable_value(&mut self.kind, k, k.label()).changed();
                }
            });
            ui.end_row();

            let direct = self.kind == ProxyKind::Direct;
            ui.label(dim(p, "Host / port"));
            ui.add_enabled_ui(!direct, |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 72.0 - ui.spacing().item_spacing.x * 2.0 - 6.0).max(120.0);
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.host).hint_text("host name or IP").desired_width(w))
                        .changed();
                    ui.label(":");
                    fields |=
                        ui.add(TextEdit::singleline(&mut self.port).hint_text("port").desired_width(72.0)).changed();
                });
            });
            ui.end_row();

            ui.label(dim(p, "Login"));
            ui.add_enabled_ui(!direct, |ui| {
                ui.horizontal(|ui| {
                    let reserve = if self.pass_stored && self.pass.is_empty() { 58.0 } else { 0.0 };
                    let w = ((ui.available_width() - ui.spacing().item_spacing.x * 2.0 - reserve) / 2.0).max(90.0);
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.user).hint_text("user (optional)").desired_width(w))
                        .changed();
                    let hint = if self.pass_stored { "saved \u{2013} type to replace" } else { "password (optional)" };
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.pass).password(true).hint_text(hint).desired_width(w))
                        .changed();
                    if self.pass_stored && self.pass.is_empty() && ui.small_button("Forget").clicked() {
                        self.pass_stored = false;
                        fields = true;
                    }
                });
            });
            ui.end_row();

            ui.label("");
            ui.horizontal_wrapped(|ui| {
                let ready = !direct && self.proxy_problem().is_none() && !self.checking;
                if widgets::button(ui, "Check proxy", ready).clicked() {
                    *act = Do::Check;
                }
                if self.checking {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(dim(p, "Checking\u{2026}"));
                } else if let Some(res) = self.check.clone() {
                    ch |= self.check_result(ui, p, res);
                }
            });
            ui.end_row();
        });
        if self.tried
            && let Some(m) = self.proxy_problem()
        {
            ui.label(RichText::new(m).color(p.err).size(12.0));
        }
        if fields {
            self.sync_to_url();
            self.check = None;
            ch = true;
        }
        ch
    }

    /// The outcome line next to `Check proxy`; returns `true` when it changed the country.
    fn check_result(&mut self, ui: &mut Ui, p: &Palette, res: Result<ProxyCheck, String>) -> bool {
        match res {
            Ok(c) if c.ok => {
                widgets::dot(ui, p.ok, true);
                let place = countries::label(&c.country);
                let place = if place.is_empty() { String::new() } else { format!(" \u{b7} {place}") };
                let text = format!("Works \u{b7} exit IP {}{place} \u{b7} {} ms", c.ip, c.latency_ms);
                ui.label(RichText::new(text).color(p.ok));
                let code = c.country.trim().to_ascii_lowercase();
                if code.len() == 2
                    && code != self.country
                    && ui.small_button(format!("Use {} as country", code.to_uppercase())).clicked()
                {
                    self.country = code;
                    return true;
                }
            }
            Ok(c) => {
                widgets::dot(ui, p.err, true);
                ui.add(egui::Label::new(RichText::new(c.message).color(p.err)).wrap());
            }
            Err(e) => {
                widgets::dot(ui, p.err, true);
                ui.add(egui::Label::new(RichText::new(e).color(p.err)).wrap());
            }
        }
        false
    }

    fn identity(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        section(ui, p, "Identity");
        Grid::new("edit-identity").num_columns(2).spacing([14.0, 9.0]).min_col_width(100.0).show(ui, |ui| {
            ui.label(dim(p, "OS persona"));
            ui.vertical(|ui| {
                let shown = OS_CHOICES.iter().find(|c| c.0 == self.os).map_or("Auto (this computer's OS)", |c| c.1);
                ComboBox::from_id_salt("edit-os").width(240.0).selected_text(shown).show_ui(ui, |ui| {
                    for (v, label) in OS_CHOICES {
                        ch |= ui.selectable_value(&mut self.os, v.to_owned(), label).changed();
                    }
                });
                let col = if self.os == "windows" { p.warn } else { p.dim };
                ui.add(egui::Label::new(RichText::new(OS_HINT).size(12.0).color(col)).wrap());
            });
            ui.end_row();

            ui.label(dim(p, "Country"));
            ui.vertical(|ui| {
                let shown = match countries::label(&self.country) {
                    s if s.is_empty() => "Auto (from the proxy IP)".to_owned(),
                    s => s,
                };
                ComboBox::from_id_salt("edit-country").width(240.0).selected_text(shown).show_ui(ui, |ui| {
                    ch |= ui.selectable_value(&mut self.country, String::new(), "Auto (from the proxy IP)").changed();
                    for (code, name) in countries::COUNTRIES {
                        let text = format!("{name}  ({})", code.to_uppercase());
                        ch |= ui.selectable_value(&mut self.country, (*code).to_owned(), text).changed();
                    }
                });
                let hint = match countries::implied(&self.country) {
                    Some((tz, lang)) => format!("Browser language {lang} \u{b7} time zone {tz}"),
                    None => "Language and time zone follow the proxy's exit IP.".to_owned(),
                };
                ui.label(small_dim(p, hint));
            });
            ui.end_row();

            ui.label(dim(p, "Start URLs"));
            ui.vertical(|ui| {
                let r = ui.add(
                    TextEdit::multiline(&mut self.tabs)
                        .hint_text("https://example.com")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ch |= r.changed();
                match self.tabs_problem() {
                    Some(m) if self.tried => ui.label(RichText::new(m).color(p.err).size(12.0)),
                    _ => ui.label(small_dim(p, "One address per line, opened when the browser starts.")),
                };
            });
            ui.end_row();

            ui.label(dim(p, "Fingerprint seed"));
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let mut seed = if self.seed.is_empty() {
                        "assigned when the profile is created".to_owned()
                    } else {
                        self.seed.clone()
                    };
                    let field = TextEdit::singleline(&mut seed)
                        .font(egui::TextStyle::Monospace)
                        .interactive(false)
                        .desired_width(300.0);
                    ui.add(field);
                    if !self.seed.is_empty() && ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(self.seed.clone());
                    }
                });
                ui.label(small_dim(
                    p,
                    "Read-only: the profile id is the seed, so one profile always presents one identity.",
                ));
            });
            ui.end_row();
        });
        ch
    }
}

fn section(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text.to_uppercase()).size(11.5).strong().color(p.dim));
    ui.add_space(2.0);
}

impl CitroApp {
    /// Open the dialog for a new profile (`None`) or an existing one.
    pub fn open_edit(&mut self, id: Option<String>) {
        let mut d = EditDialog::blank();
        match id {
            Some(id) => {
                let Some(row) = self.row(&id) else {
                    self.toast(ToastKind::Warn, "That profile no longer exists");
                    return;
                };
                d.fill(&row.profile);
                self.tasks.spawn(false, move |w| {
                    let result = w.backend.profile(&id);
                    w.send(Msg::FullProfile { id, result });
                });
            }
            None => {
                d.group = self.profiles.query.group.clone().unwrap_or_else(|| "0".to_owned());
                d.focus_name = true;
            }
        }
        self.edit = Some(d);
    }

    pub fn edit_modal(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.edit.take() else { return };
        let p = self.pal;
        let groups = self.groups.clone();
        let max_h = (ctx.content_rect().height() - 220.0).max(260.0);
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut act = Do::Nothing;
        let r = widgets::modal(ctx, &p, "edit-modal", 660.0, |ui| act = d.show(ui, &p, &groups, max_h));
        if !d.saving && (esc || (r.should_close() && !d.touched)) {
            act = Do::Cancel;
        }
        match act {
            Do::Cancel => return,
            Do::Save => {
                d.tried = true;
                if let Some(profile) = d.build() {
                    d.saving = true;
                    d.error = None;
                    let created = d.id.is_empty();
                    self.tasks.spawn(true, move |w| {
                        let result = w.backend.save_profile(profile);
                        w.send(Msg::Saved { result, created });
                    });
                }
            }
            Do::Check => {
                d.check_token += 1;
                d.checking = true;
                d.check = None;
                let (token, id, mut cfg) = (d.check_token, d.id.clone(), d.proxy_config());
                self.tasks.spawn(true, move |w| {
                    if cfg.proxy_password.as_deref() == Some(MASK)
                        && let Ok(full) = w.backend.profile(&id)
                    {
                        cfg.proxy_password = full.user_proxy_config.proxy_password;
                    }
                    w.send(Msg::ProxyChecked { token, result: w.backend.check_proxy(&cfg) });
                });
            }
            Do::CreateGroup(name) => {
                let g = Group { group_id: String::new(), group_name: name, remark: String::new() };
                self.tasks.spawn(true, move |w| w.send(Msg::GroupCreated(w.backend.save_group(g))));
            }
            Do::Nothing => {}
        }
        self.edit = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog_with_proxy(url: &str) -> EditDialog {
        let mut d = EditDialog::blank();
        d.name = "t".into();
        d.url = url.into();
        d.sync_from_url();
        d
    }

    #[test]
    fn a_pasted_url_fills_the_fields_and_the_url_box_masks_the_password() {
        let mut d = dialog_with_proxy("socks5://alice:s3cret@203.0.113.9:1080");
        assert_eq!(
            (d.kind, d.host.as_str(), d.port.as_str(), d.user.as_str()),
            (ProxyKind::Socks5, "203.0.113.9", "1080", "alice")
        );
        assert_eq!(d.pass, "s3cret");
        d.sync_to_url();
        assert_eq!(d.url, "socks5://alice:***@203.0.113.9:1080");
        assert!(!d.url.contains("s3cret"));
    }

    #[test]
    fn editing_the_masked_url_keeps_the_password() {
        let mut d = dialog_with_proxy("socks5://alice:s3cret@203.0.113.9:1080");
        d.sync_to_url();
        d.url = "socks5://alice:***@203.0.113.10:1080".into();
        d.sync_from_url();
        assert_eq!((d.host.as_str(), d.pass.as_str()), ("203.0.113.10", "s3cret"));
    }

    #[test]
    fn a_stored_password_travels_as_the_mask() {
        let mut d = EditDialog::blank();
        d.kind = ProxyKind::Http;
        d.host = "proxy.example.net".into();
        d.port = "8080".into();
        d.pass_stored = true;
        assert_eq!(d.proxy_config().proxy_password.as_deref(), Some(MASK));
        d.pass = "new".into();
        assert_eq!(d.proxy_config().proxy_password.as_deref(), Some("new"));
    }

    #[test]
    fn validation_names_the_problem() {
        let mut d = EditDialog::blank();
        assert!(d.name_problem().is_some());
        d.name = "ok".into();
        d.kind = ProxyKind::Socks5;
        assert!(d.proxy_problem().unwrap().contains("host"));
        d.host = "h.example".into();
        d.port = "99999".into();
        assert!(d.proxy_problem().unwrap().contains("port"));
        d.port = "1080".into();
        assert!(d.build().is_some());
        d.tabs = "https://a.example\nhttps://b example".into();
        assert!(d.tabs_problem().unwrap().contains('2'));
        assert!(d.build().is_none());
    }

    #[test]
    fn direct_clears_the_proxy_and_start_urls_get_a_scheme() {
        let mut d = EditDialog::blank();
        d.name = "x".into();
        d.tabs = "example.com\nabout:blank\nhttp://a.b".into();
        let p = d.build().unwrap();
        assert!(p.user_proxy_config.is_direct());
        assert_eq!(p.tabs, ["https://example.com", "about:blank", "http://a.b"]);
    }
}
