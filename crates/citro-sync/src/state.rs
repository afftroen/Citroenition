//! What this device remembers about its hub: the login, the device identity, one marker per hub profile and
//! one note per live supervisor. All of it lives under `<home>/hub/` and is private to the user (mode 0600).

use crate::error::Result;
use citro_core::store::{Home, valid_id};
use citro_hub::client::Device;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A name that is unique on this machine for this moment (temporary files and directories).
pub fn uniq() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}{:x}", std::process::id(), nanos & 0xffff_ffff_ffff, N.fetch_add(1, Ordering::Relaxed))
}

fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Write `bytes` to `path` through a temporary file, readable by the owner only.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uniq()));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// ---- login -------------------------------------------------------------------------------------------------------

/// Where this device is logged in. The token is a secret: it stays out of `Debug` output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub url: String,
    pub token: String,
    pub team_id: String,
    pub team_name: String,
    /// The hub user the token belongs to.
    pub user: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("url", &self.url)
            .field("team", &self.team_name)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

fn login_file(home: &Home) -> PathBuf {
    home.hub_dir().join("login.json")
}

pub fn load_login(home: &Home) -> Result<Option<Login>> {
    match fs::read(login_file(home)) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_login(home: &Home, login: &Login) -> Result<()> {
    Ok(write_private(&login_file(home), &serde_json::to_vec_pretty(login)?)?)
}

pub fn remove_login(home: &Home) -> Result<bool> {
    match fs::remove_file(login_file(home)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

// ---- device ------------------------------------------------------------------------------------------------------

fn device_file(home: &Home) -> PathBuf {
    home.hub_dir().join("device.json")
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "this-device".to_owned())
}

/// This device's identity: a random id made once per Citroenition home, and a name people recognise (the host
/// name unless the user chose another).
pub fn device(home: &Home) -> Result<Device> {
    private_dir(&home.hub_dir())?;
    Ok(Device::load_or_create(&device_file(home), &hostname())?)
}

pub fn rename_device(home: &Home, name: &str) -> Result<Device> {
    let mut d = device(home)?;
    let name = name.trim();
    if !name.is_empty() && d.name != name {
        d.name = name.to_owned();
        write_private(&device_file(home), &serde_json::to_vec_pretty(&d)?)?;
    }
    Ok(d)
}

// ---- per-profile marker ------------------------------------------------------------------------------------------

/// Presence of this file makes a profile "belongs to the hub": it is not launched without the hub's lock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bound {
    pub team_id: String,
    /// SHA-256 of the data archive this device last pulled from or pushed to the hub (`""` = none yet).
    pub data_sha256: String,
    pub data_version: u64,
    /// A browser ran on the data directory since it was last in step with the hub: it is the newer copy.
    pub dirty: bool,
    /// Unix seconds of the last time this device and the hub agreed.
    pub synced_at: i64,
}

fn bound_file(home: &Home, id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| home.hub_dir().join("bound").join(id))
}

impl Bound {
    pub fn new(team_id: &str) -> Bound {
        Bound { team_id: team_id.to_owned(), ..Bound::default() }
    }

    pub fn load(home: &Home, id: &str) -> Option<Bound> {
        serde_json::from_slice(&fs::read(bound_file(home, id)?).ok()?).ok()
    }

    pub fn save(&self, home: &Home, id: &str) -> Result<()> {
        let path = bound_file(home, id).ok_or_else(|| crate::Error::Invalid(format!("`{id}` is not a profile id")))?;
        Ok(write_private(&path, &serde_json::to_vec_pretty(self)?)?)
    }

    pub fn remove(home: &Home, id: &str) -> Result<bool> {
        let Some(path) = bound_file(home, id) else { return Ok(false) };
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// The ids of every profile that belongs to the hub.
pub fn bound_ids(home: &Home) -> Vec<String> {
    let Ok(entries) = fs::read_dir(home.hub_dir().join("bound")) else { return Vec::new() };
    let mut ids: Vec<String> =
        entries.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| valid_id(n)).collect();
    ids.sort();
    ids
}

// ---- supervisors -------------------------------------------------------------------------------------------------

fn session_file(home: &Home, id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| home.hub_dir().join("sessions").join(id))
}

/// A supervisor (this process or another one) writes its pid here while it keeps the lock of `id` alive and waits
/// for the browser; `citro close` in another terminal waits for the file to go away.
pub fn write_session(home: &Home, id: &str) -> Result<()> {
    if let Some(path) = session_file(home, id) {
        write_private(&path, std::process::id().to_string().as_bytes())?;
    }
    Ok(())
}

pub fn clear_session(home: &Home, id: &str) {
    if let Some(path) = session_file(home, id) {
        let _ = fs::remove_file(path);
    }
}

/// Is a supervisor alive for this profile?
pub fn session_alive(home: &Home, id: &str) -> bool {
    let Some(pid) = session_file(home, id)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| t.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    // signal 0 only asks whether the process exists
    // SAFETY: `kill` with signal 0 sends nothing.
    unsafe { libc::kill(pid, 0) == 0 || *libc::__errno_location() == libc::EPERM }
}

/// A fresh path for an archive in transit.
pub fn tmp_archive(home: &Home, id: &str) -> io::Result<PathBuf> {
    let dir = home.hub_dir().join("tmp");
    private_dir(&dir)?;
    Ok(dir.join(format!("{id}-{}.tar.gz", uniq())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("citro-sync-state-{tag}-{}", uniq()));
        fs::create_dir_all(&dir).unwrap();
        Home { root: dir }
    }

    #[test]
    fn the_marker_makes_a_profile_hub_bound_and_removing_it_frees_the_profile() {
        let home = temp_home("bound");
        assert!(!home.hub_bound("kabc123"));
        Bound::new("t1").save(&home, "kabc123").unwrap();
        assert!(home.hub_bound("kabc123"));
        assert_eq!(bound_ids(&home), vec!["kabc123".to_owned()]);
        assert_eq!(Bound::load(&home, "kabc123").unwrap().team_id, "t1");
        assert!(Bound::remove(&home, "kabc123").unwrap());
        assert!(!home.hub_bound("kabc123"));
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn ids_that_could_escape_the_directory_are_refused() {
        let home = temp_home("ids");
        for bad in ["../x", "a/b", "", "a b", "..", "x\0y"] {
            assert!(Bound::new("t").save(&home, bad).is_err(), "{bad:?}");
            assert!(!home.hub_bound(bad));
        }
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn the_login_file_is_private_and_the_token_stays_out_of_debug_output() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("login");
        let login = Login {
            url: "https://hub.example".into(),
            token: "s3cret-token".into(),
            team_id: "t".into(),
            team_name: "Ops".into(),
            user: "mia".into(),
        };
        save_login(&home, &login).unwrap();
        let mode = fs::metadata(login_file(&home)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load_login(&home).unwrap().unwrap(), login);
        assert!(!format!("{login:?}").contains("s3cret"));
        assert!(remove_login(&home).unwrap());
        assert!(load_login(&home).unwrap().is_none());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_session_note_of_a_dead_process_is_not_alive() {
        let home = temp_home("sess");
        assert!(!session_alive(&home, "kabc123"));
        write_session(&home, "kabc123").unwrap();
        assert!(session_alive(&home, "kabc123"), "our own pid");
        write_private(&session_file(&home, "kabc123").unwrap(), b"4000000").unwrap();
        assert!(!session_alive(&home, "kabc123"));
        clear_session(&home, "kabc123");
        let _ = fs::remove_dir_all(&home.root);
    }
}
