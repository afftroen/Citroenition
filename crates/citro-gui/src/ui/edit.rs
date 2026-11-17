//! The New / Edit profile page. Like AdsPower's editor it is one long form in five sections (General, Proxy,
//! Platform, Fingerprint, Advanced) with tabs that jump between them, and an Overview card that shows the identity
//! the browser will actually present (computed by `citro_core::fingerprint::Resolved`, the same code the launcher
//! uses, so the card never shows a value the kernel would not apply).
//!
//! Every fingerprint control writes an AdsPower `fingerprint_config` key the core already understands; keys the
//! dialog does not know are kept untouched.
//!
//! Secrets: the page only ever holds the *redacted* profile. A stored proxy password, platform password or cookie
//! is a flag (`*_stored`), not text; saving sends the mask back and the backend restores the real value. `Check
//! proxy` resolves the stored password on the worker thread.

mod view;

use super::app::{CitroApp, ToastKind};
use super::tasks::Msg;
use crate::backend::{MASK, ProxyCheck, redact};
use crate::proxy::{self, ProxyKind};
use citro_core::fingerprint::{FingerprintConfig, Ident, Resolved};
use citro_core::profile::{Group, Profile, ProxyConfig};
use eframe::egui::{self, Key};
use serde_json::{Map, Value, json};

const URL_HINT: &str = "Paste socks5://user:pass@host:port, host:port or host:port:user:pass";

/// The five sections, in page order.
pub const SECTIONS: [&str; 5] = ["General", "Proxy", "Platform", "Fingerprint", "Advanced"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Follows the proxy's exit country (or the seed, for hardware).
    Auto,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hw {
    /// The seed's persona value.
    Seed,
    /// The machine's real value (WebGL only).
    Real,
    Custom,
}

pub struct EditDialog {
    /// Empty = a profile that does not exist yet.
    pub id: String,
    pub saving: bool,
    pub loading: bool,
    pub error: Option<String>,
    serial: u64,
    // general
    name: String,
    group: String,
    remark: String,
    os: String,
    brand: String,
    ua: String,
    search_engine: String,
    cookie: String,
    cookie_stored: bool,
    // proxy
    url: String,
    url_err: Option<String>,
    kind: ProxyKind,
    host: String,
    port: String,
    user: String,
    pass: String,
    /// The profile already has a password; `pass` stays empty unless the user types a new one.
    pass_stored: bool,
    country: String,
    // platform
    domain: String,
    plat_user: String,
    plat_pass: String,
    plat_pass_stored: bool,
    tabs: String,
    // fingerprint
    webrtc: String,
    tz_mode: Follow,
    timezone: String,
    loc_mode: Follow,
    lat: String,
    lon: String,
    lang_mode: Follow,
    languages: String,
    page_lang_mode: Follow,
    page_lang: String,
    screen_mode: Hw,
    screen: String,
    canvas: bool,
    webgl_image: bool,
    gl_mode: Hw,
    gl_vendor: String,
    gl_renderer: String,
    cpu: String,
    ram: String,
    custom_seed: String,
    // advanced
    launch_args: String,

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
    /// A tab was clicked: scroll that section to the top on the next frame.
    jump: Option<usize>,
    /// The section at the top of the scrolled form (the highlighted tab).
    current: usize,
    /// Editing Settings' defaults for new profiles, not a profile.
    pub template: bool,
}

enum Do {
    Nothing,
    Cancel,
    Save,
    Check,
    CreateGroup(String),
}

fn fp_get(fp: &FingerprintConfig, key: &str) -> String {
    fp.str(key).unwrap_or_default().trim().to_owned()
}

/// Set `key`, or remove it when `value` is `None`.
fn put(m: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(v) => {
            m.insert(key.to_owned(), v);
        }
        None => {
            m.remove(key);
        }
    }
}

/// `"1440 x 900"`, `"1440x900"` or `"1440_900"` -> AdsPower's `"1440_900"`.
fn screen_key(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().replace(['x', 'X', ','], "_")
}

fn non_empty(s: &str) -> Option<Value> {
    let s = s.trim();
    (!s.is_empty()).then(|| Value::String(s.to_owned()))
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
            os: String::new(),
            brand: String::new(),
            ua: String::new(),
            search_engine: String::new(),
            cookie: String::new(),
            cookie_stored: false,
            url: String::new(),
            url_err: None,
            kind: ProxyKind::Direct,
            host: String::new(),
            port: String::new(),
            user: String::new(),
            pass: String::new(),
            pass_stored: false,
            country: String::new(),
            domain: String::new(),
            plat_user: String::new(),
            plat_pass: String::new(),
            plat_pass_stored: false,
            tabs: String::new(),
            webrtc: "disabled".to_owned(),
            tz_mode: Follow::Auto,
            timezone: String::new(),
            loc_mode: Follow::Auto,
            lat: String::new(),
            lon: String::new(),
            lang_mode: Follow::Auto,
            languages: String::new(),
            page_lang_mode: Follow::Auto,
            page_lang: String::new(),
            screen_mode: Hw::Seed,
            screen: String::new(),
            canvas: true,
            webgl_image: true,
            gl_mode: Hw::Seed,
            gl_vendor: String::new(),
            gl_renderer: String::new(),
            cpu: String::new(),
            ram: String::new(),
            custom_seed: String::new(),
            launch_args: String::new(),
            original: None,
            creating_group: false,
            new_group: String::new(),
            check_token: 0,
            checking: false,
            check: None,
            tried: false,
            touched: false,
            focus_name: false,
            jump: None,
            current: 0,
            template: false,
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
        self.cookie.clear();
        self.cookie_stored = !p.cookie.is_empty();
        self.domain = p.domain_name.clone();
        self.plat_user = p.username.clone();
        self.plat_pass.clear();
        self.plat_pass_stored = !p.password.is_empty();
        self.launch_args = p.launch_args.join("\n");
        self.search_engine = p.search_engine.trim().to_ascii_lowercase();
        if self.search_engine == "google" {
            self.search_engine.clear();
        }

        let fp = &p.fingerprint_config;
        self.brand = fp_get(fp, "brand").to_ascii_lowercase();
        self.ua = fp_get(fp, "ua");
        self.webrtc = match fp_get(fp, "webrtc").as_str() {
            "local" => "local",
            "proxy" | "forward" => "proxy",
            _ => "disabled",
        }
        .to_owned();
        (self.tz_mode, self.timezone) = if fp_get(fp, "automatic_timezone") == "0" {
            (Follow::Custom, fp_get(fp, "timezone"))
        } else {
            (Follow::Auto, String::new())
        };
        (self.lat, self.lon) = (fp_get(fp, "latitude"), fp_get(fp, "longitude"));
        self.loc_mode = if self.lat.is_empty() && self.lon.is_empty() { Follow::Auto } else { Follow::Custom };
        let langs = fp.list("language");
        self.lang_mode =
            if fp_get(fp, "language_switch") == "1" || langs.is_empty() { Follow::Auto } else { Follow::Custom };
        self.languages = langs.join(", ");
        self.page_lang = fp_get(fp, "page_language");
        self.page_lang_mode = if !fp.on("page_language_switch", true) && !self.page_lang.is_empty() {
            Follow::Custom
        } else {
            Follow::Auto
        };
        self.screen = fp_get(fp, "screen_resolution").replace(['x', 'X', ','], "_");
        self.screen_mode = if self.screen.is_empty() { Hw::Seed } else { Hw::Custom };
        self.canvas = fp.on("canvas", true);
        self.webgl_image = fp.on("webgl_image", true);
        let cfg = fp.obj("webgl_config");
        let gl = |k: &str| cfg.and_then(|c| c.get(k)).and_then(Value::as_str).unwrap_or("").to_owned();
        (self.gl_vendor, self.gl_renderer) = (gl("unmasked_vendor"), gl("unmasked_renderer"));
        self.gl_mode = match fp_get(fp, "webgl").as_str() {
            "0" => Hw::Real,
            "2" if !self.gl_vendor.is_empty() && !self.gl_renderer.is_empty() => Hw::Custom,
            _ => Hw::Seed,
        };
        self.cpu = fp_get(fp, "hardware_concurrency");
        self.ram = fp_get(fp, "device_memory");
        self.custom_seed = fp_get(fp, "seed");

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

    /// The `fingerprint_config` the form describes: the original keys, with every key the form owns rewritten.
    fn fingerprint(&self) -> FingerprintConfig {
        let mut fp = self.original.as_ref().map(|p| p.fingerprint_config.clone()).unwrap_or_default();
        let m = &mut fp.0;
        put(m, "brand", non_empty(&self.brand));
        put(m, "ua", non_empty(&self.ua));
        put(m, "webrtc", Some(json!(self.webrtc)));
        match self.tz_mode {
            Follow::Auto => {
                put(m, "automatic_timezone", Some(json!("1")));
                put(m, "timezone", None);
            }
            Follow::Custom => {
                put(m, "automatic_timezone", Some(json!("0")));
                put(m, "timezone", non_empty(&self.timezone));
            }
        }
        let (lat, lon) = match self.loc_mode {
            Follow::Auto => (None, None),
            Follow::Custom => (non_empty(&self.lat), non_empty(&self.lon)),
        };
        put(m, "latitude", lat);
        put(m, "longitude", lon);
        match self.lang_mode {
            Follow::Auto => {
                put(m, "language_switch", Some(json!("1")));
                put(m, "language", None);
            }
            Follow::Custom => {
                put(m, "language_switch", Some(json!("0")));
                put(m, "language", Some(json!(self.language_list())));
            }
        }
        match self.page_lang_mode {
            Follow::Auto => {
                put(m, "page_language_switch", Some(json!("1")));
                put(m, "page_language", None);
            }
            Follow::Custom => {
                put(m, "page_language_switch", Some(json!("0")));
                put(m, "page_language", non_empty(&self.page_lang));
            }
        }
        let screen = (self.screen_mode != Hw::Seed).then(|| screen_key(&self.screen));
        put(m, "screen_resolution", screen.as_deref().and_then(non_empty));
        put(m, "canvas", Some(json!(if self.canvas { "1" } else { "0" })));
        put(m, "webgl_image", Some(json!(if self.webgl_image { "1" } else { "0" })));
        match self.gl_mode {
            Hw::Seed => put(m, "webgl", None),
            Hw::Real => put(m, "webgl", Some(json!("0"))),
            Hw::Custom => {
                put(m, "webgl", Some(json!("2")));
                let mut cfg = m.get("webgl_config").and_then(Value::as_object).cloned().unwrap_or_default();
                cfg.insert("unmasked_vendor".into(), json!(self.gl_vendor.trim()));
                cfg.insert("unmasked_renderer".into(), json!(self.gl_renderer.trim()));
                m.insert("webgl_config".into(), Value::Object(cfg));
            }
        }
        put(m, "hardware_concurrency", non_empty(&self.cpu));
        put(m, "device_memory", non_empty(&self.ram));
        put(m, "seed", non_empty(&self.custom_seed));
        fp
    }

    fn language_list(&self) -> Vec<String> {
        self.languages.split([',', '\n', ';']).map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
    }

    /// What the browser will present with the form's current values.
    fn resolved(&self) -> Resolved {
        let fp = self.fingerprint();
        let ip = self.original.as_ref().map(|p| p.ip.clone()).unwrap_or_default();
        let id = if self.id.is_empty() { "new-profile" } else { self.id.as_str() };
        Resolved::resolve(&Ident { profile_id: id, country: &self.country, ip: &ip, persona_os: &self.os }, &fp)
    }

    /// AdsPower's "New fingerprint": a fresh seed, so every derived value changes at once.
    fn new_fingerprint(&mut self) {
        let mut rng = citro_core::rng::Rng::from_entropy();
        self.custom_seed = rng.token(12, b"abcdefghijklmnopqrstuvwxyz0123456789");
    }

    // ----------------------------------------------------------------------------------------------------------
    // validation

    fn name_problem(&self) -> Option<String> {
        if self.template {
            return None;
        }
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

    fn fingerprint_problem(&self) -> Option<String> {
        if self.tz_mode == Follow::Custom && !self.timezone.trim().contains('/') && self.timezone.trim() != "UTC" {
            return Some("Pick a time zone such as Europe/Paris.".to_owned());
        }
        if self.loc_mode == Follow::Custom {
            let ok = |s: &str, lim: f64| s.trim().parse::<f64>().is_ok_and(|v| (-lim..=lim).contains(&v));
            if !ok(&self.lat, 90.0) || !ok(&self.lon, 180.0) {
                return Some("Latitude must be between -90 and 90, longitude between -180 and 180.".to_owned());
            }
        }
        if self.lang_mode == Follow::Custom && self.language_list().is_empty() {
            return Some("Add at least one language, e.g. en-US.".to_owned());
        }
        if self.page_lang_mode == Follow::Custom && self.page_lang.trim().is_empty() {
            return Some("Choose the display language.".to_owned());
        }
        if self.screen_mode != Hw::Seed {
            let s = screen_key(&self.screen);
            let ok = s.split_once('_').is_some_and(|(w, h)| {
                w.parse::<u32>().is_ok_and(|w| w >= 200) && h.parse::<u32>().is_ok_and(|h| h >= 200)
            });
            if !ok {
                return Some("The screen size must look like 1920 x 1080.".to_owned());
            }
        }
        if self.gl_mode == Hw::Custom && (self.gl_vendor.trim().is_empty() || self.gl_renderer.trim().is_empty()) {
            return Some("A custom WebGL GPU needs both a vendor and a renderer.".to_owned());
        }
        None
    }

    fn advanced_problem(&self) -> Option<String> {
        self.launch_args
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .find(|l| !l.starts_with("--"))
            .map(|l| format!("\u{201c}{l}\u{201d} is not a browser switch: each line must start with --."))
    }

    fn has_problems(&self) -> bool {
        self.name_problem().is_some()
            || self.proxy_problem().is_some()
            || self.tabs_problem().is_some()
            || self.fingerprint_problem().is_some()
            || self.advanced_problem().is_some()
    }

    /// The first section with a problem (for jumping there when Save fails).
    fn first_problem_section(&self) -> Option<usize> {
        if self.name_problem().is_some() {
            Some(0)
        } else if self.proxy_problem().is_some() {
            Some(1)
        } else if self.tabs_problem().is_some() {
            Some(2)
        } else if self.fingerprint_problem().is_some() {
            Some(3)
        } else if self.advanced_problem().is_some() {
            Some(4)
        } else {
            None
        }
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
        p.cookie = if !self.cookie.trim().is_empty() {
            self.cookie.trim().to_owned()
        } else if self.cookie_stored {
            MASK.to_owned()
        } else {
            String::new()
        };
        p.domain_name = self.domain.trim().to_owned();
        p.username = self.plat_user.trim().to_owned();
        p.password = if !self.plat_pass.is_empty() {
            self.plat_pass.clone()
        } else if self.plat_pass_stored {
            MASK.to_owned()
        } else {
            String::new()
        };
        p.launch_args = self.launch_args.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        p.fingerprint_config = self.fingerprint();
        p.search_engine = self.search_engine.clone();
        Some(p)
    }

    /// The settings a new profile starts with: `t` without anything that belongs to one profile.
    pub fn defaults_of(t: &Profile) -> Profile {
        let mut d = Profile {
            persona_os: t.persona_os.clone(),
            search_engine: t.search_engine.clone(),
            country: t.country.clone(),
            tabs: t.tabs.clone(),
            domain_name: t.domain_name.clone(),
            launch_args: t.launch_args.clone(),
            fingerprint_config: t.fingerprint_config.clone(),
            user_proxy_config: t.user_proxy_config.clone(),
            ..Default::default()
        };
        // a proxy login and the seed belong to one profile
        d.user_proxy_config.proxy_user = None;
        d.user_proxy_config.proxy_password = None;
        d.fingerprint_config.0.remove("seed");
        d
    }

    /// The editor for Settings' defaults (`None` = the built-in ones).
    pub fn for_defaults(t: Option<&Profile>) -> EditDialog {
        let mut d = EditDialog::blank();
        d.template = true;
        if let Some(t) = t {
            d.fill(&EditDialog::defaults_of(t));
        }
        d
    }
}

impl CitroApp {
    /// Open the editor for a new profile (`None`) or an existing one.
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
                if let Some(t) = self.settings.new_profile.clone() {
                    d.fill(&EditDialog::defaults_of(&t));
                }
                d.group = self.profiles.query.group.clone().unwrap_or_else(|| "0".to_owned());
                d.focus_name = true;
            }
        }
        self.edit = Some(d);
    }

    /// The editor, drawn inside the content card in place of the current page.
    pub fn edit_page(&mut self, ui: &mut egui::Ui) {
        let Some(mut d) = self.edit.take() else { return };
        let p = self.pal;
        let groups = self.groups.clone();
        let kernel = self.kernel.as_ref().and_then(|k| k.installed.as_ref()).map(|k| k.version.clone());
        let esc = ui.input(|i| i.key_pressed(Key::Escape));
        let mut act = d.show(ui, &p, &groups, kernel.as_deref());
        // Esc leaves only an untouched form: a long page of settings must not vanish on a stray key
        if !d.saving && esc && !d.touched && !ui.ctx().egui_wants_keyboard_input() {
            act = Do::Cancel;
        }
        match act {
            Do::Cancel => return,
            Do::Save if d.template => {
                d.tried = true;
                if let Some(profile) = d.build() {
                    self.settings.new_profile = Some(EditDialog::defaults_of(&profile));
                    self.save_settings();
                    return;
                }
                d.jump = d.first_problem_section();
            }
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
                } else {
                    d.jump = d.first_problem_section();
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
                let g = Group { group_id: String::new(), group_name: name, remark: String::new(), tabs: Vec::new() };
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
        assert_eq!(d.first_problem_section(), Some(2));
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

    #[test]
    fn fingerprint_controls_write_the_keys_the_launcher_reads() {
        let mut d = EditDialog::blank();
        d.name = "fp".into();
        d.os = "macos".into();
        d.brand = "edge".into();
        d.tz_mode = Follow::Custom;
        d.timezone = "Europe/Paris".into();
        d.lang_mode = Follow::Custom;
        d.languages = "fr-FR, en".into();
        d.screen_mode = Hw::Custom;
        d.screen = "1440 x 900".into();
        d.gl_mode = Hw::Custom;
        d.gl_vendor = "Google Inc. (Apple)".into();
        d.gl_renderer = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)".into();
        d.cpu = "8".into();
        d.ram = "8".into();
        d.canvas = false;
        d.webrtc = "proxy".into();
        let p = d.build().unwrap();
        let r = Resolved::resolve(
            &Ident { profile_id: "k1", country: "", ip: "198.51.100.7", persona_os: &p.persona_os },
            &p.fingerprint_config,
        );
        assert_eq!(r.brand, "edge");
        assert_eq!(r.timezone.as_deref(), Some("Europe/Paris"));
        assert!(r.accept_lang.starts_with("fr-FR"));
        assert_eq!(r.screen, Some((1440, 900)));
        assert_eq!(p.fingerprint_config.str("screen_resolution").as_deref(), Some("1440_900"));
        assert_eq!((r.hardware_concurrency, r.device_memory), (Some(8), Some(8)));
        assert!(matches!(r.gpu, citro_core::fingerprint::Gpu::Custom(..)));
        assert_eq!(r.webrtc, citro_core::fingerprint::WebRtc::Address("198.51.100.7".into()));
    }

    #[test]
    fn filling_and_saving_round_trips_the_fingerprint_and_keeps_unknown_keys() {
        let mut src = Profile { name: "r".into(), persona_os: "linux".into(), ..Default::default() };
        src.fingerprint_config.0 = serde_json::from_value(json!({
            "automatic_timezone": "0", "timezone": "Asia/Tokyo", "language_switch": "0", "language": ["ja-JP"],
            "screen_resolution": "1920_1080", "webgl": "0", "canvas": "0", "hardware_concurrency": "4",
            "seed": "abc", "flash": "block"
        }))
        .unwrap();
        src.cookie = "[{\"name\":\"a\"}]".into();
        src.password = "pw".into();
        let mut d = EditDialog::blank();
        d.fill(&src);
        assert_eq!((d.tz_mode, d.timezone.as_str()), (Follow::Custom, "Asia/Tokyo"));
        assert_eq!((d.screen_mode, d.gl_mode, d.canvas), (Hw::Custom, Hw::Real, false));
        assert!(d.cookie_stored && d.plat_pass_stored && d.cookie.is_empty());
        let out = d.build().unwrap();
        let fp = &out.fingerprint_config;
        for k in ["automatic_timezone", "timezone", "screen_resolution", "webgl", "canvas", "seed", "flash"] {
            assert_eq!(fp.0.get(k), src.fingerprint_config.0.get(k), "{k}");
        }
        assert_eq!(fp.list("language"), ["ja-JP"]);
        // secrets go back as the mask, which the backend swaps for the stored value
        assert_eq!((out.cookie.as_str(), out.password.as_str()), (MASK, MASK));
    }

    #[test]
    fn fingerprint_and_advanced_validation() {
        let mut d = EditDialog::blank();
        d.name = "v".into();
        d.loc_mode = Follow::Custom;
        d.lat = "95".into();
        d.lon = "2".into();
        assert!(d.fingerprint_problem().unwrap().contains("Latitude"));
        d.lat = "48.85".into();
        assert!(d.fingerprint_problem().is_none());
        d.launch_args = "--disable-features=Foo\nbad".into();
        assert!(d.advanced_problem().is_some());
        assert_eq!(d.first_problem_section(), Some(4));
        d.launch_args = "--disable-features=Foo".into();
        assert_eq!(d.build().unwrap().launch_args, ["--disable-features=Foo"]);
    }

    #[test]
    fn new_fingerprint_changes_the_seed() {
        let mut d = EditDialog::blank();
        d.new_fingerprint();
        assert_eq!(d.custom_seed.len(), 12);
        assert_eq!(d.resolved().seed, d.custom_seed);
    }
}
