//! A blocking, typed client for the hub (`ureq`, TLS through rustls + ring).
//!
//! One [`Client`] stands for one signed-in user on one device. It is cheap to clone (the connection pool is
//! shared), which is how the [`LeaseKeeper`] gets its own handle for its heartbeat thread.
//!
//! The rule the hub enforces, and the client makes easy to follow: **a profile is only ever open on one device.**
//! Take the lock, keep the lease alive while the browser runs, move the profile data in and out only while you
//! hold the lease, and release the lock when the browser closes:
//!
//! ```no_run
//! use citro_hub::client::{Client, Device, Error, UploadOptions};
//!
//! # fn main() -> Result<(), Error> {
//! let hub = Client::new("https://hub.example.org", "citro_token_goes_here", Device::new("laptop-7f3a", "Anna's laptop"))?;
//!
//! // `Locked { holder, device, .. }` says who has the profile when somebody else does.
//! let keeper = hub.lock_and_keep("kabc123")?; // heartbeats every ttl/3 on its own thread
//! if let Some(data) = hub.download_data("kabc123", keeper.lease())? {
//!     // data.data is the archive the last session uploaded; data.sha256 was verified.
//!     let _ = data;
//! }
//! // ... run the browser; check `keeper.is_held()` now and then ...
//! hub.upload_data("kabc123", keeper.lease(), b"cookies and profile directory", &UploadOptions::default())?;
//! keeper.release(true)?; // stops the heartbeat, unlocks, stamps "last closed"
//! # Ok(())
//! # }
//! ```

pub use crate::model::Device;
use crate::model::{
    DATA_SHA256_HEADER, DATA_VERSION_HEADER, DEVICE_HEADER, DataMeta, DataVersion, HubGroup, HubProfile, LEASE_HEADER,
    Lease, LogEntry, LogFilter, LogRecord, Me, Member, ProfileFilter, ProfilePage, TeamEntry, TeamRole, User,
};
use crate::server::blobs::hash_file;
use crate::server::redact::scrub_log_text;
use crate::server::util::{hex, random_bytes};
use citro_core::profile::Profile;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use ureq::http::Response;

/// The hub accepts at most this many log entries per request; `push_logs` splits bigger batches.
pub const MAX_LOG_BATCH: usize = 500;
/// A JSON answer bigger than this is refused (a profile list of tens of thousands of profiles fits easily).
const MAX_JSON_ANSWER: u64 = 128 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, Error>;

/// What can go wrong, sorted by what the caller can do about it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The profile is open on another device (HTTP 409 `locked`): `holder` is the user, `device` the device name.
    #[error("the profile is open: {holder} on {device}")]
    Locked { holder: String, device: String, since: i64, expires_at: i64 },
    /// The lease is missing, wrong, expired, released or was broken by a team admin (HTTP 403 `lease_*`). Whoever
    /// gets this must stop working on the profile: it is not theirs any more.
    #[error("the lease is gone: {message}")]
    LeaseLost { message: String, holder: Option<String> },
    /// The token is missing, unknown or revoked (HTTP 401).
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The token is fine but the role in the team is not enough (HTTP 403).
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// No such team / profile / group / version, or it is not visible to this user (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),
    /// A condition did not hold, e.g. somebody pushed newer data first (HTTP 409/412 `stale`).
    #[error("conflict: {message}")]
    Conflict { message: String, current_version: Option<u64> },
    /// Too many requests (HTTP 429); `retry_after` is the hub's hint.
    #[error("rate limited{}", .retry_after.map(|d| format!(" (retry in {} s)", d.as_secs())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },
    /// The data is bigger than the hub accepts (HTTP 413).
    #[error("too large: {0}")]
    TooLarge(String),
    /// The hub does not accept the request (HTTP 400 and other 4xx).
    #[error("rejected: {0}")]
    Invalid(String),
    /// The hub failed (5xx) or answered something unexpected (a redirect, a malformed answer).
    #[error("hub error ({status}): {message}")]
    Server { status: u16, message: String },
    /// Downloaded or uploaded data does not match its SHA-256: nothing was kept.
    #[error("integrity check failed: expected sha256 {expected}, got {actual}")]
    Integrity { expected: String, actual: String },
    /// No answer at all: the hub is down, unreachable, or the connection broke.
    #[error("network error: {0}")]
    Network(String),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

impl Error {
    /// Worth trying again (the hub may be back in a moment). Everything else is a definite answer.
    pub fn is_transient(&self) -> bool {
        matches!(self, Error::Network(_) | Error::Server { .. } | Error::RateLimited { .. } | Error::Io(_))
    }

    /// The answer says the caller does not own the profile (any more): `Locked`, `LeaseLost`, `NotFound`,
    /// `Forbidden` and `Unauthorized`.
    pub fn is_lease_loss(&self) -> bool {
        matches!(
            self,
            Error::Locked { .. }
                | Error::LeaseLost { .. }
                | Error::NotFound(_)
                | Error::Forbidden(_)
                | Error::Unauthorized(_)
        )
    }
}

/// Turn a failure answer into an [`Error`]. Pure, so it is testable without a server.
fn map_error(status: u16, retry_after: Option<u64>, location: Option<&str>, body: &[u8]) -> Error {
    if (300..400).contains(&status) {
        let to = location.map(|l| format!(" to {l}")).unwrap_or_default();
        return Error::Server {
            status,
            message: format!("the hub redirected the request{to}; point the client at the final URL (https://?)"),
        };
    }
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let e = &v["error"];
    let code = e["code"].as_str().unwrap_or_default();
    let message = e["message"].as_str().map(str::to_owned).unwrap_or_else(|| format!("HTTP {status}"));
    let holder = &e["holder"];
    let holder_text = holder["user"].as_str().map(|u| {
        let dev = holder["device_name"].as_str().unwrap_or("?");
        format!("{u} on {dev}")
    });
    match (status, code) {
        (_, "locked") => Error::Locked {
            holder: holder["user"].as_str().unwrap_or("?").to_owned(),
            device: holder["device_name"].as_str().unwrap_or("?").to_owned(),
            since: holder["since"].as_i64().unwrap_or(0),
            expires_at: holder["expires_at"].as_i64().unwrap_or(0),
        },
        (_, "lease_required" | "lease_invalid") => Error::LeaseLost { message, holder: holder_text },
        (401, _) => Error::Unauthorized(message),
        (403, _) => Error::Forbidden(message),
        (404, _) => Error::NotFound(message),
        (409 | 412, _) => Error::Conflict { message, current_version: e["current_version"].as_u64() },
        (429, _) => Error::RateLimited { retry_after: retry_after.map(Duration::from_secs) },
        (413, _) => Error::TooLarge(message),
        (400..=499, _) => Error::Invalid(message),
        _ => Error::Server { status, message },
    }
}

fn net(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Io(e) => Error::Network(e.to_string()),
        other => Error::Network(other.to_string()),
    }
}

impl Device {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Device {
        Device { id: id.into(), name: name.into() }
    }

    /// A device whose id survives restarts: read from `path` (JSON `{"id","name"}`), or made up and written there
    /// on first use. `default_name` is what the hub shows as "which device" when a profile is locked.
    pub fn load_or_create(path: &Path, default_name: &str) -> io::Result<Device> {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(d) = serde_json::from_str::<Device>(&text)
            && !d.id.is_empty()
        {
            return Ok(d);
        }
        let mut raw = [0u8; 12];
        random_bytes(&mut raw);
        let d = Device { id: format!("dev-{}", hex(&raw)), name: default_name.to_owned() };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&d).map_err(io::Error::other)?)?;
        Ok(d)
    }
}

/// Conditions for [`Client::upload_data`]: all given ones must hold or the push is refused with
/// [`Error::Conflict`] (nothing is stored). Without any, the lease alone decides.
#[derive(Debug, Clone, Default)]
pub struct UploadOptions {
    /// The version the data is based on (`?base_version=`): refused unless it is still the stored one.
    pub base_version: Option<u64>,
    /// `If-Match`: refused unless the stored data has this SHA-256.
    pub if_match: Option<String>,
    /// `If-None-Match: *`: refused when something is stored already (the very first push).
    pub only_if_new: bool,
}

/// Verified profile data as the hub stored it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub version: u64,
    /// Lower-case hex SHA-256; the bytes in `data` matched it.
    pub sha256: String,
    pub data: Vec<u8>,
}

/// What a file download left on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub version: u64,
    pub sha256: String,
    pub size: u64,
}

/// The answer of [`Client::download_data_if_changed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pulled {
    /// The hub holds no data for this profile yet.
    Empty,
    /// The hub's data is the one the caller already has (same SHA-256); nothing was transferred.
    Unchanged {
        version: u64,
    },
    Data(Download),
}

/// The result of a bulk delete: what went, and what stayed with the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BulkDelete {
    pub deleted: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

enum Fetch {
    Empty,
    Unchanged { version: u64 },
    Stream { resp: Response<ureq::Body>, version: u64, sha256: String },
}

/// The hub as seen by one user on one device. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    base: String,
    token: Arc<str>,
    device: Device,
    agent: ureq::Agent,
}

impl fmt::Debug for Client {
    /// The token is deliberately not part of it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("device", &self.device).finish_non_exhaustive()
    }
}

/// A path segment that came from the hub (a profile or team id) is only ever used as one.
fn seg(s: &str) -> Result<&str> {
    if !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        Ok(s)
    } else {
        Err(Error::Invalid(format!("`{s}` is not a valid id")))
    }
}

fn qs(pairs: &[(&str, String)]) -> String {
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", serde_urlencoded::to_string(pairs).unwrap_or_default())
    }
}

fn header_text(resp: &Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_owned())
}

fn read_json<T: DeserializeOwned>(mut resp: Response<ureq::Body>) -> Result<T> {
    let bytes = resp.body_mut().with_config().limit(MAX_JSON_ANSWER).read_to_vec().map_err(net)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Server { status: resp.status().as_u16(), message: format!("unreadable answer: {e}") })
}

impl Client {
    /// `base_url` is the hub's address, `http://host:port` or `https://host` (a trailing `/` is fine).
    pub fn new(base_url: &str, token: &str, device: Device) -> Result<Client> {
        let base = base_url.trim().trim_end_matches('/').to_owned();
        if !(base.starts_with("http://") || base.starts_with("https://")) || base.len() < 9 {
            return Err(Error::Invalid("the hub address must start with http:// or https://".to_owned()));
        }
        let token = token.trim();
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid("the token is empty or has characters a token never has".to_owned()));
        }
        if device.id.trim().is_empty() {
            return Err(Error::Invalid("the device id must not be empty".to_owned()));
        }
        let config = ureq::Agent::config_builder()
            // Failure answers carry the JSON explanation we want to read.
            .http_status_as_error(false)
            // Never follow a redirect with a bearer token in the request: report it instead.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_per_read(Some(Duration::from_secs(120)))
            .timeout_per_write(Some(Duration::from_secs(120)))
            .user_agent(concat!("citro-hub-client/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(Client { base, token: Arc::from(token), device, agent: ureq::Agent::new_with_config(config) })
    }

    /// The same user (same token), acting as another device.
    pub fn with_device(&self, device: Device) -> Client {
        Client { device, ..self.clone() }
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    // ---- plumbing ------------------------------------------------------------------------------------------------

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn auth<B>(&self, rb: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        rb.header("Authorization", format!("Bearer {}", self.token)).header(DEVICE_HEADER, self.device.id.as_str())
    }

    /// 2xx and 304 pass; everything else becomes an [`Error`].
    fn check(&self, result: std::result::Result<Response<ureq::Body>, ureq::Error>) -> Result<Response<ureq::Body>> {
        let mut resp = result.map_err(net)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) || status == 304 {
            return Ok(resp);
        }
        let retry_after = header_text(&resp, "retry-after").and_then(|s| s.parse::<u64>().ok());
        let location = header_text(&resp, "location");
        let body = resp.body_mut().with_config().limit(1 << 20).read_to_vec().unwrap_or_default();
        Err(map_error(status, retry_after, location.as_deref(), &body))
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, lease: Option<&str>) -> Result<T> {
        let mut rb = self.auth(self.agent.get(self.url(path)));
        if let Some(l) = lease {
            rb = rb.header(LEASE_HEADER, l);
        }
        read_json(self.check(rb.call())?)
    }

    fn post_json<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize, lease: Option<&str>) -> Result<T> {
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Invalid(e.to_string()))?;
        let mut rb = self.auth(self.agent.post(self.url(path))).header("Content-Type", "application/json");
        if let Some(l) = lease {
            rb = rb.header(LEASE_HEADER, l);
        }
        read_json(self.check(rb.send(bytes))?)
    }

    fn patch_json<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize, lease: Option<&str>) -> Result<T> {
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Invalid(e.to_string()))?;
        let mut rb = self.auth(self.agent.patch(self.url(path))).header("Content-Type", "application/json");
        if let Some(l) = lease {
            rb = rb.header(LEASE_HEADER, l);
        }
        read_json(self.check(rb.send(bytes))?)
    }

    fn delete_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        read_json(self.check(self.auth(self.agent.delete(self.url(path))).call())?)
    }

    // ---- who am I ------------------------------------------------------------------------------------------------

    /// `GET /healthz` (no token needed): the hub's version.
    pub fn healthz(&self) -> Result<String> {
        let v: Value = read_json(self.check(self.agent.get(self.url("/healthz")).call())?)?;
        Ok(v["version"].as_str().unwrap_or_default().to_owned())
    }

    pub fn me(&self) -> Result<Me> {
        self.get_json("/v1/me", None)
    }

    // ---- teams, members, groups ----------------------------------------------------------------------------------

    pub fn teams(&self) -> Result<Vec<TeamEntry>> {
        self.get_json("/v1/teams", None)
    }

    /// Server admins only; the creator becomes the team's first owner.
    pub fn create_team(&self, name: &str) -> Result<TeamEntry> {
        self.post_json("/v1/teams", &json!({ "name": name }), None)
    }

    pub fn members(&self, team_id: &str) -> Result<Vec<Member>> {
        self.get_json(&format!("/v1/teams/{}/members", seg(team_id)?), None)
    }

    /// Add a user to the team, or change their role.
    pub fn add_member(&self, team_id: &str, user_name: &str, role: TeamRole) -> Result<Member> {
        self.post_json(
            &format!("/v1/teams/{}/members", seg(team_id)?),
            &json!({ "user_name": user_name, "role": role }),
            None,
        )
    }

    pub fn remove_member(&self, team_id: &str, user_id: &str) -> Result<()> {
        let _: Value = self.delete_json(&format!("/v1/teams/{}/members/{}", seg(team_id)?, seg(user_id)?))?;
        Ok(())
    }

    pub fn groups(&self, team_id: &str) -> Result<Vec<HubGroup>> {
        self.get_json(&format!("/v1/teams/{}/groups", seg(team_id)?), None)
    }

    pub fn create_group(&self, team_id: &str, name: &str, remark: &str) -> Result<HubGroup> {
        self.post_json(&format!("/v1/teams/{}/groups", seg(team_id)?), &json!({ "name": name, "remark": remark }), None)
    }

    pub fn update_group(&self, group_id: &str, name: Option<&str>, remark: Option<&str>) -> Result<HubGroup> {
        self.patch_json(&format!("/v1/groups/{}", seg(group_id)?), &json!({ "name": name, "remark": remark }), None)
    }

    /// Delete a group; its profiles become ungrouped. Returns how many moved.
    pub fn delete_group(&self, group_id: &str) -> Result<usize> {
        let v: Value = self.delete_json(&format!("/v1/groups/{}", seg(group_id)?))?;
        Ok(v["moved"].as_u64().unwrap_or(0) as usize)
    }

    // ---- profiles ------------------------------------------------------------------------------------------------

    /// The profiles of a team, secrets masked unless the caller is a team admin or holds the profile's lock.
    pub fn profiles(&self, team_id: &str, filter: &ProfileFilter) -> Result<ProfilePage> {
        let mut q = Vec::new();
        if let Some(g) = &filter.group {
            q.push(("group", g.clone()));
        }
        if let Some(t) = &filter.q {
            q.push(("q", t.clone()));
        }
        if let Some(n) = filter.limit {
            q.push(("limit", n.to_string()));
        }
        if let Some(n) = filter.offset {
            q.push(("offset", n.to_string()));
        }
        self.get_json(&format!("/v1/teams/{}/profiles{}", seg(team_id)?, qs(&q)), None)
    }

    pub fn create_profile(&self, team_id: &str, profile: &Profile) -> Result<HubProfile> {
        self.post_json(&format!("/v1/teams/{}/profiles", seg(team_id)?), profile, None)
    }

    /// One profile. Pass the lease to get the secrets (password, 2FA key, proxy password) as the holder.
    pub fn profile(&self, profile_id: &str, lease: Option<&str>) -> Result<HubProfile> {
        self.get_json(&format!("/v1/profiles/{}", seg(profile_id)?), lease)
    }

    /// Edit with a JSON merge patch (RFC 7386). Editing a profile that is open needs the holder's lease.
    pub fn patch_profile(&self, profile_id: &str, patch: &Value, lease: Option<&str>) -> Result<HubProfile> {
        self.patch_json(&format!("/v1/profiles/{}", seg(profile_id)?), patch, lease)
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<()> {
        let _: Value = self.delete_json(&format!("/v1/profiles/{}", seg(profile_id)?))?;
        Ok(())
    }

    pub fn bulk_delete(&self, team_id: &str, ids: &[String]) -> Result<BulkDelete> {
        let v: Value =
            self.post_json(&format!("/v1/teams/{}/profiles/bulk-delete", seg(team_id)?), &json!({ "ids": ids }), None)?;
        let strings = |k: &str| -> Vec<String> {
            v[k].as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        let skipped = v["skipped"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        (
                            x["id"].as_str().unwrap_or_default().to_owned(),
                            x["reason"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(BulkDelete { deleted: strings("deleted"), skipped })
    }

    // ---- locks ---------------------------------------------------------------------------------------------------

    /// Take the exclusive lock for this device with the hub's default lease length. A second device, even of the
    /// same user, gets [`Error::Locked`] naming who has it.
    pub fn lock(&self, profile_id: &str) -> Result<Lease> {
        self.lock_request(profile_id, None)
    }

    /// Like [`Client::lock`] with a lease length of your choice (the hub cuts it down to its cap; the returned
    /// [`Lease`] says what you got).
    pub fn lock_with_ttl(&self, profile_id: &str, ttl: Duration) -> Result<Lease> {
        self.lock_request(profile_id, Some(ttl))
    }

    fn lock_request(&self, profile_id: &str, ttl: Option<Duration>) -> Result<Lease> {
        let mut body = json!({ "device_id": self.device.id, "device_name": self.device.name });
        if let Some(ttl) = ttl {
            body["ttl_ms"] = json!(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX));
        }
        self.post_json(&format!("/v1/profiles/{}/lock", seg(profile_id)?), &body, None)
    }

    /// [`Client::lock`] and start a [`LeaseKeeper`] on the lease that came back.
    pub fn lock_and_keep(&self, profile_id: &str) -> Result<LeaseKeeper> {
        let lease = self.lock(profile_id)?;
        Ok(LeaseKeeper::start(self, profile_id, &lease))
    }

    /// Extend the lease by one TTL. Only the holder can; anyone else gets [`Error::Locked`] / [`Error::LeaseLost`].
    pub fn heartbeat(&self, profile_id: &str, lease: &str) -> Result<Lease> {
        self.post_json(&format!("/v1/profiles/{}/heartbeat", seg(profile_id)?), &json!({}), Some(lease))
    }

    /// Release the lock. `closed` stamps "last closed" (when the browser really was closed, not for an aborted open).
    pub fn unlock(&self, profile_id: &str, lease: &str, closed: bool) -> Result<()> {
        let _: Value = self.post_json(
            &format!("/v1/profiles/{}/unlock", seg(profile_id)?),
            &json!({ "closed": closed }),
            Some(lease),
        )?;
        Ok(())
    }

    /// Break somebody's lock (team owners and admins only). The old lease stops working at once. Returns whether
    /// there was a live lock to break.
    pub fn force_unlock(&self, profile_id: &str) -> Result<bool> {
        let v: Value = self.post_json(&format!("/v1/profiles/{}/force-unlock", seg(profile_id)?), &json!({}), None)?;
        Ok(v["was_locked"].as_bool().unwrap_or(false))
    }

    // ---- hosted profile data -------------------------------------------------------------------------------------

    fn upload_request(
        &self,
        profile_id: &str,
        lease: &str,
        sha256: &str,
        opts: &UploadOptions,
    ) -> Result<ureq::RequestBuilder<ureq::typestate::WithBody>> {
        let q = qs(&opts.base_version.map(|v| ("base_version", v.to_string())).into_iter().collect::<Vec<_>>());
        let mut rb = self
            .auth(self.agent.put(self.url(&format!("/v1/profiles/{}/data{q}", seg(profile_id)?))))
            .header(LEASE_HEADER, lease)
            .header(DATA_SHA256_HEADER, sha256)
            .header("Content-Type", "application/octet-stream")
            // The hub answers refusals (wrong lease, stale version) before it reads the body; with this header
            // the client learns that without sending the archive.
            .header("Expect", "100-continue");
        if let Some(tag) = &opts.if_match {
            rb = rb.header("If-Match", format!("\"{}\"", tag.trim_matches('"')));
        }
        if opts.only_if_new {
            rb = rb.header("If-None-Match", "*");
        }
        Ok(rb)
    }

    fn upload_answer(resp: Response<ureq::Body>, sha256: &str, size: u64) -> Result<DataMeta> {
        let meta: DataMeta = read_json(resp)?;
        if meta.sha256 != sha256 || meta.size != size {
            return Err(Error::Integrity { expected: sha256.to_owned(), actual: meta.sha256 });
        }
        Ok(meta)
    }

    /// Push the profile data (an opaque archive: the hub never looks inside). Needs the lease of the lock this
    /// device holds. The hub verifies the SHA-256 and stores a new version; the answer carries its number.
    pub fn upload_data(&self, profile_id: &str, lease: &str, data: &[u8], opts: &UploadOptions) -> Result<DataMeta> {
        let sha = hex(&Sha256::digest(data));
        let resp = self.check(self.upload_request(profile_id, lease, &sha, opts)?.send(data))?;
        Self::upload_answer(resp, &sha, data.len() as u64)
    }

    /// [`Client::upload_data`] for an archive on disk (read twice, never held in memory).
    pub fn upload_data_file(
        &self,
        profile_id: &str,
        lease: &str,
        path: &Path,
        opts: &UploadOptions,
    ) -> Result<DataMeta> {
        let (size, sha) = hash_file(path)?;
        let file = fs::File::open(path)?;
        let resp = self.check(self.upload_request(profile_id, lease, &sha, opts)?.send(&file))?;
        Self::upload_answer(resp, &sha, size)
    }

    fn fetch(&self, profile_id: &str, lease: &str, version: Option<u64>, have: Option<&str>) -> Result<Fetch> {
        let q = qs(&version.map(|v| ("version", v.to_string())).into_iter().collect::<Vec<_>>());
        let mut rb = self
            .auth(self.agent.get(self.url(&format!("/v1/profiles/{}/data{q}", seg(profile_id)?))))
            .header(LEASE_HEADER, lease);
        if let Some(h) = have {
            rb = rb.header("If-None-Match", format!("\"{}\"", h.trim_matches('"')));
        }
        let resp = self.check(rb.call())?;
        let hub_version = header_text(&resp, DATA_VERSION_HEADER).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        match resp.status().as_u16() {
            204 => Ok(Fetch::Empty),
            304 => Ok(Fetch::Unchanged { version: hub_version }),
            status => {
                let sha256 =
                    header_text(&resp, DATA_SHA256_HEADER).map(|s| s.to_ascii_lowercase()).ok_or_else(|| {
                        Error::Server { status, message: format!("the hub did not send {DATA_SHA256_HEADER}") }
                    })?;
                Ok(Fetch::Stream { resp, version: hub_version, sha256 })
            }
        }
    }

    /// Copy the body into `sink`, hashing on the way; fails with [`Error::Integrity`] on a mismatch.
    fn drain_verified(mut resp: Response<ureq::Body>, expected: &str, sink: &mut dyn Write) -> Result<u64> {
        let mut reader = resp.body_mut().as_reader();
        let (mut hasher, mut size, mut buf) = (Sha256::new(), 0u64, vec![0u8; 64 * 1024]);
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            sink.write_all(&buf[..n])?;
            size += n as u64;
        }
        let actual = hex(&hasher.finalize());
        if actual != expected {
            return Err(Error::Integrity { expected: expected.to_owned(), actual });
        }
        Ok(size)
    }

    fn download(&self, profile_id: &str, lease: &str, version: Option<u64>, have: Option<&str>) -> Result<Pulled> {
        match self.fetch(profile_id, lease, version, have)? {
            Fetch::Empty => Ok(Pulled::Empty),
            Fetch::Unchanged { version } => Ok(Pulled::Unchanged { version }),
            Fetch::Stream { resp, version, sha256 } => {
                let mut data = Vec::new();
                Self::drain_verified(resp, &sha256, &mut data)?;
                Ok(Pulled::Data(Download { version, sha256, data }))
            }
        }
    }

    /// Fetch the current data; `None` when the hub holds nothing yet. The bytes are checked against the hub's
    /// SHA-256 ([`Error::Integrity`] otherwise). Needs the lease: while another device holds the profile its
    /// cookies are not served to anybody else.
    pub fn download_data(&self, profile_id: &str, lease: &str) -> Result<Option<Download>> {
        Ok(match self.download(profile_id, lease, None, None)? {
            Pulled::Data(d) => Some(d),
            _ => None,
        })
    }

    /// Like [`Client::download_data`], but nothing is transferred when the hub's data has the SHA-256 the caller
    /// already has on disk ([`Pulled::Unchanged`]).
    pub fn download_data_if_changed(&self, profile_id: &str, lease: &str, have_sha256: &str) -> Result<Pulled> {
        self.download(profile_id, lease, None, Some(have_sha256))
    }

    /// An older version that is still kept (see [`Client::data_versions`]).
    pub fn download_data_version(&self, profile_id: &str, lease: &str, version: u64) -> Result<Download> {
        match self.download(profile_id, lease, Some(version), None)? {
            Pulled::Data(d) => Ok(d),
            _ => Err(Error::NotFound(format!("no data for version {version}"))),
        }
    }

    /// Stream the current data into `dest` (a temporary file next to it, renamed only once the SHA-256 matched,
    /// so a failed download never leaves a damaged `dest`). `None` when the hub holds nothing yet.
    pub fn download_data_to_file(&self, profile_id: &str, lease: &str, dest: &Path) -> Result<Option<Fetched>> {
        let Fetch::Stream { resp, version, sha256 } = (match self.fetch(profile_id, lease, None, None)? {
            Fetch::Empty => return Ok(None),
            Fetch::Unchanged { .. } => return Ok(None),
            s @ Fetch::Stream { .. } => s,
        }) else {
            return Ok(None);
        };
        let mut raw = [0u8; 6];
        random_bytes(&mut raw);
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "data".to_owned());
        let tmp: PathBuf = dest.with_file_name(format!(".{name}.part-{}", hex(&raw)));
        let result = (|| -> Result<u64> {
            let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            let size = Self::drain_verified(resp, &sha256, &mut f)?;
            f.sync_all()?;
            fs::rename(&tmp, dest)?;
            Ok(size)
        })();
        match result {
            Ok(size) => Ok(Some(Fetched { version, sha256, size })),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// The versions of the profile's data that are still kept, newest first.
    pub fn data_versions(&self, profile_id: &str) -> Result<Vec<DataVersion>> {
        self.get_json(&format!("/v1/profiles/{}/data/versions", seg(profile_id)?), None)
    }

    // ---- logs ----------------------------------------------------------------------------------------------------

    /// Upload log lines of the browser or of Citroenition. The hub scrubs secrets, caps sizes and files each line
    /// under its profile's team. Returns how many lines were accepted.
    pub fn push_logs(&self, entries: &[LogEntry]) -> Result<usize> {
        let mut accepted = 0;
        for chunk in entries.chunks(MAX_LOG_BATCH) {
            // The hub scrubs again; doing it here means a secret never even leaves this machine.
            let clean: Vec<LogEntry> =
                chunk.iter().map(|e| LogEntry { message: scrub_log_text(&e.message), ..e.clone() }).collect();
            let v: Value =
                self.post_json("/v1/logs", &json!({ "device_id": self.device.id, "entries": clean }), None)?;
            accepted += v["accepted"].as_u64().unwrap_or(0) as usize;
        }
        Ok(accepted)
    }

    fn log_pairs(filter: &LogFilter) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(k) = filter.kind {
            q.push(("kind", k.as_str().to_owned()));
        }
        if let Some(p) = &filter.profile_id {
            q.push(("profile", p.clone()));
        }
        if let Some(l) = filter.level {
            q.push(("level", l.as_str().to_owned()));
        }
        if let Some(s) = filter.since {
            q.push(("since", s.to_string()));
        }
        if let Some(n) = filter.limit {
            q.push(("limit", n.to_string()));
        }
        q
    }

    /// `GET /v1/logs`, newest first. `team` is an id or a name; leave it out when the user is in one team only.
    pub fn logs(&self, team: Option<&str>, filter: &LogFilter) -> Result<Vec<LogRecord>> {
        let mut q = Self::log_pairs(filter);
        if let Some(t) = team {
            q.insert(0, ("team", t.to_owned()));
        }
        self.get_json(&format!("/v1/logs{}", qs(&q)), None)
    }

    /// Server-level events (failed logins, start-ups); server admins only.
    pub fn admin_logs(&self, filter: &LogFilter) -> Result<Vec<LogRecord>> {
        self.get_json(&format!("/v1/admin/logs{}", qs(&Self::log_pairs(filter))), None)
    }

    /// Every user of the hub (server admins only; never includes tokens).
    pub fn admin_users(&self) -> Result<Vec<User>> {
        self.get_json("/v1/admin/users", None)
    }

    // ---- CSV -----------------------------------------------------------------------------------------------------

    /// The team's profiles as CSV, exactly the bytes the hub sent: UTF-8 with BOM, CRLF line ends, no secrets.
    /// `group` is a group id or name; `None` exports every group.
    pub fn export_csv(&self, team_id: &str, group: Option<&str>) -> Result<Vec<u8>> {
        let q = qs(&group.map(|g| ("group", g.to_owned())).into_iter().collect::<Vec<_>>());
        let mut resp = self.check(
            self.auth(self.agent.get(self.url(&format!("/v1/teams/{}/profiles.csv{q}", seg(team_id)?)))).call(),
        )?;
        resp.body_mut().with_config().limit(MAX_JSON_ANSWER).read_to_vec().map_err(net)
    }

    /// [`Client::export_csv`] into a file (written whole, then renamed into place).
    pub fn export_csv_to(&self, team_id: &str, group: Option<&str>, dest: &Path) -> Result<u64> {
        let bytes = self.export_csv(team_id, group)?;
        let name =
            dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "export.csv".to_owned());
        let tmp = dest.with_file_name(format!(".{name}.part"));
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, dest)?;
        Ok(bytes.len() as u64)
    }
}

// ---- the lease keeper --------------------------------------------------------------------------------------------

/// Why a [`LeaseKeeper`] gave up. Once reported, the profile is not this device's any more: stop working on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseLoss {
    /// Human readable, free of secrets.
    pub reason: String,
    /// `true` when the hub answered that the lease is gone (taken over, force-unlocked, expired there); `false`
    /// when no heartbeat reached the hub for a whole lease time (the lease ran out unseen).
    pub confirmed_by_hub: bool,
}

struct KeeperState {
    stop: bool,
    loss: Option<LeaseLoss>,
    /// The hub's expiry (unix seconds) from the last answer.
    expires_at: i64,
    beats: u64,
}

struct KeeperShared {
    state: Mutex<KeeperState>,
    changed: Condvar,
}

impl KeeperShared {
    fn lock(&self) -> MutexGuard<'_, KeeperState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

type OnLost = Box<dyn FnOnce(&LeaseLoss) + Send>;

/// Keeps a lock alive: a thread sends a heartbeat every `ttl / 3` and notices when the lease is lost (somebody
/// force-unlocked the profile, the hub forgot the lock, or the hub could not be reached for a whole lease time).
///
/// Dropping a keeper only stops the heartbeat; the lock then simply runs out after one TTL. Call
/// [`LeaseKeeper::release`] to give the profile back right away.
pub struct LeaseKeeper {
    client: Client,
    profile_id: String,
    lease: String,
    shared: Arc<KeeperShared>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for LeaseKeeper {
    /// The lease is a secret and stays out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseKeeper")
            .field("profile_id", &self.profile_id)
            .field("held", &self.is_held())
            .finish_non_exhaustive()
    }
}

fn lease_ttl(lease: &Lease) -> Duration {
    let ms = if lease.ttl_ms > 0 { lease.ttl_ms } else { lease.ttl_secs.saturating_mul(1000) };
    Duration::from_millis(ms.max(1))
}

impl LeaseKeeper {
    /// Start heartbeating `lease` (as returned by [`Client::lock`]).
    pub fn start(client: &Client, profile_id: &str, lease: &Lease) -> LeaseKeeper {
        LeaseKeeper::spawn(client, profile_id, lease, None)
    }

    /// Like [`LeaseKeeper::start`]; `on_lost` runs once, on the keeper's thread, when the lease is lost.
    pub fn start_with(
        client: &Client,
        profile_id: &str,
        lease: &Lease,
        on_lost: impl FnOnce(&LeaseLoss) + Send + 'static,
    ) -> LeaseKeeper {
        LeaseKeeper::spawn(client, profile_id, lease, Some(Box::new(on_lost)))
    }

    fn spawn(client: &Client, profile_id: &str, lease: &Lease, on_lost: Option<OnLost>) -> LeaseKeeper {
        let shared = Arc::new(KeeperShared {
            state: Mutex::new(KeeperState { stop: false, loss: None, expires_at: lease.expires_at, beats: 0 }),
            changed: Condvar::new(),
        });
        let ttl = lease_ttl(lease);
        let (c, pid, token, sh) = (client.clone(), profile_id.to_owned(), lease.lease.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name(format!("lease-{profile_id}"))
            .spawn(move || keep(&c, &pid, &token, ttl, &sh, on_lost))
            .ok();
        LeaseKeeper {
            client: client.clone(),
            profile_id: profile_id.to_owned(),
            lease: lease.lease.clone(),
            shared,
            thread,
        }
    }

    /// The lease token, for the requests that need it (data transfer, edits).
    pub fn lease(&self) -> &str {
        &self.lease
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// Still ours as far as anybody knows: not lost, not stopped.
    pub fn is_held(&self) -> bool {
        let g = self.shared.lock();
        g.loss.is_none() && !g.stop
    }

    /// The loss, once it happened.
    pub fn loss(&self) -> Option<LeaseLoss> {
        self.shared.lock().loss.clone()
    }

    /// How many heartbeats the hub confirmed so far.
    pub fn heartbeats(&self) -> u64 {
        self.shared.lock().beats
    }

    /// When the hub will free the lock if no further heartbeat arrives (unix seconds).
    pub fn expires_at(&self) -> i64 {
        self.shared.lock().expires_at
    }

    /// Block until the lease is lost (`Some`) or `timeout` passed (`None`).
    pub fn wait_lost(&self, timeout: Duration) -> Option<LeaseLoss> {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.lock();
        loop {
            if let Some(l) = &g.loss {
                return Some(l.clone());
            }
            let now = Instant::now();
            if now >= deadline || g.stop {
                return None;
            }
            g = self.shared.changed.wait_timeout(g, deadline - now).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    fn halt(&mut self) {
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Stop heartbeating without unlocking (the lock runs out after one TTL).
    pub fn stop(mut self) {
        self.halt();
    }

    /// Stop heartbeating and release the lock; `closed` stamps "last closed". Fails with what the hub says when the
    /// lease was lost in the meantime.
    pub fn release(mut self, closed: bool) -> Result<()> {
        self.halt();
        self.client.unlock(&self.profile_id, &self.lease, closed)
    }
}

impl Drop for LeaseKeeper {
    fn drop(&mut self) {
        // Not joined: the thread may be inside a request, and a drop must not stall the caller.
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
    }
}

/// The heartbeat loop. A *definite* refusal ends it at once; transient trouble (network, 5xx, 429) is retried
/// sooner than the normal beat, until a whole lease time went by without a confirmed beat.
fn keep(client: &Client, pid: &str, lease: &str, ttl: Duration, shared: &KeeperShared, on_lost: Option<OnLost>) {
    let beat = (ttl / 3).max(Duration::from_millis(10));
    let retry = (ttl / 8).clamp(Duration::from_millis(10), Duration::from_secs(2));
    // The lock was granted a moment ago; the hub counts from when it got our request, which is no earlier than this.
    let mut valid_until = Instant::now() + ttl;
    let mut wait = beat;
    loop {
        {
            let mut g = shared.lock();
            let wake = Instant::now() + wait;
            loop {
                if g.stop {
                    return;
                }
                let now = Instant::now();
                if now >= wake {
                    break;
                }
                g = shared.changed.wait_timeout(g, wake - now).unwrap_or_else(|p| p.into_inner()).0;
            }
        }
        let sent = Instant::now();
        let outcome = client.heartbeat(pid, lease);
        if shared.lock().stop {
            return;
        }
        let loss = match outcome {
            Ok(l) => {
                valid_until = sent + lease_ttl(&l);
                wait = beat;
                let mut g = shared.lock();
                g.expires_at = l.expires_at;
                g.beats += 1;
                None
            }
            Err(e) if e.is_transient() => {
                if Instant::now() >= valid_until {
                    Some(LeaseLoss {
                        reason: format!("no heartbeat reached the hub for a whole lease time ({e})"),
                        confirmed_by_hub: false,
                    })
                } else {
                    wait = retry;
                    None
                }
            }
            Err(e) => Some(LeaseLoss { reason: e.to_string(), confirmed_by_hub: true }),
        };
        if let Some(loss) = loss {
            {
                let mut g = shared.lock();
                g.loss = Some(loss.clone());
            }
            shared.changed.notify_all();
            if let Some(f) = on_lost {
                f(&loss);
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(code: &str, message: &str, extra: Value) -> Vec<u8> {
        let mut e = json!({ "code": code, "message": message });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                e[k] = v;
            }
        }
        serde_json::to_vec(&json!({ "error": e })).unwrap()
    }

    #[test]
    fn every_failure_answer_maps_to_its_own_error() {
        let holder = json!({ "holder": { "user": "ann", "device_name": "Laptop", "since": 5, "expires_at": 95 } });
        match map_error(409, None, None, &body("locked", "profile is open: ann on Laptop", holder.clone())) {
            Error::Locked { holder, device, since, expires_at } => {
                assert_eq!((holder.as_str(), device.as_str(), since, expires_at), ("ann", "Laptop", 5, 95));
            }
            other => panic!("{other:?}"),
        }
        match map_error(403, None, None, &body("lease_invalid", "the lease is wrong", holder)) {
            Error::LeaseLost { holder, .. } => assert_eq!(holder.as_deref(), Some("ann on Laptop")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            map_error(403, None, None, &body("lease_required", "lock first", json!({}))),
            Error::LeaseLost { holder: None, .. }
        ));
        assert!(matches!(
            map_error(401, None, None, &body("unauthorized", "bad token", json!({}))),
            Error::Unauthorized(_)
        ));
        assert!(matches!(
            map_error(403, None, None, &body("forbidden", "viewers are read-only", json!({}))),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            map_error(404, None, None, &body("not_found", "no such profile", json!({}))),
            Error::NotFound(_)
        ));
        assert!(matches!(
            map_error(409, None, None, &body("stale", "x", json!({ "current_version": 4 }))),
            Error::Conflict { current_version: Some(4), .. }
        ));
        assert!(matches!(
            map_error(412, None, None, &body("stale", "x", json!({ "current_version": 7 }))),
            Error::Conflict { current_version: Some(7), .. }
        ));
        assert!(
            matches!(map_error(429, Some(30), None, &body("rate_limited", "x", json!({}))), Error::RateLimited { retry_after: Some(d) } if d.as_secs() == 30)
        );
        assert!(matches!(map_error(413, None, None, &body("too_large", "x", json!({}))), Error::TooLarge(_)));
        assert!(matches!(map_error(400, None, None, &body("invalid", "x", json!({}))), Error::Invalid(_)));
        assert!(matches!(
            map_error(500, None, None, &body("internal", "storage error", json!({}))),
            Error::Server { status: 500, .. }
        ));
        // not JSON at all (a proxy's error page) still yields something useful
        assert!(matches!(map_error(502, None, None, b"<html>Bad gateway</html>"), Error::Server { status: 502, .. }));
        assert!(
            matches!(map_error(301, None, Some("https://hub.example/"), b""), Error::Server { status: 301, message } if message.contains("https://hub.example/"))
        );
    }

    #[test]
    fn only_transient_errors_are_retried_and_lease_loss_is_recognised() {
        assert!(Error::Network("x".into()).is_transient());
        assert!(Error::Server { status: 503, message: String::new() }.is_transient());
        assert!(Error::RateLimited { retry_after: None }.is_transient());
        assert!(!Error::Unauthorized(String::new()).is_transient());
        assert!(!Error::LeaseLost { message: String::new(), holder: None }.is_transient());
        assert!(Error::LeaseLost { message: String::new(), holder: None }.is_lease_loss());
        assert!(
            Error::Locked { holder: String::new(), device: String::new(), since: 0, expires_at: 0 }.is_lease_loss()
        );
        assert!(!Error::Network("x".into()).is_lease_loss());
        assert!(!Error::Conflict { message: String::new(), current_version: None }.is_lease_loss());
    }

    #[test]
    fn ids_are_path_segments_and_queries_are_encoded() {
        assert_eq!(seg("kabc123").unwrap(), "kabc123");
        assert_eq!(seg("t_ab-9").unwrap(), "t_ab-9");
        for bad in ["", "../x", "a/b", "a b", "a?b", "é"] {
            assert!(seg(bad).is_err(), "{bad:?}");
        }
        assert_eq!(qs(&[]), "");
        assert_eq!(qs(&[("q", "a b&c=d".to_owned()), ("limit", "5".to_owned())]), "?q=a+b%26c%3Dd&limit=5");
    }

    #[test]
    fn construction_checks_its_inputs_and_debug_hides_the_token() {
        let dev = || Device::new("d1", "Laptop");
        assert!(Client::new("hub.example.org", "tok", dev()).is_err(), "scheme required");
        assert!(Client::new("http://", "tok", dev()).is_err());
        assert!(Client::new("http://127.0.0.1:1", "", dev()).is_err());
        assert!(Client::new("http://127.0.0.1:1", "has space", dev()).is_err());
        assert!(Client::new("http://127.0.0.1:1", "tok", Device::new(" ", "x")).is_err());
        let c = Client::new(" http://127.0.0.1:8470/ ", "citro_SECRETSECRET", dev()).unwrap();
        assert_eq!(c.base_url(), "http://127.0.0.1:8470");
        let shown = format!("{c:?}");
        assert!(!shown.contains("SECRETSECRET") && shown.contains("Laptop"), "{shown}");
        assert_eq!(c.with_device(Device::new("d2", "Desk")).device().id, "d2");
    }

    #[test]
    fn a_device_id_survives_restarts() {
        let dir = std::env::temp_dir().join(format!("citro-hub-device-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("device.json");
        let first = Device::load_or_create(&path, "Anna's laptop").unwrap();
        assert!(first.id.starts_with("dev-") && first.id.len() == 28, "{}", first.id);
        let again = Device::load_or_create(&path, "another name").unwrap();
        assert_eq!(again, first, "same id and name as the first time");
        fs::write(&path, "garbage").unwrap();
        let repaired = Device::load_or_create(&path, "x").unwrap();
        assert_ne!(repaired.id, first.id);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_hub_is_a_network_error() {
        // Port 1 on localhost: nothing listens there.
        let c = Client::new("http://127.0.0.1:1", "tok", Device::new("d", "n")).unwrap();
        assert!(matches!(c.me(), Err(Error::Network(_))));
        assert!(c.me().unwrap_err().is_transient());
    }
}
