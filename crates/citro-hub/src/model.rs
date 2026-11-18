//! Wire and state types shared by the hub server and the client.
//!
//! Every timestamp is unix **seconds** (`i64`). Profiles reuse [`citro_core::profile::Profile`] unchanged, so the
//! hub never invents a second profile schema.

use citro_core::profile::Profile;
use serde::{Deserialize, Serialize};

/// What list responses (and everybody who may not see secrets) get instead of a password / cookie / 2FA key.
pub const MASK: &str = "********";
/// Request header carrying the lease of the current lock holder.
pub const LEASE_HEADER: &str = "X-Citro-Lease";
/// Optional request header naming the calling device; it only attributes log lines (a lock is bound to the
/// `device_id` in the lock request, not to this header).
pub const DEVICE_HEADER: &str = "X-Citro-Device";
/// Response header of `GET /v1/profiles/{pid}/data`: the stored version.
pub const DATA_VERSION_HEADER: &str = "X-Citro-Data-Version";
/// Request/response header: lower-case hex SHA-256 of the data blob.
pub const DATA_SHA256_HEADER: &str = "X-Citro-Data-Sha256";
/// The group id of profiles that are in no group.
pub const UNGROUPED: &str = "0";

/// A server-wide role: admins (the hub operators) create teams and see everything, members only see their own
/// teams. Per-team privileges are a [`TeamRole`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    Member,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            UserRole::Admin => "admin",
            UserRole::Member => "member",
        }
    }
}

impl std::str::FromStr for UserRole {
    type Err = String;
    /// `owner` is accepted as another word for `admin`: the person who runs the hub.
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "admin" | "owner" => Ok(UserRole::Admin),
            "member" => Ok(UserRole::Member),
            other => Err(format!("unknown role `{other}` (expected owner, admin or member)")),
        }
    }
}

/// A person with a token. The token itself is never part of this struct (the server only stores its hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub role: UserRole,
    #[serde(default)]
    pub disabled: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

pub use citro_core::team::{GroupAccess, TeamRole};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub team_id: String,
    pub user_id: String,
    pub user_name: String,
    pub role: TeamRole,
}

/// A team together with the caller's role in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamEntry {
    pub team: Team,
    pub role: TeamRole,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user: User,
    pub teams: Vec<TeamEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HubGroup {
    pub id: String,
    pub team_id: String,
    pub name: String,
    #[serde(default)]
    pub remark: String,
    /// Who may see and open the group's profiles (enforced by the hub). Absent on an old hub.
    #[serde(default)]
    pub access: GroupAccess,
    /// What every member's app applies to the group's profiles. Absent on an old hub.
    #[serde(default)]
    pub settings: GroupSettings,
}

/// A group's settings shared with the team: start pages and bookmarks for its profiles, and the settings a new
/// profile in the group starts with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupSettings {
    pub tabs: Vec<String>,
    pub bookmarks: Vec<citro_core::bookmarks::Bookmark>,
    pub new_profile: Option<citro_core::profile::Profile>,
}

/// Metadata of the hosted data blob (cookies / session archive) of one profile. `version == 0` = nothing stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataMeta {
    pub version: u64,
    pub size: u64,
    pub sha256: String,
    pub updated_at: i64,
}

/// One kept version of a profile's data (`GET /v1/profiles/{pid}/data/versions`, newest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataVersion {
    pub version: u64,
    pub size: u64,
    /// When the version was stored (file time, unix seconds).
    pub modified_at: i64,
    /// Whether this is the version a plain `GET .../data` returns.
    pub current: bool,
    /// Only known (without reading the file) for the current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Who has a profile open (public view of a lock; the lease itself is never listed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub user: String,
    pub device_id: String,
    pub device_name: String,
    pub since: i64,
    pub expires_at: i64,
}

/// The holder as reported inside a `locked` (409) error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub user: String,
    pub device_name: String,
    pub since: i64,
    pub expires_at: i64,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {}", self.user, self.device_name)
    }
}

/// A profile as the hub stores and serves it: the core [`Profile`] plus team bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HubProfile {
    pub team_id: String,
    pub created_by: String,
    pub updated_at: i64,
    pub last_open_by: String,
    pub last_close_by: String,
    pub data: DataMeta,
    pub lock: Option<LockInfo>,
    pub profile: Profile,
}

/// What `lock` returns: the secret lease token the holder must present afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub lease: String,
    /// Unix seconds, rounded up: the lock is certainly still held before this second.
    pub expires_at: i64,
    /// How long one heartbeat keeps the lock (whole seconds, at least 1).
    pub ttl_secs: u64,
    /// The same in milliseconds (sub-second TTLs only exist in tests); `0` when talking to an older hub.
    #[serde(default)]
    pub ttl_ms: u64,
    /// The locked profile (filled in by the server; empty when talking to an older hub).
    #[serde(default)]
    pub profile_id: String,
    /// The server's blob size limit in bytes (`0` = unknown).
    #[serde(default)]
    pub max_data_bytes: u64,
}

/// `GET /v1/teams/{tid}/profiles`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfilePage {
    pub profiles: Vec<HubProfile>,
    /// How many profiles matched before `limit`/`offset` were applied.
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogKind {
    /// Citroenition itself (app events and everything the hub does). `citro` is accepted as a short spelling.
    #[serde(rename = "citroenition", alias = "citro")]
    Citro,
    /// The browser kernel.
    #[serde(rename = "browser")]
    Browser,
}

impl LogKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LogKind::Citro => "citroenition",
            LogKind::Browser => "browser",
        }
    }
}

impl std::str::FromStr for LogKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "citroenition" | "citro" => Ok(LogKind::Citro),
            "browser" => Ok(LogKind::Browser),
            other => Err(format!("unknown log kind `{other}` (expected citroenition or browser)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            other => Err(format!("unknown log level `{other}` (expected debug, info, warn or error)")),
        }
    }
}

/// One log line a client posts to `POST /v1/logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Event time, unix seconds (the client's clock).
    pub ts: i64,
    pub kind: LogKind,
    pub level: LogLevel,
    /// The profile the line is about; the server derives the team from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Explicit team for lines that belong to no profile (app-level events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub message: String,
}

impl LogEntry {
    /// An entry stamped with the current time.
    pub fn now(kind: LogKind, level: LogLevel, profile_id: Option<&str>, message: impl Into<String>) -> LogEntry {
        let ts =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        LogEntry { ts, kind, level, profile_id: profile_id.map(str::to_owned), team_id: None, message: message.into() }
    }
}

/// A stored log line (`GET /v1/teams/{tid}/logs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts: i64,
    pub kind: LogKind,
    pub level: LogLevel,
    pub team_id: String,
    /// The user the line is attributed to (`"hub"` for events without a user).
    pub user: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub message: String,
}

/// Filters of the profile list; `None` = no filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileFilter {
    /// A group id (`"0"` = ungrouped) or an exact group name.
    pub group: Option<String>,
    /// Case-insensitive substring over name, remark, id, serial number, domain, account, tabs, country, ip, proxy host.
    pub q: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Filters of the log query; `None` = no filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub kind: Option<LogKind>,
    pub profile_id: Option<String>,
    /// Only this level (the server also accepts a comma separated list).
    pub level: Option<LogLevel>,
    /// Only records with `ts >= since`.
    pub since: Option<i64>,
    pub limit: Option<usize>,
}

/// The machine the client runs on. `id` must be stable across restarts (it is what a lock is bound to).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roles_and_levels_serialize_lowercase() {
        assert_eq!(serde_json::to_value(TeamRole::Owner).unwrap(), json!("owner"));
        assert_eq!(serde_json::to_value(UserRole::Admin).unwrap(), json!("admin"));
        assert_eq!(serde_json::to_value(LogLevel::Warn).unwrap(), json!("warn"));
        assert_eq!(serde_json::to_value(LogKind::Browser).unwrap(), json!("browser"));
        assert!(TeamRole::Member < TeamRole::Admin && TeamRole::Admin < TeamRole::Owner);
        assert_eq!("WARNING".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert!("loud".parse::<LogLevel>().is_err());
    }

    #[test]
    fn hub_profile_nests_the_core_profile() {
        let mut hp = HubProfile { team_id: "t1".into(), ..Default::default() };
        hp.profile.name = "shop".into();
        let v = serde_json::to_value(&hp).unwrap();
        assert_eq!(v["profile"]["name"], "shop");
        assert_eq!(v["data"]["version"], 0);
        assert!(v["lock"].is_null());
        let back: HubProfile = serde_json::from_value(v).unwrap();
        assert_eq!(back, hp);
    }

    #[test]
    fn lease_tolerates_missing_optional_fields() {
        let l: Lease = serde_json::from_value(json!({"lease": "x", "expires_at": 5, "ttl_secs": 90})).unwrap();
        assert_eq!((l.profile_id.as_str(), l.max_data_bytes), ("", 0));
    }
}
