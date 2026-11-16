//! One browser on one device, under the hub's lock.
//!
//! **Open**: take the lock (refused, naming who has it, when another device or user does), keep it alive, pull the
//! cookies and profile data when the hub's copy is newer, launch.
//! **While it runs**: a supervisor thread heartbeats the lease and watches the browser. A lost lease stops the
//! browser: the profile is not this device's any more.
//! **Close**: the supervisor packs the data directory (caches left out), uploads it, then unlocks. The last-closed
//! time is stamped on the hub only after the data is safe there.
//!
//! A supervisor lives in the process that opened the profile (the CLI waits for it, the Local API server and the
//! app keep it in a thread). `citro close` from another process stops the browser and waits for that supervisor.

use crate::error::{Error, Result};
use crate::hub::Hub;
use crate::pack;
use crate::state::{self, Bound, now, uniq};
use citro_core::kernel::Kernel;
use citro_core::launch::{self, LaunchOptions, Running};
use citro_core::profile::Profile;
use citro_core::store::{self, Home};
use citro_hub::client::{Client, Error as HubError, LeaseKeeper, LeaseLoss, UploadOptions};
use citro_hub::model::{DataMeta, LogEntry, LogKind, LogLevel};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long a close keeps retrying an upload the hub (or the network) refuses for a transient reason. The lease is
/// kept alive meanwhile.
const UPLOAD_PATIENCE: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(250);

static LIVE: Mutex<BTreeMap<String, JoinHandle<Outcome>>> = Mutex::new(BTreeMap::new());
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn live() -> MutexGuard<'static, BTreeMap<String, JoinHandle<Outcome>>> {
    LIVE.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a finished session did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub profile_id: String,
    /// The hub version this close stored (`None` = nothing went up).
    pub uploaded: Option<u64>,
    /// The lock was handed back.
    pub released: bool,
    /// Why the lease was lost while the browser ran, if it was.
    pub lost: Option<String>,
    /// Data that could not be merged and was kept aside (`data.conflict-<time>`).
    pub conflict: Option<PathBuf>,
    pub notes: Vec<String>,
    /// Something that needs the user's eyes.
    pub error: Option<String>,
}

impl Outcome {
    /// Everything went as it should.
    pub fn is_clean(&self) -> bool {
        self.error.is_none() && self.released && self.lost.is_none() && self.conflict.is_none()
    }

    /// One line for a terminal or a toast.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.uploaded {
            Some(v) => parts.push(format!("profile data uploaded (version {v})")),
            None => parts.push("no new profile data went up".to_owned()),
        }
        parts.push(if self.released { "lock released".into() } else { "lock NOT released".into() });
        if let Some(l) = &self.lost {
            parts.push(format!("the lock was lost while it ran ({l})"));
        }
        if let Some(c) = &self.conflict {
            parts.push(format!("what changed here was kept in {}", c.display()));
        }
        if let Some(e) = &self.error {
            parts.push(e.clone());
        }
        parts.join("; ")
    }
}

/// The part of the work that runs in the supervisor thread.
struct Session {
    home: Home,
    client: Client,
    team_id: String,
    id: String,
    device: String,
    keeper: Option<LeaseKeeper>,
    /// What the hub and this device agreed on last (updated by a successful upload).
    bound: Bound,
    /// Length of the kernel log when the browser started: what comes after is this session's.
    log_from: u64,
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Session {
    fn log(&self, level: LogLevel, message: impl Into<String>) {
        let e = LogEntry {
            ts: now(),
            kind: LogKind::Citro,
            level,
            profile_id: Some(self.id.clone()),
            team_id: Some(self.team_id.clone()),
            message: message.into(),
        };
        let _ = self.client.push_logs(&[e]);
    }

    /// The browser's own output of this session goes to the team's browser log (the client scrubs secrets).
    fn ship_browser_log(&self) {
        let Ok(mut f) = File::open(launch::kernel_log(&self.home, &self.id)) else { return };
        let len = f.metadata().map_or(0, |m| m.len());
        let from = self.log_from.min(len).max(len.saturating_sub(64 * 1024));
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(from)).is_err() || Read::take(&mut f, 64 * 1024).read_to_end(&mut buf).is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&buf);
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let entries: Vec<LogEntry> = lines[lines.len().saturating_sub(200)..]
            .iter()
            .map(|l| LogEntry {
                ts: now(),
                kind: LogKind::Browser,
                level: LogLevel::Info,
                profile_id: Some(self.id.clone()),
                team_id: Some(self.team_id.clone()),
                message: l.chars().take(500).collect(),
            })
            .collect();
        if !entries.is_empty() {
            let _ = self.client.push_logs(&entries);
        }
    }

    fn save_bound(&self) {
        let _ = self.bound.save(&self.home, &self.id);
    }

    /// Move what this device has aside, never merge it: the next open then pulls the hub's copy.
    fn keep_aside(&mut self) -> Option<PathBuf> {
        let root = self.home.profile_dir(&self.id);
        let data = root.join("data");
        let kept = data.exists().then(|| root.join(format!("data.conflict-{}-{}", now(), &uniq()[..4])));
        let kept = kept.filter(|dst| fs::rename(&data, dst).is_ok());
        self.bound.data_sha256.clear();
        self.bound.data_version = 0;
        self.bound.dirty = false;
        self.save_bound();
        kept
    }

    /// Pack, upload, record. The lease keeps beating the whole time.
    fn upload(&mut self, out: &mut Outcome) {
        let archive = match state::tmp_archive(&self.home, &self.id) {
            Ok(p) => p,
            Err(e) => return out.error = Some(format!("could not prepare the upload: {e}")),
        };
        let result = self.upload_archive(&archive, out);
        let _ = fs::remove_file(&archive);
        if let Err(e) = result {
            out.error = Some(e);
        }
    }

    fn upload_archive(&mut self, archive: &Path, out: &mut Outcome) -> std::result::Result<(), String> {
        let data = self.home.profile_dir(&self.id).join("data");
        let packed = pack::pack(&data, archive).map_err(|e| format!("could not pack the profile data: {e}"))?;
        let sha = sha256_file(archive).map_err(|e| e.to_string())?;
        if sha == self.bound.data_sha256 {
            // the same bytes the hub already has
            self.bound.dirty = false;
            self.bound.synced_at = now();
            self.save_bound();
            out.notes.push("the profile data did not change".into());
            return Ok(());
        }
        let opts = UploadOptions { base_version: Some(self.bound.data_version), ..UploadOptions::default() };
        let deadline = Instant::now() + UPLOAD_PATIENCE;
        let mut delay = Duration::from_secs(1);
        let meta: std::result::Result<DataMeta, HubError> = loop {
            let lease = match self.keeper.as_ref().filter(|k| k.is_held()) {
                Some(k) => k.lease().to_owned(),
                None => break Err(HubError::Network("the lock was lost during the upload".into())),
            };
            match self.client.upload_data_file(&self.id, &lease, archive, &opts) {
                Ok(m) => break Ok(m),
                Err(e) if e.is_transient() && Instant::now() < deadline => {
                    thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(15));
                }
                Err(e) => break Err(e),
            }
        };
        match meta {
            Ok(m) => {
                self.bound = Bound {
                    team_id: self.team_id.clone(),
                    data_sha256: if m.sha256.is_empty() { sha } else { m.sha256.clone() },
                    data_version: m.version,
                    dirty: false,
                    synced_at: now(),
                };
                self.save_bound();
                out.uploaded = Some(m.version);
                self.log(
                    LogLevel::Info,
                    format!(
                        "profile data uploaded: version {} ({} files, {} KiB packed)",
                        m.version,
                        packed.files,
                        packed.archive.div_ceil(1024)
                    ),
                );
                Ok(())
            }
            Err(HubError::Conflict { .. }) => {
                out.conflict = self.keep_aside();
                self.log(LogLevel::Warn, "the hub's copy changed while this device had the profile: kept aside here");
                Err("the hub's data changed while this device held the profile; what this device has was kept aside \
                     and the next open pulls the hub's copy"
                    .to_owned())
            }
            Err(e) => Err(format!(
                "the profile data could not be uploaded ({e}); it stays on this device and goes up the next time \
                 the profile is closed or `citro hub recover` runs"
            )),
        }
    }
}

/// Everything after the browser is gone: upload, release.
fn finish(s: &mut Session, lost: Option<LeaseLoss>) -> Outcome {
    let mut out = Outcome { profile_id: s.id.clone(), ..Outcome::default() };
    s.ship_browser_log();
    if let Some(l) = &lost {
        out.lost = Some(l.reason.clone());
        s.log(LogLevel::Warn, format!("the lock was lost ({}): the browser was stopped", l.reason));
        if let Some(k) = s.keeper.take() {
            k.stop();
        }
        // nobody else took it in the meantime? then it can still be handed back properly
        match s.client.lock(&s.id) {
            Ok(lease) => s.keeper = Some(LeaseKeeper::start(&s.client, &s.id, &lease)),
            Err(e) => {
                out.conflict = s.keep_aside();
                out.error = Some(format!("{e}; what this device did meanwhile was kept aside, not merged"));
                return out;
            }
        }
    }
    s.upload(&mut out);
    if let Some(k) = s.keeper.take() {
        match k.release(true) {
            Ok(()) => out.released = true,
            Err(e) => out.notes.push(format!("the lock could not be released ({e}); it frees itself in a moment")),
        }
    }
    out
}

fn supervise(mut s: Session) -> Outcome {
    let mut lost = None;
    loop {
        if launch::running(&s.home, &s.id).is_none() {
            break;
        }
        if let Some(l) = s.keeper.as_ref().and_then(LeaseKeeper::loss) {
            lost = Some(l);
            let _ = launch::stop(&s.home, &s.id);
            break;
        }
        if SHUTDOWN.load(Ordering::Relaxed) {
            let _ = launch::stop(&s.home, &s.id);
            break;
        }
        thread::sleep(POLL);
    }
    let _ = launch::sync_closed(&s.home);
    let out = finish(&mut s, lost);
    state::clear_session(&s.home, &s.id);
    out
}

fn file_len(p: &Path) -> u64 {
    fs::metadata(p).map_or(0, |m| m.len())
}

/// Start the supervisor thread of an opened (or adopted) browser.
fn spawn(session: Session) -> Result<()> {
    state::write_session(&session.home, &session.id)?;
    let id = session.id.clone();
    let handle = thread::Builder::new().name(format!("sync-{id}")).spawn(move || supervise(session))?;
    live().insert(id, handle);
    Ok(())
}

impl Hub {
    /// Open a hub profile: lock, pull, launch, supervise. Returns the running browser at once; the supervisor
    /// uploads and unlocks when it closes.
    pub fn open(&self, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<Running> {
        let id = p.profile_id.clone();
        if let Some(r) = launch::running(&self.home, &id) {
            if !is_syncing(&id) && !state::session_alive(&self.home, &id) {
                // its session ended without it (the process that opened it died): back under the lock first
                if let Some(bad) = self.adopt().into_iter().find(|o| o.profile_id == id) {
                    return Err(Error::Invalid(bad.summary()));
                }
            }
            return Ok(r);
        }
        {
            let mut l = live();
            match l.get(&id) {
                Some(h) if !h.is_finished() => {
                    return Err(Error::Invalid(format!("{id} is still being closed: its data is going up to the hub")));
                }
                Some(_) => drop(l.remove(&id)),
                None => {}
            }
        }
        if state::session_alive(&self.home, &id) {
            return Err(Error::Invalid(format!("{id} is being closed by another Citroenition process")));
        }
        let lease = self.client.lock(&id)?; // refused with the holder named when somebody else has it
        let keeper = LeaseKeeper::start(&self.client, &id, &lease);
        let mut bound = Bound::load(&self.home, &id).unwrap_or_else(|| Bound::new(&self.login.team_id));
        let started = self.prepare_and_launch(kernel, &id, opts, &keeper, &mut bound);
        let (running, log_from) = match started {
            Ok(v) => v,
            Err(e) => {
                let _ = keeper.release(false);
                return Err(e);
            }
        };
        let _ = store::locked(&self.home, |s| s.touch_opened(&id));
        let session = Session {
            home: self.home.clone(),
            client: self.client.clone(),
            team_id: self.login.team_id.clone(),
            id: id.clone(),
            device: self.device_name().to_owned(),
            keeper: Some(keeper),
            bound,
            log_from,
        };
        session.log(LogLevel::Info, format!("opened on {} (kernel {})", session.device, running.kernel_version));
        spawn(session)?;
        Ok(running)
    }

    /// Browsers of hub profiles that run on this device without a supervisor (the process that opened them is
    /// gone): take their lock again and watch them. When somebody else has the profile by now the browser is
    /// stopped and what it changed stays aside. What could not be set right is returned.
    pub fn adopt(&self) -> Vec<Outcome> {
        let mut problems = Vec::new();
        for id in state::bound_ids(&self.home) {
            if launch::running(&self.home, &id).is_none() || is_syncing(&id) || state::session_alive(&self.home, &id) {
                continue;
            }
            let Some(bound) = Bound::load(&self.home, &id).filter(|b| b.team_id == self.login.team_id) else {
                continue;
            };
            let mut s = Session {
                home: self.home.clone(),
                client: self.client.clone(),
                team_id: self.login.team_id.clone(),
                id: id.clone(),
                device: self.device_name().to_owned(),
                keeper: None,
                bound,
                log_from: file_len(&launch::kernel_log(&self.home, &id)),
            };
            match self.client.lock(&id) {
                Ok(lease) => {
                    s.keeper = Some(LeaseKeeper::start(&self.client, &id, &lease));
                    s.bound.dirty = true;
                    s.save_bound();
                    s.log(LogLevel::Info, "took over a running browser whose session had ended");
                    if let Err(e) = spawn(s) {
                        problems.push(Outcome { profile_id: id, error: Some(e.to_string()), ..Outcome::default() });
                    }
                }
                Err(HubError::Locked { holder, device, .. }) => {
                    let _ = launch::stop(&self.home, &id);
                    let _ = launch::sync_closed(&self.home);
                    s.log(
                        LogLevel::Warn,
                        format!(
                            "a browser that outlived its session was stopped: {holder} on {device} has the profile"
                        ),
                    );
                    let kept = s.keep_aside();
                    problems.push(Outcome {
                        profile_id: id,
                        error: Some(format!(
                            "this browser outlived its session and {holder} on {device} has the profile now: it was stopped and what it changed was kept aside"
                        )),
                        conflict: kept,
                        ..Outcome::default()
                    });
                }
                Err(_) => {} // the hub cannot be reached: the next call tries again
            }
        }
        problems
    }

    fn prepare_and_launch(
        &self,
        kernel: &Kernel,
        id: &str,
        opts: &LaunchOptions,
        keeper: &LeaseKeeper,
        bound: &mut Bound,
    ) -> Result<(Running, u64)> {
        // secrets (proxy password, cookies, logins) are served to the holder of the lease only
        let hp = self.client.profile(id, Some(keeper.lease()))?;
        if let Some(path) = self.pull_data(id, keeper.lease(), &hp.data, bound)? {
            let _ = path; // a conflict note was already logged by pull_data
        }
        let was_dirty = bound.dirty;
        bound.dirty = true; // from here on the local copy is the newer one
        bound.save(&self.home, id)?;
        let mut o = opts.clone();
        o.hub_cleared = true;
        let log_from = file_len(&launch::kernel_log(&self.home, id));
        match launch::start(&self.home, kernel, &hp.profile, &o) {
            Ok(r) => Ok((r, log_from)),
            Err(e) => {
                bound.dirty = was_dirty;
                let _ = bound.save(&self.home, id);
                Err(e.into())
            }
        }
    }

    /// Make the local data directory the hub's current copy (when it is not already). A local copy that is newer
    /// than the one this device last agreed on, while the hub moved on too, is kept aside, never merged.
    fn pull_data(&self, id: &str, lease: &str, meta: &DataMeta, bound: &mut Bound) -> Result<Option<PathBuf>> {
        if meta.version == 0 {
            return Ok(None); // the hub holds nothing yet: what is here is the first copy
        }
        if !meta.sha256.is_empty() && meta.sha256 == bound.data_sha256 {
            bound.data_version = meta.version;
            return Ok(None); // already the hub's copy (or a newer one of ours built on it)
        }
        let root = self.home.profile_dir(id);
        fs::create_dir_all(&root)?;
        let archive = root.join(format!(".incoming-{}.tar.gz", uniq()));
        let fetched = match self.client.download_data_to_file(id, lease, &archive)? {
            Some(f) => f,
            None => return Ok(None),
        };
        let incoming = root.join(format!("data.incoming-{}", uniq()));
        let unpacked = pack::unpack(&archive, &incoming);
        let _ = fs::remove_file(&archive);
        unpacked?;
        let data = root.join("data");
        let mut conflict = None;
        let mut old = None;
        if data.exists() {
            if bound.dirty && bound.data_sha256 != fetched.sha256 {
                let c = root.join(format!("data.conflict-{}-{}", now(), &uniq()[..4]));
                fs::rename(&data, &c)?;
                conflict = Some(c);
            } else {
                let o = root.join(format!("data.old-{}", uniq()));
                fs::rename(&data, &o)?;
                old = Some(o);
            }
        }
        fs::rename(&incoming, &data)?;
        if let Some(o) = old {
            let _ = fs::remove_dir_all(o);
        }
        *bound = Bound {
            team_id: self.login.team_id.clone(),
            data_sha256: fetched.sha256,
            data_version: fetched.version,
            dirty: false,
            synced_at: now(),
        };
        bound.save(&self.home, id)?;
        if let Some(c) = &conflict {
            let e = LogEntry {
                ts: now(),
                kind: LogKind::Citro,
                level: LogLevel::Warn,
                profile_id: Some(id.to_owned()),
                team_id: Some(self.login.team_id.clone()),
                message: format!(
                    "local changes of {} were kept in {} and the hub's copy was pulled",
                    self.device_name(),
                    c.display()
                ),
            };
            let _ = self.client.push_logs(&[e]);
        }
        Ok(conflict)
    }
}

/// Lock a profile that has no browser, upload its data directory, unlock (what a first push or a recovery does).
pub(crate) fn publish(hub: &Hub, id: &str) -> Result<Outcome> {
    let lease = hub.client.lock(id)?;
    let keeper = LeaseKeeper::start(&hub.client, id, &lease);
    let mut s = Session {
        home: hub.home.clone(),
        client: hub.client.clone(),
        team_id: hub.login.team_id.clone(),
        id: id.to_owned(),
        device: hub.device_name().to_owned(),
        keeper: Some(keeper),
        bound: Bound::load(&hub.home, id).unwrap_or_else(|| Bound::new(&hub.login.team_id)),
        log_from: u64::MAX,
    };
    // the hub's current numbers decide what "based on" means for this upload
    if let Ok(hp) = hub.client.profile(id, Some(s.keeper.as_ref().map_or("", |k| k.lease()))) {
        s.bound.data_version = hp.data.version;
        if hp.data.version == 0 {
            s.bound.data_sha256.clear();
        }
    }
    Ok(finish(&mut s, None))
}

/// Finish what an interrupted session left: the browser is gone, the lock may be gone, the newer data is only here.
pub fn recover(home: &Home, key: &str) -> Result<Outcome> {
    let hub = Hub::connect(home)?.ok_or(Error::NotLoggedIn)?;
    let id = hub.resolve(key)?;
    let bound = Bound::load(home, &id).ok_or_else(|| Error::Invalid(format!("{id} is not a hub profile")))?;
    if launch::running(home, &id).is_some() || is_syncing(&id) {
        return Err(Error::Invalid(format!("{id} is open: close it first")));
    }
    if !bound.dirty {
        return Ok(Outcome {
            profile_id: id,
            released: true,
            notes: vec!["nothing to recover".into()],
            ..Outcome::default()
        });
    }
    publish(&hub, &id)
}

/// Open a profile. Profiles of a team hub go through the hub's lock; local ones are launched as they are.
pub fn open(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<Running> {
    if !home.hub_bound(&p.profile_id) {
        return Ok(launch::start(home, kernel, p, opts)?);
    }
    Hub::connect(home)?.ok_or(Error::NotLoggedIn)?.open(kernel, p, opts)
}

/// What [`close`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloseReport {
    /// The browser was running and was stopped.
    pub stopped: bool,
    /// The hub side of the close (hub profiles only; `None` when nobody was supervising and nothing was pending).
    pub outcome: Option<Outcome>,
}

/// Close a profile's browser. For a hub profile this also waits (up to `patience`) until its data is uploaded and
/// the lock released, so the next device can open it the moment this returns.
pub fn close(home: &Home, id: &str, patience: Duration) -> Result<CloseReport> {
    let stopped = launch::stop(home, id)?;
    let _ = launch::sync_closed(home);
    if !home.hub_bound(id) {
        return Ok(CloseReport { stopped, outcome: None });
    }
    if live().contains_key(id) {
        return Ok(CloseReport { stopped, outcome: wait(id, patience) });
    }
    let deadline = Instant::now() + patience;
    while state::session_alive(home, id) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
    }
    if !state::session_alive(home, id) && Bound::load(home, id).is_some_and(|b| b.dirty) {
        return Ok(CloseReport { stopped, outcome: Some(recover(home, id)?) });
    }
    Ok(CloseReport { stopped, outcome: None })
}

/// Is a supervisor of this process still working on the profile?
pub fn is_syncing(id: &str) -> bool {
    live().get(id).is_some_and(|h| !h.is_finished())
}

/// Wait until this process's supervisor of `id` is done: browser closed, data uploaded, lock released.
pub fn wait(id: &str, patience: Duration) -> Option<Outcome> {
    let deadline = Instant::now() + patience;
    loop {
        match live().get(id) {
            None => return None,
            Some(h) if h.is_finished() => break,
            Some(_) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(100));
    }
    live().remove(id)?.join().ok()
}

/// Close the browsers of the hub profiles this process supervises, upload their data and hand the locks back. The
/// app and `citro serve` do this when they quit: nothing else would keep those locks alive, and a lock that expires
/// under a running browser is how two devices end up with one profile.
pub fn shutdown(patience: Duration) -> Vec<Outcome> {
    SHUTDOWN.store(true, Ordering::SeqCst);
    let ids: Vec<String> = live().keys().cloned().collect();
    ids.into_iter().filter_map(|id| wait(&id, patience)).collect()
}

/// The outcomes of the supervisors that finished since the last call (the app shows them as notices).
pub fn take_finished() -> Vec<Outcome> {
    let mut l = live();
    let done: Vec<String> = l.iter().filter(|(_, h)| h.is_finished()).map(|(id, _)| id.clone()).collect();
    done.into_iter().filter_map(|id| l.remove(&id)).filter_map(|h| h.join().ok()).collect()
}

extern "C" fn on_signal(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// For a process that supervises in the foreground (the CLI): Ctrl-C, SIGTERM and SIGHUP close the browser
/// properly, so its data is uploaded and the lock released before the process ends.
pub fn catch_signals() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores into an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
}
