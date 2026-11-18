//! The seam between the GUI and whatever owns the data.
//!
//! The UI only ever talks to a `dyn Backend`. Every method is **blocking** (it may spawn a process, hit the
//! network or the disk), so the UI calls it from a worker thread (see `tasks.rs`) and never from the frame loop.
//!
//! Implementations:
//! * [`mock::MockBackend`]  - in-memory fake data (`--demo`, unit tests, screenshots).
//! * [`local::LocalBackend`] - single-user implementation over `citro-core` (Store / Home / launch / Kernel).
//!
//! The team features of [`local::LocalBackend`] sit on `citro-sync`, the layer the command line and the Local API
//! use too, so every way of opening a team profile obeys the hub's lock.

pub use citro_core::extensions::Extension;
pub use citro_core::profile::Tag;
use citro_core::profile::{Group, Profile, ProxyConfig};
use std::fmt;

pub mod local;
mod local_hub;
pub mod mock;
pub mod proxycheck;

// ---------------------------------------------------------------------------------------------------------------
// errors

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// This backend does not implement the feature (yet). The UI renders a "not available in this build" state.
    Unsupported(String),
    NotFound(String),
    Invalid(String),
    Other(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Unsupported(s) => write!(f, "not available in this build yet: {s}"),
            BackendError::NotFound(s) => write!(f, "not found: {s}"),
            BackendError::Invalid(s) | BackendError::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<citro_core::Error> for BackendError {
    fn from(e: citro_core::Error) -> Self {
        match e {
            citro_core::Error::ProfileNotFound(id) => BackendError::NotFound(format!("profile {id}")),
            citro_core::Error::Invalid(s) => BackendError::Invalid(s),
            other => BackendError::Other(other.to_string()),
        }
    }
}

impl From<citro_sync::Error> for BackendError {
    fn from(e: citro_sync::Error) -> Self {
        match e {
            citro_sync::Error::Core(e) => e.into(),
            citro_sync::Error::Invalid(s) => BackendError::Invalid(s),
            citro_sync::Error::NotLoggedIn => BackendError::Invalid(
                "this device is not connected to a team hub: connect on the Teams page first".into(),
            ),
            citro_sync::Error::TeamProfile(name) => BackendError::Invalid(format!(
                "{name} belongs to the team hub: deleting it removes it for the whole team, not only here"
            )),
            other => BackendError::Other(other.to_string()),
        }
    }
}

impl From<citro_hub::client::Error> for BackendError {
    fn from(e: citro_hub::client::Error) -> Self {
        citro_sync::Error::from(e).into()
    }
}

impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        BackendError::Other(e.to_string())
    }
}

pub type BResult<T> = Result<T, BackendError>;

// ---------------------------------------------------------------------------------------------------------------
// profiles

/// What the table shows in the status column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Closed,
    /// Open on this device.
    Open {
        pid: u32,
        since: i64,
        /// The DevTools port (Puppeteer / Playwright / Selenium connect to 127.0.0.1:port); 0 when unknown.
        port: u16,
    },
    /// Open on another device (team lock). It cannot be opened here.
    LockedElsewhere {
        user: String,
        device: String,
        since: i64,
    },
    /// Transient: an open/close request is in flight (the UI tracks this itself, a backend may also report it).
    Opening,
    Closing,
}

impl RunState {
    pub fn is_open(&self) -> bool {
        matches!(self, RunState::Open { .. })
    }
    pub fn is_locked(&self) -> bool {
        matches!(self, RunState::LockedElsewhere { .. })
    }
    pub fn is_busy(&self) -> bool {
        matches!(self, RunState::Opening | RunState::Closing)
    }
}

/// One line of the profile table. `profile` has its secrets redacted (see [`redact`]); use
/// [`Backend::profile`] for the full record (edit dialog).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub profile: Profile,
    pub run: RunState,
    /// Owning team id when the profile is shared through a hub.
    pub team: Option<String>,
}

impl Row {
    pub fn id(&self) -> &str {
        &self.profile.profile_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    Open,
    Closed,
    Locked,
}

impl StatusFilter {
    pub const ALL: [StatusFilter; 4] =
        [StatusFilter::All, StatusFilter::Open, StatusFilter::Closed, StatusFilter::Locked];

    pub fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "All",
            StatusFilter::Open => "Open",
            StatusFilter::Closed => "Closed",
            StatusFilter::Locked => "Locked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SortKey {
    #[default]
    Serial,
    Name,
    Group,
    Remark,
    Proxy,
    Os,
    LastOpened,
    LastClosed,
    Status,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Group id; `"0"` = Ungrouped; `None` = every group.
    pub group: Option<String>,
    /// Free text: every whitespace separated token must match name / remark / proxy / id / serial / group.
    pub q: String,
    pub status: StatusFilter,
    pub sort: SortKey,
    pub desc: bool,
    /// Only the profiles shared with this team.
    pub team: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenOpts {
    /// No window (`--headless=new`).
    pub headless: bool,
    /// Keep Chromium's sandbox (needs unprivileged user namespaces; off by default as in other antidetect browsers).
    pub sandbox: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    pub pid: u32,
    pub since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Somebody else has it open: the UI shows "Open on <device> by <user> since <time>" instead of opening.
    LockedElsewhere {
        user: String,
        device: String,
        since: i64,
    },
    Other(String),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::LockedElsewhere { user, device, .. } => write!(f, "open on {device} by {user}"),
            OpenError::Other(s) => f.write_str(s),
        }
    }
}

impl From<BackendError> for OpenError {
    fn from(e: BackendError) -> Self {
        OpenError::Other(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCheck {
    pub ok: bool,
    pub ip: String,
    /// ISO 3166-1 alpha-2, empty when unknown.
    pub country: String,
    pub latency_ms: u32,
    pub message: String,
}

// ---------------------------------------------------------------------------------------------------------------
// logs

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum Level {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl Level {
    pub const ALL: [Level; 5] = [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error];

    pub fn label(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LogKind {
    /// Citroenition's own log.
    #[default]
    Citro,
    /// The browser kernel's log.
    Browser,
}

impl LogKind {
    pub fn label(self) -> &'static str {
        match self {
            LogKind::Citro => "Citroenition",
            LogKind::Browser => "Browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogQuery {
    pub kind: LogKind,
    pub profile_id: Option<String>,
    pub level_min: Level,
    pub text: String,
    /// Newest `limit` lines that match, returned oldest first.
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Unix seconds; `0` = unknown.
    pub ts: i64,
    pub level: Level,
    pub profile_id: Option<String>,
    pub message: String,
}

// ---------------------------------------------------------------------------------------------------------------
// kernel

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelInfo {
    pub version: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KernelStatus {
    /// The kernel a launch would use right now.
    pub installed: Option<KernelInfo>,
    /// Newest version the installer knows about (`None` = not checked / offline).
    pub latest: Option<String>,
    /// Every kernel installed under the data directory, newest first.
    pub all: Vec<KernelInfo>,
    /// Why `installed` was chosen: `settings override`, `$CITRO_KERNEL` or `newest installed`.
    pub source: String,
    /// Why there is no usable kernel (not found, not a file ...).
    pub error: Option<String>,
}

impl KernelStatus {
    pub fn update_available(&self) -> bool {
        match (&self.installed, &self.latest) {
            (Some(i), Some(l)) => i.version != *l,
            (None, Some(_)) => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// teams / hub

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    Owner,
    Admin,
    #[default]
    Member,
    Viewer,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Owner => "Owner",
            Role::Admin => "Admin",
            Role::Member => "Member",
            Role::Viewer => "Viewer",
        }
    }

    /// Owners and admins may force-unlock a profile someone else holds.
    pub fn can_force_unlock(self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The hub's user id (empty in demo data).
    pub id: String,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamInfo {
    pub id: String,
    pub name: String,
    /// *My* role in this team.
    pub role: Role,
    pub members: Vec<Member>,
    pub profile_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HubStatus {
    pub connected: bool,
    pub url: String,
    pub user: String,
    /// The team this device works with.
    pub team: String,
    pub device_name: String,
    pub error: Option<String>,
}

/// Something that happened without the user asking: a team browser was closed by its window and its data went up,
/// a lock was lost while the browser ran, a browser that outlived its session was stopped. Shown once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub text: String,
}

// ---------------------------------------------------------------------------------------------------------------
// backend description

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Demo,
    Local,
    Hub,
}

/// Which optional features the backend implements; the UI shows an explanatory state for the missing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub teams: bool,
    pub kernel_install: bool,
    pub proxy_check: bool,
    pub group_edit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub kind: BackendKind,
    pub data_dir: String,
    pub caps: Caps,
}

// ---------------------------------------------------------------------------------------------------------------

pub trait Backend: Send + Sync {
    fn info(&self) -> BackendInfo;

    // profiles ---------------------------------------------------------------------------------------------
    fn list_profiles(&self, q: &Query) -> BResult<Vec<Row>>;
    /// The full record (secrets included) for the edit dialog.
    fn profile(&self, id: &str) -> BResult<Profile>;
    /// Creates the profile when `profile_id` is empty. Returns the stored record.
    fn save_profile(&self, p: Profile) -> BResult<Profile>;
    fn delete_profiles(&self, ids: &[String]) -> BResult<()>;
    fn set_remark(&self, id: &str, text: &str) -> BResult<()>;
    /// Give the profile another number (No.); the profile that had it takes the old one.
    fn set_serial(&self, _id: &str, _n: u64) -> BResult<()> {
        Err(BackendError::Unsupported("changing the number".into()))
    }
    /// `group` is a group id; `"0"` = Ungrouped.
    fn move_to_group(&self, ids: &[String], group: &str) -> BResult<()>;
    fn duplicate_profile(&self, id: &str) -> BResult<Profile>;
    fn clear_cache(&self, id: &str) -> BResult<()>;
    fn check_proxy(&self, proxy: &ProxyConfig) -> BResult<ProxyCheck>;

    // running ----------------------------------------------------------------------------------------------
    fn open(&self, id: &str, opts: &OpenOpts) -> Result<RunInfo, OpenError>;
    fn close(&self, id: &str) -> BResult<()>;

    // groups -----------------------------------------------------------------------------------------------
    fn groups(&self) -> BResult<Vec<Group>>;
    /// Creates the group when `group_id` is empty, otherwise renames / updates the remark.
    fn save_group(&self, g: Group) -> BResult<Group>;
    /// The group's profiles move to Ungrouped.
    fn delete_group(&self, id: &str) -> BResult<()>;

    // tags -------------------------------------------------------------------------------------------------
    fn tags(&self) -> BResult<Vec<Tag>> {
        Ok(Vec::new())
    }
    /// Create (empty id) or rename / recolor.
    fn save_tag(&self, _t: Tag) -> BResult<Tag> {
        Err(BackendError::Unsupported("tags".into()))
    }
    fn delete_tag(&self, _id: &str) -> BResult<()> {
        Err(BackendError::Unsupported("tags".into()))
    }
    /// Add and remove tag ids on profiles.
    fn retag(&self, _ids: &[String], _add: &[String], _remove: &[String]) -> BResult<()> {
        Err(BackendError::Unsupported("tags".into()))
    }

    // extensions -------------------------------------------------------------------------------------------
    fn extensions(&self) -> BResult<Vec<Extension>> {
        Ok(Vec::new())
    }
    /// Download from the Chrome Web Store (`input` = a store link or the id) and install; a new extension is
    /// loaded into every profile until it is assigned to groups.
    fn add_extension(&self, _input: &str) -> BResult<Extension> {
        Err(BackendError::Unsupported("extensions".into()))
    }
    fn remove_extension(&self, _id: &str) -> BResult<()> {
        Err(BackendError::Unsupported("extensions".into()))
    }
    /// Which profiles load `id`: every one, or those of `groups`.
    fn assign_extension(&self, _id: &str, _all_profiles: bool, _groups: Vec<String>) -> BResult<()> {
        Err(BackendError::Unsupported("extensions".into()))
    }

    /// Add what an import read: groups by name, new profiles, skipping ones imported before.
    fn apply_import(&self, _parsed: &citro_core::import::Parsed) -> BResult<citro_core::import::Report> {
        Err(BackendError::Unsupported("importing profiles".into()))
    }

    /// What the profile's browser really shows (read from the browser itself; cached per persona). `p` may be an
    /// unsaved form.
    fn probe(&self, _p: &Profile) -> BResult<citro_core::probe::Probe> {
        Err(BackendError::Unsupported("reading the fingerprint".into()))
    }

    /// The cookies of an open profile's browser, as JSON (one cookie per line).
    fn live_cookies(&self, _id: &str) -> BResult<String> {
        Err(BackendError::Unsupported("reading cookies".into()))
    }

    /// The hub's version and update state; `None` without a hub.
    fn hub_upgrade(&self) -> BResult<Option<HubUpgrade>> {
        Ok(None)
    }

    /// Ask the hub's server to install the newest release.
    fn request_hub_upgrade(&self) -> BResult<()> {
        Err(BackendError::Unsupported("updating the hub".into()))
    }

    /// Invite someone to the team: a new account on the hub. Returns their token (shown this once).
    fn invite_member(&self, _team_id: &str, _name: &str, _role: Role) -> BResult<String> {
        Err(BackendError::Unsupported("inviting people".into()))
    }

    /// Change a member's role.
    fn set_member_role(&self, _team_id: &str, _name: &str, _role: Role) -> BResult<()> {
        Err(BackendError::Unsupported("changing roles".into()))
    }

    fn remove_member(&self, _team_id: &str, _user_id: &str) -> BResult<()> {
        Err(BackendError::Unsupported("removing people".into()))
    }

    /// A new token for a member who lost theirs; the old one stops working.
    fn new_member_token(&self, _team_id: &str, _user_id: &str) -> BResult<String> {
        Err(BackendError::Unsupported("new tokens".into()))
    }

    /// The team (members, this person's role), when this device is logged in to a hub.
    fn team_people(&self) -> BResult<Option<citro_sync::People>> {
        Ok(None)
    }

    /// Installed plugins.
    fn plugins(&self) -> BResult<Vec<citro_core::plugins::Plugin>> {
        Ok(Vec::new())
    }

    /// Download a plugin (a GitHub link: its newest commit; or a local folder / archive) and read it, without
    /// installing it yet.
    fn fetch_plugin(&self, _input: &str) -> BResult<PluginPreview> {
        Err(BackendError::Unsupported("plugins".into()))
    }

    fn install_plugin(&self, _p: &PluginPreview) -> BResult<citro_core::plugins::Plugin> {
        Err(BackendError::Unsupported("plugins".into()))
    }

    fn remove_plugin(&self, _id: &str) -> BResult<()> {
        Err(BackendError::Unsupported("plugins".into()))
    }

    fn set_plugin_scope(&self, _id: &str, _all_profiles: bool, _groups: Vec<String>) -> BResult<()> {
        Err(BackendError::Unsupported("plugins".into()))
    }

    /// For a GitHub plugin: the newest commit and the page with the changes, when it differs from the pinned one.
    fn newest_plugin_commit(&self, _id: &str) -> BResult<Option<(String, String)>> {
        Err(BackendError::Unsupported("plugins".into()))
    }

    /// Move all data to `to` (a new or empty folder) and use it from now on; refused while a browser runs.
    fn move_data(&self, _to: &str) -> BResult<()> {
        Err(BackendError::Unsupported("moving the data folder".into()))
    }

    // export / logs ----------------------------------------------------------------------------------------
    /// `ids` wins over `group`; both `None` = every profile. The text is UTF-8 (with a BOM when `bom`).
    fn export_csv(&self, ids: Option<Vec<String>>, group: Option<String>, bom: bool) -> BResult<Vec<u8>>;
    fn logs(&self, q: &LogQuery) -> BResult<Vec<LogLine>>;

    // kernel -----------------------------------------------------------------------------------------------
    fn kernel_status(&self) -> BResult<KernelStatus>;
    /// Download + verify + install the newest kernel. `progress(done_bytes, total_bytes)` may be called often from
    /// the worker thread; `total` is `0` while unknown.
    fn install_kernel(&self, progress: &dyn Fn(u64, u64)) -> BResult<KernelStatus>;

    // team -------------------------------------------------------------------------------------------------
    fn teams(&self) -> BResult<Vec<TeamInfo>>;
    fn hub_status(&self) -> HubStatus;
    /// `team` is only needed when the account is in several teams (empty otherwise).
    fn connect_hub(&self, url: &str, token: &str, device_name: &str, team: &str) -> BResult<HubStatus>;
    /// Admin only: releases a lock held by somebody else.
    fn force_unlock(&self, profile_id: &str) -> BResult<()>;
    /// Forget the hub login on this device. Team profiles stay, but cannot be opened until it is connected again.
    fn disconnect_hub(&self) -> BResult<HubStatus> {
        Err(BackendError::Unsupported("this backend has no team hub".into()))
    }
    /// Bring the team's profiles up to date (names, remarks, groups, proxies, who opened what last). Returns a
    /// sentence about what changed. Cookies and browser data come down when a profile is opened.
    fn pull_team(&self) -> BResult<String> {
        Err(BackendError::Unsupported("this backend has no team hub".into()))
    }
    /// Put local profiles on the team hub (they belong to the team from then on). Returns a sentence.
    fn share_profiles(&self, _ids: &[String]) -> BResult<String> {
        Err(BackendError::Unsupported("this backend has no team hub".into()))
    }
    /// Deletes profiles for everybody: a team profile goes from the hub too (`delete_profiles` refuses those).
    fn delete_profiles_everywhere(&self, ids: &[String]) -> BResult<()> {
        self.delete_profiles(ids)
    }
    /// What happened on its own since the last call (see [`Notice`]).
    fn take_notices(&self) -> Vec<Notice> {
        Vec::new()
    }
    /// The app is quitting: close what must not outlive it (the browsers of team profiles: nothing else would keep
    /// their lock alive). Blocks until their data is uploaded. Returns one line per profile that needed it.
    fn shutdown(&self) -> Vec<String> {
        Vec::new()
    }

    // misc -------------------------------------------------------------------------------------------------
    /// Adds a line to the Citroenition log (the UI reports what it did: exports, ...). Default: dropped.
    fn record(&self, _level: Level, _profile_id: Option<&str>, _message: &str) {}
    /// The kernel binary the user pinned in Settings (`None` = discover it). Default: ignored.
    fn set_kernel_override(&self, _path: Option<String>) {}
}

// ---------------------------------------------------------------------------------------------------------------
// helpers shared by the backends

/// The team hub's own version and its update button (for hub operators).
#[derive(Debug, Clone, Default)]
pub struct HubUpgrade {
    /// The hub's version.
    pub version: String,
    /// The newest Citroenition release (`None` when GitHub could not be asked).
    pub latest: Option<String>,
    /// This person runs the hub (may press Update).
    pub operator: bool,
    /// The server can update itself (installed with a new enough install-hub.sh).
    pub helper: bool,
    /// An update was asked for and has not started yet.
    pub requested: bool,
    /// The last update: `running`, `done` or `failed`, and what it said.
    pub state: String,
    pub message: String,
}

/// A downloaded plugin, read but not installed yet.
#[derive(Debug, Clone)]
pub struct PluginPreview {
    pub manifest: citro_core::plugins::Manifest,
    pub source: String,
    pub commit: String,
    /// The folder holding the manifest.
    pub dir: std::path::PathBuf,
    /// The installed plugin with the same id (this is then an update).
    pub existing: Option<citro_core::plugins::Plugin>,
}

pub const MASK: &str = "********";

/// A copy of `p` that is safe to show in lists, logs and screenshots: passwords, 2FA keys and cookies are masked.
pub fn redact(p: &Profile) -> Profile {
    let mask = |s: &str| if s.is_empty() { String::new() } else { MASK.to_owned() };
    let mut out = p.clone();
    out.password = mask(&p.password);
    out.fakey = mask(&p.fakey);
    out.cookie = mask(&p.cookie);
    out.user_proxy_config = p.user_proxy_config.redacted();
    out
}

/// `type host:port` split for display; `None` = direct connection.
pub fn proxy_parts(c: &ProxyConfig) -> Option<(String, String)> {
    let hop = c.hop()?;
    let host = if hop.host.contains(':') { format!("[{}]", hop.host) } else { hop.host };
    Some((hop.scheme, format!("{host}:{}", hop.port)))
}

/// `"socks5 203.0.113.7:1080"` or `"direct"`. Never contains credentials.
pub fn proxy_label(c: &ProxyConfig) -> String {
    match proxy_parts(c) {
        Some((scheme, addr)) => format!("{scheme} {addr}"),
        None => "direct".to_owned(),
    }
}

pub fn os_label(persona_os: &str) -> &'static str {
    match persona_os {
        "windows" => "Windows",
        "macos" | "mac" => "macOS",
        "linux" => "Linux",
        _ => "Auto",
    }
}

pub fn group_label(groups: &[Group], id: &str) -> String {
    if id.is_empty() || id == "0" {
        return "Ungrouped".to_owned();
    }
    groups.iter().find(|g| g.group_id == id).map(|g| g.group_name.clone()).unwrap_or_else(|| "Ungrouped".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_masks_every_secret() {
        let p = Profile {
            password: "hunter2".into(),
            fakey: "JBSWY3DPEHPK3PXP".into(),
            cookie: "sid=abc".into(),
            user_proxy_config: ProxyConfig {
                proxy_host: Some("203.0.113.5".into()),
                proxy_port: Some("1080".into()),
                proxy_password: Some("proxy-secret".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let r = redact(&p);
        let dump = format!("{r:?}");
        for secret in ["hunter2", "JBSWY3DPEHPK3PXP", "sid=abc", "proxy-secret"] {
            assert!(!dump.contains(secret), "{secret} leaked");
        }
        assert_eq!(r.password, MASK);
        assert_eq!(redact(&Profile::default()).password, "");
    }

    #[test]
    fn proxy_label_never_has_credentials() {
        let c = ProxyConfig::from_url("socks5://bob:s3cret@203.0.113.9:1081").unwrap();
        assert_eq!(proxy_label(&c), "socks5 203.0.113.9:1081");
        assert_eq!(proxy_label(&ProxyConfig::default()), "direct");
        let v6 = ProxyConfig::from_url("http://[2001:db8::1]:8080").unwrap();
        assert_eq!(proxy_label(&v6), "http [2001:db8::1]:8080");
    }

    #[test]
    fn kernel_update_detection() {
        let k = |v: &str| Some(KernelInfo { version: v.into(), path: String::new() });
        let st = |installed, latest: Option<&str>| KernelStatus {
            installed,
            latest: latest.map(str::to_owned),
            ..Default::default()
        };
        assert!(st(k("157.0.1"), Some("158.0.2")).update_available());
        assert!(!st(k("157.0.1"), Some("157.0.1")).update_available());
        assert!(st(None, Some("1")).update_available());
        assert!(!st(None, None).update_available());
    }

    #[test]
    fn group_labels() {
        let g = vec![Group {
            group_id: "3".into(),
            group_name: "Vinted FR".into(),
            remark: String::new(),
            tabs: Vec::new(),
            ..Default::default()
        }];
        assert_eq!(group_label(&g, "3"), "Vinted FR");
        assert_eq!(group_label(&g, "0"), "Ungrouped");
        assert_eq!(group_label(&g, "99"), "Ungrouped");
    }
}
