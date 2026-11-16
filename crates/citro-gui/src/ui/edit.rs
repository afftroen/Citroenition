//! The New / Edit profile dialog: name, group, remark, proxy (one URL field kept in sync with type / host / port /
//! user / password), OS persona, country, start URLs and the read-only fingerprint seed.
//!
//! Secrets: the dialog only ever holds the *redacted* profile. A stored proxy password is a flag (`pass_stored`),
//! not text; saving sends the mask back and the backend restores the real value. `Check proxy` resolves the
//! stored password on the worker thread.

mod view;

use super::app::{CitroApp, ToastKind};
use super::tasks::Msg;
use super::widgets;
use crate::backend::{MASK, ProxyCheck, redact};
use crate::proxy::{self, ProxyKind};
use citro_core::profile::{Group, Profile, ProxyConfig};
use eframe::egui::{self, Key};

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
