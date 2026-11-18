//! The store: the in-memory [`State`] behind one mutex, persisted as `<data>/state.json`.
//!
//! * Every operation runs under the mutex, so concurrent requests are strictly serialised (one lock winner).
//! * State-changing operations are written to disk (tmp + fsync + rename) before the response is sent; lease
//!   extensions are only marked dirty and flushed every `flush_interval` (a crash loses at most that much expiry).
//! * `users` is the one part of the state that is owned by the offline admin commands (`citro-hub admin ...`): they
//!   edit `state.json` under `state.lock`, and the server adopts the users whenever the file changed on disk.
//! * Blob files live under `<data>/blobs/<profile_id>/`, logs under `<data>/logs/`.

use super::error::ApiResult;
use super::fsio::{
    FileLock, Stamp, create_private_dir, read_state, serve_lock_path, stamp_of, state_lock_path, state_path,
    write_state,
};
use super::logs::{LogQuery, LogSink, SERVER_LOG};
use super::state::{Cx, Event, State};
use super::util::{now_ms, now_secs, scrub_secrets};
use crate::model::{LogKind, LogLevel, LogRecord, User};
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Everything configurable about a running hub.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// How long a lock lives without a heartbeat (unless the client asks for another TTL).
    pub lease_ttl: Duration,
    /// The longest TTL a client may ask for; longer requests are cut down to this.
    pub max_lease_ttl: Duration,
    /// Upper bound of one hosted data blob.
    pub max_blob_bytes: u64,
    /// How many versions of a profile's data are kept on disk (at least 1).
    pub keep_versions: usize,
    /// Upper bound of a JSON request body.
    pub max_json_bytes: usize,
    /// Failed logins per IP within `auth_fail_window` before the IP is blocked for `auth_block`.
    pub auth_fail_limit: u32,
    pub auth_fail_window: Duration,
    pub auth_block: Duration,
    /// Requests per minute and (address, user); `0` turns the limit off.
    pub rate_limit_per_min: u32,
    /// Header that carries the real client IP behind a reverse proxy (`X-Forwarded-For`, `CF-Connecting-IP`).
    pub real_ip_header: Option<String>,
    pub log_max_bytes: u64,
    pub log_files: usize,
    /// How often deferred writes (lease extensions) reach the disk.
    pub flush_interval: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            bind: SocketAddr::from(([127, 0, 0, 1], 7878)),
            lease_ttl: Duration::from_secs(90),
            max_lease_ttl: Duration::from_secs(600),
            max_blob_bytes: 512 * 1024 * 1024,
            keep_versions: super::blobs::DEFAULT_KEEP,
            max_json_bytes: 1024 * 1024,
            auth_fail_limit: 10,
            auth_fail_window: Duration::from_secs(60),
            auth_block: Duration::from_secs(60),
            rate_limit_per_min: 600,
            real_ip_header: None,
            log_max_bytes: 20 * 1024 * 1024,
            log_files: 3,
            flush_interval: Duration::from_secs(2),
        }
    }
}

struct Inner {
    state: State,
    /// `state.json` as we last read or wrote it.
    stamp: Option<Stamp>,
    /// In-memory changes the disk does not have yet.
    dirty: bool,
}

pub struct Store {
    dir: PathBuf,
    cfg: Config,
    inner: Mutex<Inner>,
    logs: LogSink,
    /// Held for the lifetime of the store: one server per data directory.
    _serve_lock: FileLock,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("dir", &self.dir).finish_non_exhaustive()
    }
}

/// A blob id from a URL is only ever used as a path component after this check.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

impl Store {
    /// Open an initialised data directory for serving. Fails when another server already holds it.
    pub fn open(dir: &Path, cfg: Config) -> io::Result<Store> {
        if !state_path(dir).exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{0} is not an initialised hub data directory; create its first user with \
                     `citro-hub admin create-user --data-dir {0} --name NAME --role owner`",
                    dir.display()
                ),
            ));
        }
        let serve_lock = FileLock::try_exclusive(&serve_lock_path(dir)).map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another citro-hub is already serving {}", dir.display()),
                )
            } else {
                e
            }
        })?;
        let _state_lock = FileLock::exclusive(&state_lock_path(dir))?;
        let mut state = read_state(dir)?;
        create_private_dir(&dir.join("blobs"))?;
        create_private_dir(&dir.join("logs"))?;
        if super::blobs::reconcile(dir, &mut state, now_secs(), cfg.keep_versions) {
            write_state(dir, &state)?;
        }
        let stamp = stamp_of(&state_path(dir));
        let logs = LogSink::new(dir.join("logs"), cfg.log_max_bytes, cfg.log_files);
        Ok(Store {
            dir: dir.to_owned(),
            cfg,
            inner: Mutex::new(Inner { state, stamp, dirty: false }),
            logs,
            _serve_lock: serve_lock,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `<data>/blobs/<profile_id>`; `None` for anything that is not a plain profile id.
    pub fn blob_dir(&self, pid: &str) -> Option<PathBuf> {
        safe_id(pid).then(|| self.dir.join("blobs").join(pid))
    }

    /// Adopt the users another process (the admin CLI) wrote to `state.json`.
    fn sync_external(&self, inner: &mut Inner) {
        let now = stamp_of(&state_path(&self.dir));
        if now == inner.stamp {
            return;
        }
        match read_state(&self.dir) {
            Ok(disk) => {
                // the admin tool's users win; users this server invited since (not on disk yet) are kept (the tool
                // never deletes a user, it only revokes)
                let mine: Vec<_> = inner
                    .state
                    .users
                    .iter()
                    .filter(|u| !disk.users.iter().any(|d| d.user.id == u.user.id || d.user.name == u.user.name))
                    .cloned()
                    .collect();
                inner.state.users = disk.users;
                inner.state.users.extend(mine);
                let State { users, locks, .. } = &mut inner.state;
                let before = locks.len();
                locks.retain(|_, l| users.iter().any(|u| u.user.id == l.user_id && !u.user.disabled));
                if locks.len() != before {
                    inner.dirty = true;
                }
            }
            Err(e) => tracing::warn!("state.json changed on disk but cannot be read: {e}"),
        }
        inner.stamp = now;
    }

    /// Write the state to disk (caller holds the mutex).
    fn persist(&self, inner: &mut Inner) -> io::Result<()> {
        let _file_lock = FileLock::exclusive(&state_lock_path(&self.dir))?;
        self.sync_external(inner);
        write_state(&self.dir, &inner.state)?;
        inner.stamp = stamp_of(&state_path(&self.dir));
        inner.dirty = false;
        Ok(())
    }

    /// Write deferred changes (lease extensions) to disk.
    pub fn flush(&self) -> io::Result<()> {
        let mut g = self.lock();
        if g.dirty { self.persist(&mut g) } else { Ok(()) }
    }

    /// Who owns this bearer token? (`None` for unknown and revoked tokens.)
    pub fn authenticate(&self, token: &str) -> Option<User> {
        let mut g = self.lock();
        self.sync_external(&mut g);
        g.state.user_by_token(token).map(|u| u.user.clone())
    }

    fn exec<T>(
        &self,
        user: &User,
        device_id: &str,
        write: bool,
        f: impl FnOnce(&mut State, &mut Cx) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let ttl_ms = i64::try_from(self.cfg.lease_ttl.as_millis()).unwrap_or(i64::MAX / 4).max(1);
        let mut g = self.lock();
        self.sync_external(&mut g);
        let mut cx = Cx::new(user.clone(), device_id, now_ms(), ttl_ms, self.cfg.max_blob_bytes);
        let result = f(&mut g.state, &mut cx);
        if write && result.is_ok() {
            if cx.lazy {
                g.dirty = true;
            } else if let Err(e) = self.persist(&mut g) {
                // The change is live in memory; keep serving and retry on the next flush.
                tracing::error!("cannot write state.json: {e}");
                g.dirty = true;
            }
        }
        drop(g);
        self.write_events(std::mem::take(&mut cx.events));
        result
    }

    /// Run a state-changing operation atomically and persist it.
    pub fn run<T>(
        &self,
        user: &User,
        device_id: &str,
        f: impl FnOnce(&mut State, &mut Cx) -> ApiResult<T>,
    ) -> ApiResult<T> {
        self.exec(user, device_id, true, f)
    }

    /// Run a read-only operation (still atomic with respect to everything else; nothing is persisted).
    pub fn read<T>(
        &self,
        user: &User,
        device_id: &str,
        f: impl FnOnce(&mut State, &mut Cx) -> ApiResult<T>,
    ) -> ApiResult<T> {
        self.exec(user, device_id, false, f)
    }

    // ---- logs --------------------------------------------------------------------------------------------------

    fn write_events(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        let ts = now_secs();
        let mut by_team: BTreeMap<String, Vec<LogRecord>> = BTreeMap::new();
        for e in events {
            by_team.entry(e.team_id.clone()).or_default().push(LogRecord {
                ts,
                kind: LogKind::Citro,
                level: e.level,
                team_id: e.team_id,
                user: e.user,
                device_id: e.device_id,
                profile_id: e.profile_id,
                message: scrub_secrets(&e.message),
            });
        }
        for (team, records) in by_team {
            if let Err(e) = self.logs.append(&team, &records) {
                tracing::warn!("cannot write the log of {team}: {e}");
            }
        }
    }

    /// A hub-level event that belongs to no team (failed logins, start-up); readable by server admins.
    pub fn log_server(&self, level: LogLevel, ip: Option<IpAddr>, message: &str) {
        let device = ip.map(|i| i.to_string()).unwrap_or_default();
        let rec = LogRecord {
            ts: now_secs(),
            kind: LogKind::Citro,
            level,
            team_id: SERVER_LOG.to_owned(),
            user: "hub".to_owned(),
            device_id: device,
            profile_id: None,
            message: scrub_secrets(message),
        };
        if let Err(e) = self.logs.append(SERVER_LOG, &[rec]) {
            tracing::warn!("cannot write the server log: {e}");
        }
    }

    /// Store client-posted records (already stamped by the state layer) in their teams' logs.
    pub fn append_logs(&self, records: Vec<(String, LogRecord)>) -> io::Result<()> {
        let mut by_team: BTreeMap<String, Vec<LogRecord>> = BTreeMap::new();
        for (team, rec) in records {
            by_team.entry(team).or_default().push(rec);
        }
        for (team, recs) in by_team {
            self.logs.append(&team, &recs)?;
        }
        Ok(())
    }

    pub fn query_logs(&self, team_id: &str, q: &LogQuery) -> ApiResult<Vec<LogRecord>> {
        Ok(self.logs.query(team_id, q)?)
    }
}
