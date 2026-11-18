//! The single-user backend: everything the GUI shows comes from `citro-core` (the same store, launcher and kernel
//! discovery the `citro` CLI uses), so the CLI and the GUI can be used side by side on one data directory.
//!
//! All methods block; the UI calls them from worker threads.

use super::local_hub::{self, CLOSE_PATIENCE, HubState};
use super::proxycheck;
use super::{
    BResult, Backend, BackendError, BackendInfo, BackendKind, Caps, HubStatus, KernelInfo, KernelStatus, Level,
    LogKind, LogLine, LogQuery, MASK, Notice, OpenError, OpenOpts, PluginPreview, ProxyCheck, Query, Row, RunInfo,
    RunState, TeamInfo, redact,
};
use crate::eventlog::EventLog;
use crate::installer::{KernelInstaller, StubInstaller};
use crate::query;
use crate::timefmt;
use crate::util::mask_secrets_in_text;
use citro_core::csv::{self, CsvRow};
use citro_core::kernel::Kernel;
use citro_core::launch::{self, LaunchOptions};
use citro_core::profile::{Group, Profile, ProxyConfig};
use citro_core::store::{self, Home, Store};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The newest part of `kernel.log` that is read for the Browser log tab.
const KERNEL_LOG_TAIL_BYTES: u64 = 512 * 1024;

pub struct LocalBackend {
    home: Home,
    events: Arc<EventLog>,
    installer: Arc<dyn KernelInstaller>,
    kernel_override: Mutex<Option<PathBuf>>,
    /// The team side: the cache of who has what open, notices, background refresh (see `local_hub`).
    pub(super) hub: HubState,
}

impl LocalBackend {
    /// Uses (and creates on demand) the data directory `home`. The GUI event log lives next to the store.
    pub fn new(home: Home) -> LocalBackend {
        let events = Arc::new(EventLog::with_file(home.root.join("gui-events.log")));
        LocalBackend {
            home,
            events,
            installer: Arc::new(StubInstaller),
            kernel_override: Mutex::new(None),
            hub: HubState::default(),
        }
    }

    /// Swap the stub for the real installer (`citro-install`) without touching any UI code.
    pub fn with_installer(mut self, installer: Arc<dyn KernelInstaller>) -> LocalBackend {
        self.installer = installer;
        self
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    pub fn events(&self) -> &Arc<EventLog> {
        &self.events
    }

    fn kernel(&self) -> citro_core::Result<Kernel> {
        let pinned = self.kernel_override.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Kernel::discover(&self.home, pinned.as_deref())
    }

    /// Notices browsers that exited since the last look (closed by the user, crashed, closed by the CLI),
    /// stores their `last_close_time` and logs them. Returns the live browsers.
    fn sync(&self) -> HashMap<String, launch::Running> {
        // closing a browser's last window quits it, also for browsers opened before this app started
        launch::watch_open_browsers(&self.home);
        // a team browser closed by its window: its supervisor uploaded the data and gave the lock back; say so once
        for o in citro_sync::take_finished() {
            self.report_outcome(&o, true);
        }
        let running = launch::list_running(&self.home); // also retires the records of dead browsers
        let closed = launch::take_closed(&self.home);
        if !closed.is_empty() {
            let named = store::locked(&self.home, |s| {
                let mut named = Vec::new();
                for c in &closed {
                    let name = s.find(&c.profile_id).map(|p| p.name.clone());
                    let _ = s.touch_closed(&c.profile_id, c.at); // deleted in the meantime is fine
                    named.push((c.profile_id.clone(), c.at, name));
                }
                Ok(named)
            });
            if let Ok(named) = named {
                for (id, at, name) in named {
                    let name = name.unwrap_or_else(|| id.clone());
                    self.events.push_at(at, Level::Info, Some(&id), format!("closed {name}"));
                }
            }
        }
        running.into_iter().map(|r| (r.profile_id.clone(), r)).collect()
    }

    pub(super) fn profile_name(&self, id: &str) -> String {
        Store::open(&self.home).ok().and_then(|s| s.find(id).map(|p| p.name.clone())).unwrap_or_else(|| id.to_owned())
    }
}

fn kernel_info(k: &Kernel) -> KernelInfo {
    KernelInfo { version: k.version.clone(), path: k.binary.display().to_string() }
}

/// Fields the user cannot change through an edit; they always keep the stored value.
fn keep_identity(new: &mut Profile, old: &Profile) {
    new.profile_id = old.profile_id.clone();
    new.serial_number = old.serial_number;
    new.created_time = old.created_time;
    new.last_open_time = old.last_open_time;
    new.last_close_time = old.last_close_time;
}

/// A masked secret in `new` (a caller passed a redacted record back) means "unchanged": keep the stored one.
fn restore_masked_secrets(new: &mut Profile, old: &Profile) {
    if new.password == MASK {
        new.password = old.password.clone();
    }
    if new.fakey == MASK {
        new.fakey = old.fakey.clone();
    }
    if new.cookie == MASK {
        new.cookie = old.cookie.clone();
    }
    if new.user_proxy_config.proxy_password.as_deref() == Some(MASK) {
        new.user_proxy_config.proxy_password = old.user_proxy_config.proxy_password.clone();
    }
}

/// Trims / normalises what the user typed and rejects what would break a launch.
fn normalize_and_validate(p: &mut Profile, groups: &[Group]) -> BResult<()> {
    p.name = p.name.trim().to_owned();
    if p.name.is_empty() {
        return Err(BackendError::Invalid("give the profile a name".into()));
    }
    p.remark = p.remark.trim_end().to_owned();
    p.persona_os = match p.persona_os.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => String::new(),
        "windows" | "win" => "windows".into(),
        "macos" | "mac" => "macos".into(),
        "linux" => "linux".into(),
        other => return Err(BackendError::Invalid(format!("unknown OS persona `{other}` (windows, macos or linux)"))),
    };
    p.country = p.country.trim().to_ascii_lowercase();
    if !p.country.is_empty() && !(p.country.len() == 2 && p.country.chars().all(|c| c.is_ascii_lowercase())) {
        return Err(BackendError::Invalid("the country is a 2-letter ISO code such as de or fr".into()));
    }
    p.tabs = p.tabs.iter().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
    if p.group_id.is_empty() {
        p.group_id = "0".into();
    }
    if p.group_id != "0" && !groups.iter().any(|g| g.group_id == p.group_id) {
        return Err(BackendError::Invalid(format!("group {} does not exist", p.group_id)));
    }
    if let Some(port) = p.user_proxy_config.proxy_port.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && port.parse::<u16>().ok().filter(|&n| n != 0).is_none()
    {
        return Err(BackendError::Invalid(format!("`{port}` is not a valid proxy port")));
    }
    Ok(())
}

/// Edits `store.json` under the store lock. `citro-core::Store` can create groups but not rename or delete them
/// yet, so those two go through the documented file format (`{next_serial, groups[], profiles[]}`) directly.
fn edit_store_file(home: &Home, edit: impl FnOnce(&mut Value) -> BResult<()>) -> BResult<()> {
    let _guard = store::lock(home)?;
    let path = home.root.join("store.json");
    let mut doc: Value = match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| BackendError::Other(format!("store.json is damaged: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    edit(&mut doc)?;
    let tmp = home.root.join("store.json.tmp");
    let mut f = fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(&doc).map_err(|e| BackendError::Other(e.to_string()))?)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn group_array(doc: &mut Value) -> BResult<&mut Vec<Value>> {
    doc.as_object_mut()
        .ok_or_else(|| BackendError::Other("store.json has an unexpected shape".into()))?
        .entry("groups")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| BackendError::Other("store.json: `groups` is not a list".into()))
}

/// `[12345:12345:1116/003412.123456:ERROR:foo.cc(12)] message` -> Error. Anything else is plain info.
fn browser_level(line: &str) -> Level {
    if !line.starts_with('[') {
        return Level::Info;
    }
    let head = &line[..line.find(']').unwrap_or(0)];
    if head.contains(":FATAL:") || head.contains(":ERROR:") {
        Level::Error
    } else if head.contains(":WARNING:") {
        Level::Warn
    } else if head.contains(":VERBOSE") {
        Level::Debug
    } else {
        Level::Info
    }
}

/// The last `KERNEL_LOG_TAIL_BYTES` of a log file as lines (the first, possibly cut, line is dropped).
fn read_tail_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    let cut = len > KERNEL_LOG_TAIL_BYTES;
    if cut {
        f.seek(SeekFrom::Start(len - KERNEL_LOG_TAIL_BYTES))?;
    }
    let mut bytes = Vec::new();
    f.take(KERNEL_LOG_TAIL_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_owned()).collect();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    Ok(lines)
}

fn browser_lines(path: &Path, q: &LogQuery) -> std::io::Result<Vec<LogLine>> {
    let needle = q.text.trim().to_lowercase();
    let mut out: Vec<LogLine> = read_tail_lines(path)?
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| LogLine {
            ts: 0,
            level: browser_level(&l),
            profile_id: q.profile_id.clone(),
            message: mask_secrets_in_text(&l),
        })
        .filter(|l| l.level >= q.level_min)
        .filter(|l| needle.is_empty() || l.message.to_lowercase().contains(&needle))
        .collect();
    if q.limit > 0 && out.len() > q.limit {
        out.drain(..out.len() - q.limit);
    }
    Ok(out)
}

impl Backend for LocalBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            kind: BackendKind::Local,
            data_dir: self.home.root.display().to_string(),
            caps: Caps { teams: true, kernel_install: !self.installer.is_stub(), proxy_check: true, group_edit: true },
        }
    }

    // profiles ------------------------------------------------------------------------------------------------
    fn list_profiles(&self, q: &Query) -> BResult<Vec<Row>> {
        let running = self.sync();
        self.keep_hub_fresh(); // background: the table never waits for the network
        let store = Store::open(&self.home)?;
        let bound: HashMap<String, String> = self.bound_teams().into_iter().collect();
        let rows: Vec<Row> = store
            .profiles()
            .iter()
            .map(|p| {
                let id = &p.profile_id;
                let run = running.get(id).map(|r| (r.pid, r.started, r.debug_port));
                match bound.get(id) {
                    Some(team) => Row {
                        run: local_hub::run_state(
                            run,
                            citro_sync::is_syncing(&self.home, id),
                            self.hub.lock_of(id).as_ref(),
                        ),
                        profile: redact(p),
                        team: Some(team.clone()),
                    },
                    None => Row {
                        run: run.map_or(RunState::Closed, |(pid, since, port)| RunState::Open { pid, since, port }),
                        profile: redact(p),
                        team: None,
                    },
                }
            })
            .collect();
        Ok(query::apply(rows, q, store.groups()))
    }

    fn profile(&self, id: &str) -> BResult<Profile> {
        Ok(Store::open(&self.home)?.get(id)?.clone())
    }

    fn save_profile(&self, mut p: Profile) -> BResult<Profile> {
        let groups = Store::open(&self.home)?.groups().to_vec();
        normalize_and_validate(&mut p, &groups)?;
        if p.profile_id.is_empty() {
            let created = store::locked(&self.home, |s| s.create(p))?;
            self.events.info(Some(&created.profile_id), format!("created profile {}", created.name));
            return Ok(created);
        }
        let id = p.profile_id.clone();
        // a local profile is updated in the store; a team profile is written through to the hub first (which refuses
        // while somebody has it open), so everybody gets the same one
        let saved = citro_sync::edit(&self.home, &id, |_, staged| {
            let old = staged.clone();
            restore_masked_secrets(&mut p, &old);
            keep_identity(&mut p, &old);
            *staged = p;
            Ok(())
        })?;
        self.events.info(Some(&id), format!("saved profile {}", saved.name));
        Ok(saved)
    }

    fn delete_profiles(&self, ids: &[String]) -> BResult<()> {
        let team = self.bound_names(ids);
        if !team.is_empty() {
            return Err(BackendError::Invalid(format!(
                "{} belong{} to the team hub: deleting removes {} for the whole team, not only here",
                team.join(", "),
                if team.len() == 1 { "s" } else { "" },
                if team.len() == 1 { "it" } else { "them" },
            )));
        }
        let gone = store::locked(&self.home, |s| s.delete(ids))?;
        for p in &gone {
            // stops the browser if it is still running, then removes the data directory
            if let Err(e) = launch::purge(&self.home, &p.profile_id) {
                self.events.warn(Some(&p.profile_id), format!("deleted {} but could not remove its data: {e}", p.name));
            }
            self.events.info(Some(&p.profile_id), format!("deleted profile {}", p.name));
        }
        if gone.is_empty() && !ids.is_empty() {
            return Err(BackendError::NotFound("none of the selected profiles exists any more".into()));
        }
        Ok(())
    }

    fn set_serial(&self, id: &str, n: u64) -> BResult<()> {
        store::locked(&self.home, |s| s.set_serial(id, n))?;
        Ok(())
    }

    fn set_remark(&self, id: &str, text: &str) -> BResult<()> {
        let text = text.trim_end().to_owned();
        citro_sync::edit(&self.home, id, |_, p| {
            p.remark = text;
            Ok(())
        })?;
        Ok(())
    }

    fn move_to_group(&self, ids: &[String], group: &str) -> BResult<()> {
        let group = if group.is_empty() { "0" } else { group };
        let name = Store::open(&self.home)?.group_name(group);
        for id in ids {
            // each profile on its own: a team profile goes to the hub (which also names the group there)
            citro_sync::edit(&self.home, id, |s, p| {
                if group != "0" && !s.groups().iter().any(|g| g.group_id == group) {
                    return Err(citro_core::Error::Invalid(format!("group {group} does not exist")));
                }
                p.group_id = group.to_owned();
                Ok(())
            })?;
        }
        self.events.info(None, format!("moved {} profile(s) to {name}", ids.len()));
        Ok(())
    }

    fn duplicate_profile(&self, id: &str) -> BResult<Profile> {
        let copy = store::locked(&self.home, |s| {
            let mut p = s.get(id)?.clone();
            p.name = format!("{} (copy)", p.name);
            // a copy is another machine: it gets its own seed (the new profile id) instead of sharing one
            p.fingerprint_config.0.remove("seed");
            p.last_open_time = 0;
            p.last_close_time = 0;
            s.create(p)
        })?;
        self.events.info(Some(&copy.profile_id), format!("duplicated profile as {}", copy.name));
        Ok(copy)
    }

    fn clear_cache(&self, id: &str) -> BResult<()> {
        let name = self.profile_name(id);
        if launch::running(&self.home, id).is_some() {
            return Err(BackendError::Invalid(format!("close {name} before clearing its cache")));
        }
        let data = self.home.profile_dir(id).join("data").join("Default");
        let mut freed = 0usize;
        for dir in ["Cache", "Code Cache", "GPUCache", "Service Worker/CacheStorage"] {
            let path = data.join(dir);
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
                freed += 1;
            }
        }
        self.events.info(Some(id), format!("cleared the cache of {name} ({freed} folders)"));
        Ok(())
    }

    fn check_proxy(&self, proxy: &ProxyConfig) -> BResult<ProxyCheck> {
        let r = proxycheck::check(proxy, Duration::from_secs(10))?;
        let level = if r.ok { Level::Info } else { Level::Warn };
        let what = proxy.hop().map(|h| format!("{}:{}", h.host, h.port)).unwrap_or_default();
        self.events.push(level, None, format!("proxy check {what}: {}", r.message));
        Ok(r)
    }

    // running -------------------------------------------------------------------------------------------------
    fn open(&self, id: &str, opts: &OpenOpts) -> Result<RunInfo, OpenError> {
        self.sync();
        let p =
            Store::open(&self.home).and_then(|s| s.get(id).cloned()).map_err(|e| OpenError::Other(e.to_string()))?;
        if let Some(r) = launch::running(&self.home, id) {
            return Ok(RunInfo { pid: r.pid, since: r.started }); // already open: nothing to do
        }
        let fail = |message: String| {
            self.events.error(Some(id), format!("could not open {}: {}", p.name, crate::util::first_line(&message)));
            Err(OpenError::Other(message))
        };
        if self.is_bound(id) && !matches!(citro_sync::load_login(&self.home), Ok(Some(_))) {
            return fail(format!(
                "{} belongs to a team hub and this device is not connected to it: connect on the Teams page first",
                p.name
            ));
        }
        let kernel = match self.kernel() {
            Ok(k) => k,
            Err(e) => return fail(e.to_string()),
        };
        let lo = LaunchOptions { headless: opts.headless, sandbox: opts.sandbox, ..Default::default() };
        // a team profile is opened with the hub's lock (and its newest data), a local one is launched as it is
        match citro_sync::open(&self.home, &kernel, &p, &lo) {
            Ok(r) => {
                if self.is_bound(id) {
                    self.hub.stale();
                } else {
                    let _ = store::locked(&self.home, |s| s.touch_opened(id));
                }
                let how = if opts.headless { ", headless" } else { "" };
                self.events.info(Some(id), format!("opened {} (pid {}{how})", p.name, r.pid));
                Ok(RunInfo { pid: r.pid, since: r.started })
            }
            Err(e) => match local_hub::open_error(e) {
                OpenError::LockedElsewhere { user, device, since } => {
                    self.hub.stale();
                    self.events.warn(Some(id), format!("{} is open on {device} by {user}: not opened here", p.name));
                    Err(OpenError::LockedElsewhere { user, device, since })
                }
                OpenError::Other(message) => fail(message),
            },
        }
    }

    fn close(&self, id: &str) -> BResult<()> {
        let name = self.profile_name(id);
        if self.is_bound(id) {
            // stops the browser, then waits until its data is on the hub and the lock is handed back, so the next
            // device can open it the moment this returns
            let report = citro_sync::close(&self.home, id, CLOSE_PATIENCE)?;
            self.hub.stale();
            if report.stopped {
                self.events.info(Some(id), format!("closed {name}"));
            } else {
                self.events.info(Some(id), format!("{name} was not running"));
            }
            return match report.outcome {
                Some(o) => {
                    self.report_outcome(&o, false);
                    if o.is_clean() {
                        Ok(())
                    } else {
                        Err(BackendError::Other(format!("{name} closed, but: {}", o.summary())))
                    }
                }
                None => Ok(()),
            };
        }
        let was_running = launch::stop(&self.home, id)?;
        self.sync(); // stores last_close_time and logs `closed <name>`
        if !was_running {
            self.events.info(Some(id), format!("{name} was not running"));
        }
        Ok(())
    }

    // tags ----------------------------------------------------------------------------------------------------
    fn tags(&self) -> BResult<Vec<super::Tag>> {
        Ok(Store::open(&self.home)?.tags().to_vec())
    }

    fn save_tag(&self, t: super::Tag) -> BResult<super::Tag> {
        Ok(store::locked(&self.home, |s| s.save_tag(t))?)
    }

    fn delete_tag(&self, id: &str) -> BResult<()> {
        Ok(store::locked(&self.home, |s| s.delete_tag(id))?)
    }

    fn retag(&self, ids: &[String], add: &[String], remove: &[String]) -> BResult<()> {
        Ok(store::locked(&self.home, |s| s.retag(ids, add, remove))?)
    }

    // extensions ----------------------------------------------------------------------------------------------
    fn extensions(&self) -> BResult<Vec<super::Extension>> {
        let mut all = citro_core::extensions::list(&self.home);
        all.sort_by_key(|e| e.name.to_lowercase());
        Ok(all)
    }

    fn add_extension(&self, input: &str) -> BResult<super::Extension> {
        let id = citro_core::extensions::id_from_input(input).ok_or_else(|| {
            BackendError::Invalid(
                "Paste a Chrome Web Store link (chromewebstore.google.com/detail/…) or an extension id".into(),
            )
        })?;
        // the store hands out the build for the browser that asks; without a kernel, a recent Chromium
        let major = self.kernel().map(|k| k.major).unwrap_or(150);
        let work = citro_core::extensions::root(&self.home).join(".download");
        let unpacked = citro_install::chrome_store::download_extension(&id, major, &work, Duration::from_secs(60))
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let e = citro_core::extensions::install(&self.home, &id, &unpacked, timefmt::now_ts())?;
        let _ = std::fs::remove_dir_all(&work);
        self.events.info(None, format!("installed extension {} {}", e.name, e.version));
        Ok(e)
    }

    fn remove_extension(&self, id: &str) -> BResult<()> {
        citro_core::extensions::remove(&self.home, id)?;
        self.events.info(None, format!("removed extension {id}"));
        Ok(())
    }

    fn assign_extension(&self, id: &str, all_profiles: bool, groups: Vec<String>) -> BResult<()> {
        Ok(citro_core::extensions::assign(&self.home, id, all_profiles, groups)?)
    }

    fn apply_import(&self, parsed: &citro_core::import::Parsed) -> BResult<citro_core::import::Report> {
        let r = citro_core::import::apply(&self.home, parsed)?;
        self.events.info(
            None,
            format!("imported {} profiles ({} skipped, {} new groups)", r.created, r.skipped, r.groups_created),
        );
        Ok(r)
    }

    fn probe(&self, p: &Profile) -> BResult<citro_core::probe::Probe> {
        let k = self.kernel()?;
        Ok(citro_core::probe::probe(&self.home, &k, p)?)
    }

    fn live_cookies(&self, id: &str) -> BResult<String> {
        let r = launch::running(&self.home, id).ok_or_else(|| {
            BackendError::Invalid("open the profile first: its cookies are read from the browser".into())
        })?;
        citro_core::cookies::export_live(&r.ws).map_err(BackendError::Invalid)
    }

    fn plugins(&self) -> BResult<Vec<citro_core::plugins::Plugin>> {
        Ok(citro_core::plugins::list(&self.home))
    }

    fn fetch_plugin(&self, input: &str) -> BResult<PluginPreview> {
        use citro_core::plugins;
        let work = plugins::root(&self.home).join(format!(".download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let f = citro_install::github::fetch(input, &work, Duration::from_secs(60))
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let dir = plugins::find_root(&f.dir)
            .ok_or_else(|| BackendError::Invalid(format!("there is no {} in {}", plugins::MANIFEST, f.source)))?;
        let manifest = plugins::read_manifest(&dir)?;
        let existing = plugins::list(&self.home).into_iter().find(|p| p.id() == manifest.id);
        Ok(PluginPreview { manifest, source: f.source, commit: f.commit, dir, existing })
    }

    fn install_plugin(&self, p: &PluginPreview) -> BResult<citro_core::plugins::Plugin> {
        let installed = citro_core::plugins::install(&self.home, &p.dir, &p.source, &p.commit)?;
        let work = citro_core::plugins::root(&self.home).join(format!(".download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        self.events.info(
            None,
            format!(
                "installed plugin {} {} from {}",
                installed.manifest.name, installed.manifest.version, installed.source
            ),
        );
        Ok(installed)
    }

    fn remove_plugin(&self, id: &str) -> BResult<()> {
        citro_core::plugins::remove(&self.home, id)?;
        self.events.info(None, format!("removed plugin {id}"));
        Ok(())
    }

    fn set_plugin_scope(&self, id: &str, all_profiles: bool, groups: Vec<String>) -> BResult<()> {
        Ok(citro_core::plugins::set_scope(&self.home, id, all_profiles, groups)?)
    }

    fn newest_plugin_commit(&self, id: &str) -> BResult<Option<(String, String)>> {
        use citro_install::github;
        let p = citro_core::plugins::list(&self.home)
            .into_iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| BackendError::NotFound(format!("plugin {id}")))?;
        if !p.is_github() {
            return Ok(None);
        }
        let src = github::GithubSource::parse(&p.source).map_err(|e| BackendError::Other(e.to_string()))?;
        let newest =
            github::latest_commit(&src, Duration::from_secs(30)).map_err(|e| BackendError::Other(e.to_string()))?;
        Ok((newest != p.commit).then(|| (newest.clone(), src.compare_url(&p.commit, &newest))))
    }

    fn move_data(&self, to: &str) -> BResult<()> {
        let to = crate::util::expand_tilde(to.trim(), crate::util::home_dir().as_deref());
        store::move_data(&self.home, &to)?;
        Ok(())
    }

    // groups --------------------------------------------------------------------------------------------------
    fn groups(&self) -> BResult<Vec<Group>> {
        Ok(Store::open(&self.home)?.groups().to_vec())
    }

    fn save_group(&self, g: Group) -> BResult<Group> {
        let name = g.group_name.trim().to_owned();
        if name.is_empty() {
            return Err(BackendError::Invalid("the group name must not be empty".into()));
        }
        let remark = g.remark.trim().to_owned();
        let tabs: Vec<String> = g.tabs.iter().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
        let taken = |groups: &[Group], except: &str| {
            groups.iter().any(|x| x.group_id != except && x.group_name.eq_ignore_ascii_case(&name))
        };
        if g.group_id.is_empty() {
            let created = store::locked(&self.home, |s| {
                if taken(s.groups(), "") {
                    return Err(citro_core::Error::Invalid(format!("a group called `{name}` already exists")));
                }
                s.create_group(&name, &remark)
            })?;
            let created = if tabs.is_empty() && g.bookmarks.is_empty() {
                created
            } else {
                self.save_group(Group { tabs: tabs.clone(), bookmarks: g.bookmarks.clone(), ..created })?
            };
            self.events.info(None, format!("created group {}", created.group_name));
            return Ok(created);
        }
        let id = g.group_id.clone();
        let mut saved = Group { group_id: id.clone(), group_name: name.clone(), remark, tabs, ..g.clone() };
        edit_store_file(&self.home, |doc| {
            let arr = group_array(doc)?;
            let others: Vec<Group> = arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
            if taken(&others, &id) {
                return Err(BackendError::Invalid(format!("a group called `{name}` already exists")));
            }
            let slot = arr
                .iter_mut()
                .find(|v| v.get("group_id").and_then(Value::as_str) == Some(id.as_str()))
                .ok_or_else(|| BackendError::NotFound(format!("group {id}")))?;
            slot["group_name"] = Value::String(saved.group_name.clone());
            slot["remark"] = Value::String(saved.remark.clone());
            slot["tabs"] = serde_json::json!(saved.tabs);
            slot["bookmarks"] = serde_json::json!(saved.bookmarks);
            // the rules, defaults and "this computer only"; the hub id stays what it is
            let set = |slot: &mut Value, k: &str, v: Value, empty: bool| {
                if empty {
                    if let Some(o) = slot.as_object_mut() {
                        o.remove(k);
                    }
                } else {
                    slot[k] = v;
                }
            };
            set(slot, "new_profile", serde_json::json!(saved.new_profile), saved.new_profile.is_none());
            set(slot, "access", serde_json::json!(saved.access), saved.access.is_default());
            set(slot, "local_only", Value::Bool(true), !saved.local_only);
            saved = serde_json::from_value(slot.clone()).unwrap_or(saved.clone());
            Ok(())
        })?;
        self.events.info(None, format!("updated group {}", saved.group_name));
        // a team group goes to the hub too: everybody's profiles of the group follow its settings
        if !saved.local_only
            && let Ok(Some(hub)) = citro_sync::Hub::connect(&self.home)
        {
            if let Err(e) = hub.push_group(&saved.group_id) {
                let msg = BackendError::from(e).to_string();
                self.events.warn(None, format!("group {} saved here, not on the team hub: {msg}", saved.group_name));
                return Err(BackendError::Other(format!("Saved on this computer, but the team hub refused it: {msg}")));
            }
            self.hub.stale();
        }
        Ok(saved)
    }

    fn delete_group(&self, id: &str) -> BResult<()> {
        if id.is_empty() || id == "0" {
            return Err(BackendError::Invalid("Ungrouped is built in and cannot be deleted".into()));
        }
        let mut deleted = String::new();
        edit_store_file(&self.home, |doc| {
            let arr = group_array(doc)?;
            let pos = arr
                .iter()
                .position(|v| v.get("group_id").and_then(Value::as_str) == Some(id))
                .ok_or_else(|| BackendError::NotFound(format!("group {id}")))?;
            deleted = arr.remove(pos).get("group_name").and_then(Value::as_str).unwrap_or(id).to_owned();
            if let Some(profiles) = doc.get_mut("profiles").and_then(Value::as_array_mut) {
                for p in profiles {
                    if p.get("group_id").and_then(Value::as_str) == Some(id) {
                        p["group_id"] = Value::String("0".into());
                    }
                }
            }
            Ok(())
        })?;
        self.events.info(None, format!("deleted group {deleted}; its profiles are now Ungrouped"));
        Ok(())
    }

    // export / logs -------------------------------------------------------------------------------------------
    fn export_csv(&self, ids: Option<Vec<String>>, group: Option<String>, bom: bool) -> BResult<Vec<u8>> {
        let running = self.sync();
        let store = Store::open(&self.home)?;
        let wanted: Option<Vec<&Profile>> =
            ids.as_ref().map(|ids| ids.iter().filter_map(|id| store.find(id)).collect());
        let all: Vec<&Profile> = match (wanted, &group) {
            (Some(w), _) => w,
            (None, Some(g)) => {
                let g = if g.is_empty() { "0" } else { g.as_str() };
                store
                    .profiles()
                    .iter()
                    .filter(|p| (if p.group_id.is_empty() { "0" } else { &p.group_id }) == g)
                    .collect()
            }
            (None, None) => store.profiles().iter().collect(),
        };
        let rows: Vec<CsvRow> = all
            .iter()
            .map(|p| {
                let group_name = match store.group_name(&p.group_id).as_str() {
                    "Ungrouped" => String::new(),
                    n => n.to_owned(),
                };
                CsvRow::from_profile(p, &group_name, running.contains_key(&p.profile_id))
            })
            .collect();
        Ok(csv::write(&rows, bom))
    }

    fn logs(&self, q: &LogQuery) -> BResult<Vec<LogLine>> {
        match q.kind {
            LogKind::Citro => Ok(self.events.query(q)),
            LogKind::Browser => match &q.profile_id {
                Some(id) => Ok(browser_lines(&launch::kernel_log(&self.home, id), q)?),
                None => Ok(Vec::new()),
            },
        }
    }

    // kernel --------------------------------------------------------------------------------------------------
    fn kernel_status(&self) -> BResult<KernelStatus> {
        let pinned = self.kernel_override.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let all: Vec<KernelInfo> = Kernel::installed(&self.home).iter().map(kernel_info).collect();
        let env_kernel = std::env::var_os("CITRO_KERNEL").is_some_and(|v| !v.is_empty());
        let source = if pinned.is_some() {
            "kernel path set in Settings"
        } else if env_kernel {
            "$CITRO_KERNEL"
        } else {
            "newest installed kernel"
        };
        let latest = self.installer.latest_version().ok().flatten();
        Ok(match Kernel::discover(&self.home, pinned.as_deref()) {
            Ok(k) => KernelStatus { installed: Some(kernel_info(&k)), latest, all, source: source.into(), error: None },
            Err(e) => KernelStatus { installed: None, latest, all, source: source.into(), error: Some(e.to_string()) },
        })
    }

    fn install_kernel(&self, progress: &dyn Fn(u64, u64)) -> BResult<KernelStatus> {
        match self.installer.install(&self.home.root, progress) {
            Ok(k) => self.events.info(None, format!("installed kernel {}", k.version)),
            Err(e) => {
                self.events.warn(None, format!("kernel install: {e}"));
                return Err(e);
            }
        }
        self.kernel_status()
    }

    // team ----------------------------------------------------------------------------------------------------
    fn teams(&self) -> BResult<Vec<TeamInfo>> {
        self.hub_teams()
    }

    fn hub_status(&self) -> HubStatus {
        self.hub_info()
    }

    fn connect_hub(&self, url: &str, token: &str, device_name: &str, team: &str) -> BResult<HubStatus> {
        self.hub_connect(url, token, device_name, team)
    }

    fn disconnect_hub(&self) -> BResult<HubStatus> {
        self.hub_disconnect()
    }

    fn force_unlock(&self, profile_id: &str) -> BResult<()> {
        self.hub_force_unlock(profile_id)
    }

    fn invite_member(&self, team_id: &str, name: &str, role: crate::backend::Role) -> BResult<String> {
        let hub = citro_sync::Hub::connect(&self.home)?.ok_or(citro_sync::Error::NotLoggedIn)?;
        let (m, token) = hub.client().invite(team_id, name, team_role(role)).map_err(citro_sync::Error::from)?;
        self.events.info(None, format!("invited {} to the team as {}", m.user_name, m.role.as_str()));
        Ok(token)
    }

    fn set_member_role(&self, team_id: &str, name: &str, role: crate::backend::Role) -> BResult<()> {
        let hub = citro_sync::Hub::connect(&self.home)?.ok_or(citro_sync::Error::NotLoggedIn)?;
        hub.client().add_member(team_id, name, team_role(role)).map_err(citro_sync::Error::from)?;
        self.events.info(None, format!("{name} is now {}", role.label().to_lowercase()));
        Ok(())
    }

    fn remove_member(&self, team_id: &str, user_id: &str) -> BResult<()> {
        let hub = citro_sync::Hub::connect(&self.home)?.ok_or(citro_sync::Error::NotLoggedIn)?;
        hub.client().remove_member(team_id, user_id).map_err(citro_sync::Error::from)?;
        self.events.info(None, "removed a member from the team".to_owned());
        Ok(())
    }

    fn new_member_token(&self, team_id: &str, user_id: &str) -> BResult<String> {
        let hub = citro_sync::Hub::connect(&self.home)?.ok_or(citro_sync::Error::NotLoggedIn)?;
        Ok(hub.client().new_member_token(team_id, user_id).map_err(citro_sync::Error::from)?)
    }

    fn team_people(&self) -> BResult<Option<citro_sync::People>> {
        match citro_sync::Hub::connect(&self.home)? {
            Some(hub) => Ok(Some(hub.people()?)),
            None => Ok(None),
        }
    }

    fn pull_team(&self) -> BResult<String> {
        self.hub_pull_now()
    }

    fn share_profiles(&self, ids: &[String]) -> BResult<String> {
        self.hub_share(ids)
    }

    fn delete_profiles_everywhere(&self, ids: &[String]) -> BResult<()> {
        let mut gone = 0usize;
        for id in ids {
            let name = self.profile_name(id);
            match citro_sync::delete(&self.home, id, true) {
                Ok(p) => {
                    gone += 1;
                    self.events.info(
                        Some(&p.profile_id),
                        format!("deleted profile {} (for everybody it was shared with)", p.name),
                    );
                }
                Err(citro_sync::Error::Core(citro_core::Error::ProfileNotFound(_))) => {}
                Err(e) => {
                    let msg = BackendError::from(e).to_string();
                    self.events.error(Some(id), format!("could not delete {name}: {msg}"));
                    return Err(BackendError::Other(format!("{name}: {msg}")));
                }
            }
        }
        self.hub.stale();
        if gone == 0 && !ids.is_empty() {
            return Err(BackendError::NotFound("none of the selected profiles exists any more".into()));
        }
        Ok(())
    }

    fn take_notices(&self) -> Vec<Notice> {
        for o in citro_sync::take_finished() {
            self.report_outcome(&o, true);
        }
        self.hub.take_notices()
    }

    fn shutdown(&self) -> Vec<String> {
        self.quit()
    }

    // misc ----------------------------------------------------------------------------------------------------
    fn record(&self, level: Level, profile_id: Option<&str>, message: &str) {
        self.events.push(level, profile_id, message);
    }

    fn set_kernel_override(&self, path: Option<String>) {
        *self.kernel_override.lock().unwrap_or_else(|e| e.into_inner()) =
            path.map(|p| crate::util::expand_tilde(&p, crate::util::home_dir().as_deref()));
    }
}

fn team_role(r: crate::backend::Role) -> citro_core::team::TeamRole {
    use citro_core::team::TeamRole;
    match r {
        crate::backend::Role::Owner => TeamRole::Owner,
        crate::backend::Role::Admin => TeamRole::Admin,
        crate::backend::Role::Member => TeamRole::Member,
        crate::backend::Role::Viewer => TeamRole::Viewer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpHome(Home);

    impl TmpHome {
        fn new(tag: &str) -> TmpHome {
            let dir = std::env::temp_dir().join(format!("citro-gui-local-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TmpHome(Home { root: dir })
        }

        fn backend(&self) -> LocalBackend {
            LocalBackend::new(self.0.clone())
        }
    }

    impl Drop for TmpHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.root);
        }
    }

    fn proxy(pass: &str) -> ProxyConfig {
        ProxyConfig::from_url(&format!("socks5://bob:{pass}@203.0.113.9:1080")).unwrap()
    }

    fn new_profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            country: "de".into(),
            persona_os: "linux".into(),
            password: "account-pass".into(),
            cookie: "sid=top-secret-cookie".into(),
            user_proxy_config: proxy("proxy-secret"),
            ..Default::default()
        }
    }

    #[test]
    fn rows_are_redacted_and_the_edit_record_is_not() {
        let t = TmpHome::new("rows");
        let b = t.backend();
        let p = b.save_profile(new_profile("Anna")).unwrap();
        let rows = b.list_profiles(&Query::default()).unwrap();
        assert_eq!(rows.len(), 1);
        let dump = format!("{rows:?}");
        for secret in ["proxy-secret", "account-pass", "top-secret-cookie"] {
            assert!(!dump.contains(secret), "{secret} leaked into the table rows");
        }
        assert_eq!(rows[0].run, RunState::Closed);
        let full = b.profile(&p.profile_id).unwrap();
        assert_eq!(full.password, "account-pass");
        assert_eq!(full.user_proxy_config.proxy_password.as_deref(), Some("proxy-secret"));
    }

    #[test]
    fn creating_normalises_and_validates() {
        let t = TmpHome::new("validate");
        let b = t.backend();
        let mut p = new_profile("  Padded  ");
        p.country = " DE ".into();
        p.persona_os = "Mac".into();
        p.tabs = vec![" https://a.example ".into(), "  ".into()];
        let saved = b.save_profile(p).unwrap();
        assert_eq!(
            (saved.name.as_str(), saved.country.as_str(), saved.persona_os.as_str(), saved.group_id.as_str()),
            ("Padded", "de", "macos", "0")
        );
        assert_eq!(saved.tabs, vec!["https://a.example"]);
        assert!(saved.profile_id.starts_with('k') && saved.serial_number == 1 && saved.created_time > 0);
        for (edit, needle) in [
            (Box::new(|p: &mut Profile| p.name = "  ".into()) as Box<dyn Fn(&mut Profile)>, "name"),
            (Box::new(|p| p.persona_os = "beos".into()), "persona"),
            (Box::new(|p| p.country = "germany".into()), "country"),
            (Box::new(|p| p.group_id = "77".into()), "group"),
            (Box::new(|p| p.user_proxy_config.proxy_port = Some("99999".into())), "port"),
        ] {
            let mut bad = new_profile("x");
            edit(&mut bad);
            let err = b.save_profile(bad).unwrap_err().to_string();
            assert!(err.contains(needle), "{err}");
        }
        assert_eq!(b.list_profiles(&Query::default()).unwrap().len(), 1, "invalid profiles are not stored");
    }

    #[test]
    fn editing_keeps_identity_times_and_unchanged_secrets() {
        let t = TmpHome::new("edit");
        let b = t.backend();
        let created = b.save_profile(new_profile("Anna")).unwrap();
        store::locked(&t.0, |s| s.touch_closed(&created.profile_id, 1_700_000_500)).unwrap();
        // a stale copy (as an edit dialog holds it) saved after the browser was closed meanwhile
        let mut stale = created.clone();
        stale.name = "Anna B".into();
        stale.last_close_time = 0;
        stale.serial_number = 99;
        let saved = b.save_profile(stale).unwrap();
        assert_eq!((saved.name.as_str(), saved.serial_number, saved.last_close_time), ("Anna B", 1, 1_700_000_500));
        // a record that came from the table (masked secrets) must not overwrite the real ones
        let mut masked = redact(&saved);
        masked.remark = "from the table".into();
        let saved = b.save_profile(masked).unwrap();
        assert_eq!(saved.password, "account-pass");
        assert_eq!(saved.cookie, "sid=top-secret-cookie");
        assert_eq!(saved.user_proxy_config.proxy_password.as_deref(), Some("proxy-secret"));
        assert_eq!(saved.remark, "from the table");
    }

    #[test]
    fn remark_group_duplicate_and_delete() {
        let t = TmpHome::new("ops");
        let b = t.backend();
        let a = b.save_profile(new_profile("Anna")).unwrap();
        let c = b.save_profile(new_profile("Chloé")).unwrap();
        let g = b.save_group(Group { group_name: "Vinted FR".into(), ..Default::default() }).unwrap();
        b.move_to_group(&[a.profile_id.clone(), c.profile_id.clone()], &g.group_id).unwrap();
        b.set_remark(&a.profile_id, "main account  \n").unwrap();
        assert!(b.move_to_group(std::slice::from_ref(&a.profile_id), "404").is_err());
        let rows = b.list_profiles(&Query { group: Some(g.group_id.clone()), ..Default::default() }).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(b.profile(&a.profile_id).unwrap().remark, "main account");

        let copy = b.duplicate_profile(&a.profile_id).unwrap();
        assert_eq!(copy.name, "Anna (copy)");
        assert_ne!(copy.profile_id, a.profile_id);
        assert_eq!((copy.last_open_time, copy.last_close_time), (0, 0));

        fs::create_dir_all(t.0.profile_dir(&c.profile_id).join("data")).unwrap();
        b.delete_profiles(std::slice::from_ref(&c.profile_id)).unwrap();
        assert!(!t.0.profile_dir(&c.profile_id).exists(), "the data directory goes with the profile");
        assert!(matches!(b.profile(&c.profile_id), Err(BackendError::NotFound(_))));
        assert!(b.delete_profiles(&[c.profile_id]).is_err(), "deleting twice says so");
        let log: Vec<String> =
            b.logs(&LogQuery { limit: 50, ..Default::default() }).unwrap().into_iter().map(|l| l.message).collect();
        assert!(log.iter().any(|m| m == "deleted profile Chloé"), "{log:?}");
        assert!(log.iter().any(|m| m.starts_with("created profile Anna")), "{log:?}");
    }

    #[test]
    fn groups_can_be_renamed_and_deleted_through_the_store_file() {
        let t = TmpHome::new("groups");
        let b = t.backend();
        let g1 =
            b.save_group(Group { group_name: "eBay DE".into(), remark: "old".into(), ..Default::default() }).unwrap();
        let g2 = b.save_group(Group { group_name: "Vinted FR".into(), ..Default::default() }).unwrap();
        assert!(
            b.save_group(Group { group_name: "ebay de".into(), ..Default::default() }).is_err(),
            "names are unique"
        );
        assert!(b.save_group(Group { group_name: " ".into(), ..Default::default() }).is_err());
        let p =
            b.save_profile(Profile { name: "P".into(), group_id: g1.group_id.clone(), ..Default::default() }).unwrap();

        let renamed = b
            .save_group(Group {
                group_id: g1.group_id.clone(),
                group_name: "eBay Germany".into(),
                remark: "new remark".into(),
                tabs: vec![" https://www.ebay.de/ ".into(), "".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!((renamed.group_name.as_str(), renamed.remark.as_str()), ("eBay Germany", "new remark"));
        assert_eq!(renamed.tabs, ["https://www.ebay.de/"], "group start pages are saved, trimmed");
        assert!(
            b.save_group(Group { group_id: g1.group_id.clone(), group_name: "vinted fr".into(), ..Default::default() })
                .is_err(),
            "renaming onto another group's name is refused"
        );
        // keeping its own name while changing the remark is fine
        b.save_group(Group {
            group_id: g2.group_id.clone(),
            group_name: "Vinted FR".into(),
            remark: "x".into(),
            tabs: Vec::new(),
            ..Default::default()
        })
        .unwrap();
        assert!(
            b.save_group(Group { group_id: "404".into(), group_name: "Nope".into(), ..Default::default() }).is_err()
        );

        // the real Store (the CLI's view) reads what the GUI wrote
        let groups = Store::open(&t.0).unwrap().groups().to_vec();
        assert_eq!(groups.iter().map(|g| g.group_name.as_str()).collect::<Vec<_>>(), ["eBay Germany", "Vinted FR"]);

        b.delete_group(&g1.group_id).unwrap();
        assert_eq!(b.groups().unwrap().len(), 1);
        assert_eq!(b.profile(&p.profile_id).unwrap().group_id, "0", "its profiles become Ungrouped");
        assert!(b.delete_group("0").is_err());
        assert!(b.delete_group(&g1.group_id).is_err());
        // serial numbers / other keys survive the raw edit
        let q = b.save_profile(Profile { name: "Q".into(), ..Default::default() }).unwrap();
        assert_eq!(q.serial_number, 2);
    }

    #[test]
    fn csv_export_has_no_secrets_and_follows_the_selection() {
        let t = TmpHome::new("csv");
        let b = t.backend();
        let g = b.save_group(Group { group_name: "Vinted FR".into(), ..Default::default() }).unwrap();
        let mut p = new_profile("=cmd|' /C calc'!A0");
        p.group_id = g.group_id.clone();
        let a = b.save_profile(p).unwrap();
        let c = b.save_profile(new_profile("Chloé")).unwrap();

        let all = b.export_csv(None, None, true).unwrap();
        assert!(all.starts_with(csv::UTF8_BOM));
        let text = String::from_utf8(all[3..].to_vec()).unwrap();
        for secret in ["proxy-secret", "account-pass", "top-secret-cookie"] {
            assert!(!text.contains(secret), "{secret} leaked into the CSV");
        }
        assert!(text.starts_with(&format!("{}\r\n", csv::HEADER.join(","))));
        assert!(text.contains("'=cmd|"), "formula injection is neutralised: {text}");
        assert!(text.contains(",Vinted FR,"));
        assert_eq!(text.matches("\r\n").count(), 3, "header + 2 rows");

        let one = String::from_utf8(b.export_csv(Some(vec![c.profile_id.clone()]), None, false).unwrap()).unwrap();
        assert!(!one.starts_with('\u{feff}') && one.contains("Chloé") && !one.contains("cmd"));
        let by_group = String::from_utf8(b.export_csv(None, Some(g.group_id), false).unwrap()).unwrap();
        assert!(by_group.contains("cmd") && !by_group.contains("Chloé"));
        let none = b.export_csv(Some(vec![]), None, false).unwrap();
        assert_eq!(String::from_utf8(none).unwrap(), format!("{}\r\n", csv::HEADER.join(",")));
        let _ = a;
    }

    #[test]
    fn kernel_status_finds_installed_kernels_and_honours_the_override() {
        let t = TmpHome::new("kernel");
        let b = t.backend();
        let st = b.kernel_status().unwrap();
        assert!(st.installed.is_none() && st.all.is_empty());
        assert!(st.error.as_deref().is_some_and(|e| e.contains("citro kernel install")), "{:?}", st.error);

        // in the layout this platform's installer leaves: `chrome`, or the app bundle's executable on a Mac
        let bin = |v: &str| t.0.root.join("kernels").join(format!("clearcote-{v}")).join(citro_core::kernel::BINARY);
        for v in ["150.0.1.2", "151.0.7922.108"] {
            fs::create_dir_all(bin(v).parent().unwrap()).unwrap();
            fs::write(bin(v), "#!/bin/sh\n").unwrap();
        }
        let st = b.kernel_status().unwrap();
        assert_eq!(st.installed.as_ref().map(|k| k.version.as_str()), Some("151.0.7922.108"));
        assert_eq!(st.all.len(), 2);
        assert!(st.error.is_none());

        let pinned = bin("150.0.1.2");
        b.set_kernel_override(Some(pinned.display().to_string()));
        let st = b.kernel_status().unwrap();
        assert_eq!(st.installed.unwrap().version, "150.0.1.2");
        assert!(st.source.contains("Settings"));
        b.set_kernel_override(Some("/nonexistent/chrome".into()));
        assert!(b.kernel_status().unwrap().error.is_some());
        b.set_kernel_override(None);
        assert_eq!(b.kernel_status().unwrap().installed.unwrap().version, "151.0.7922.108");

        let e = b.install_kernel(&|_, _| {}).unwrap_err().to_string();
        assert!(e.contains("citro kernel install"), "{e}");
    }

    #[test]
    fn opening_without_a_kernel_fails_cleanly_and_is_logged() {
        let t = TmpHome::new("open");
        let b = t.backend();
        let p = b.save_profile(new_profile("Anna")).unwrap();
        let e = b.open(&p.profile_id, &OpenOpts::default()).unwrap_err();
        let OpenError::Other(msg) = e else { panic!("expected a plain error") };
        assert!(msg.contains("no Clearcote kernel found"), "{msg}");
        let log = b.logs(&LogQuery { level_min: Level::Error, ..Default::default() }).unwrap();
        assert_eq!(log.len(), 1);
        assert!(log[0].message.starts_with("could not open Anna"), "{}", log[0].message);
        assert_eq!(log[0].profile_id.as_deref(), Some(p.profile_id.as_str()));
        assert!(matches!(b.open("nope", &OpenOpts::default()), Err(OpenError::Other(_))));
    }

    #[test]
    fn browser_log_tail_is_classified_filtered_and_scrubbed() {
        let t = TmpHome::new("klog");
        let b = t.backend();
        let p = b.save_profile(new_profile("Anna")).unwrap();
        let path = launch::kernel_log(&t.0, &p.profile_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "[1:1:1116/003412.1:INFO:chrome.cc(1)] started\n\
             [1:1:1116/003413.1:WARNING:net.cc(2)] slow\n\
             [1:1:1116/003414.1:ERROR:gpu.cc(3)] via socks5://bob:proxy-secret@203.0.113.9:1080 failed\n\
             plain stderr line\n",
        )
        .unwrap();
        let q = |level_min, text: &str, limit| LogQuery {
            kind: LogKind::Browser,
            profile_id: Some(p.profile_id.clone()),
            level_min,
            text: text.into(),
            limit,
        };
        let all = b.logs(&q(Level::Trace, "", 0)).unwrap();
        assert_eq!(
            all.iter().map(|l| l.level).collect::<Vec<_>>(),
            [Level::Info, Level::Warn, Level::Error, Level::Info]
        );
        assert!(all.iter().all(|l| !l.message.contains("proxy-secret")), "{all:?}");
        assert_eq!(b.logs(&q(Level::Warn, "", 0)).unwrap().len(), 2);
        assert_eq!(b.logs(&q(Level::Trace, "SLOW", 0)).unwrap().len(), 1);
        assert_eq!(b.logs(&q(Level::Trace, "", 2)).unwrap().len(), 2);
        // no log yet / no profile chosen
        let other = b.save_profile(new_profile("Chloé")).unwrap();
        let none = LogQuery { kind: LogKind::Browser, profile_id: Some(other.profile_id), ..Default::default() };
        assert!(b.logs(&none).unwrap().is_empty());
        assert!(b.logs(&LogQuery { kind: LogKind::Browser, ..Default::default() }).unwrap().is_empty());
    }

    #[test]
    fn a_big_log_is_read_from_its_end_without_a_torn_first_line() {
        let t = TmpHome::new("klog-big");
        let path = t.0.root.join("big.log");
        let line = "[1:1:1116/003412.1:INFO:x.cc(1)] 0123456789012345678901234567890123456789\n";
        let n = (KERNEL_LOG_TAIL_BYTES as usize / line.len()) * 3;
        fs::write(&path, line.repeat(n)).unwrap();
        let lines = read_tail_lines(&path).unwrap();
        assert!(lines.len() < n && lines.len() > n / 4);
        assert!(lines.iter().all(|l| l.starts_with("[1:1:")), "a cut line leaked in");
    }

    #[test]
    fn local_backend_describes_itself() {
        let t = TmpHome::new("info");
        let i = t.backend().info();
        assert_eq!(i.kind, BackendKind::Local);
        assert!(i.caps.teams && i.caps.proxy_check && i.caps.group_edit);
        assert!(!i.caps.kernel_install, "the installer is a stub for now");
        assert_eq!(i.data_dir, t.0.root.display().to_string());
        let b = t.backend();
        assert!(!b.hub_status().connected);
        // no hub yet: nothing to list, nothing to pull, and a team action says what to do first
        assert!(b.teams().unwrap().is_empty());
        assert!(b.take_notices().is_empty() && b.shutdown().is_empty());
        assert!(b.pull_team().is_err() && b.force_unlock("k1").is_err());
        let status = b.disconnect_hub().unwrap();
        assert!(!status.connected, "logging out of nothing is not an error");
        // a hub that is not there is refused in words, and the device stays unconnected
        assert!(b.connect_hub("http://127.0.0.1:1", "tok", "dev", "").is_err());
        assert!(!b.hub_status().connected);
    }
}
