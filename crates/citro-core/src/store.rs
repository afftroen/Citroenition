//! Persistent state under `$CITRO_HOME` (default `~/.local/share/citroenition`).
//!
//! ```text
//! store.json                 profiles + groups (mode 0600: it holds proxy credentials)
//! store.lock                 flock'd around every read-modify-write
//! profiles/<id>/data/        the kernel's --user-data-dir
//! profiles/<id>/cfg/         generated StaticConfig / DynamicConfig / WebGL files
//! profiles/<id>/kernel.log   the kernel's stderr
//! run/<id>.json              pid + debug endpoint of a running browser
//! ```

use crate::profile::{Group, Profile};
use crate::rng::Rng;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn discover(explicit: Option<&Path>) -> Home {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let root = explicit
            .map(Path::to_path_buf)
            .or_else(|| env("CITRO_HOME"))
            .or_else(|| env("XDG_DATA_HOME").map(|p| p.join("citroenition")))
            .unwrap_or_else(|| env("HOME").unwrap_or_default().join(".local/share/citroenition"));
        Home { root }
    }

    pub fn profile_dir(&self, id: &str) -> PathBuf {
        self.root.join("profiles").join(id)
    }

    pub fn run_file(&self, id: &str) -> PathBuf {
        self.root.join("run").join(format!("{id}.json"))
    }

    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    fn store_file(&self) -> PathBuf {
        self.root.join("store.json")
    }
}

/// Holds the store lock until dropped.
#[derive(Debug)]
pub struct Locked(#[allow(dead_code)] File);

pub fn lock(home: &Home) -> Result<Locked> {
    fs::create_dir_all(&home.root)?;
    let f = OpenOptions::new().create(true).write(true).truncate(false).open(home.root.join("store.lock"))?;
    // SAFETY: plain flock(2) on an fd we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(Locked(f))
}

/// Run `f` on a freshly opened store while holding the store lock (the read-modify-write pattern).
pub fn locked<T>(home: &Home, f: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    let _guard = lock(home)?;
    let mut store = Store::open(home)?;
    f(&mut store)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoreFile {
    next_serial: u64,
    groups: Vec<Group>,
    profiles: Vec<Profile>,
}

#[derive(Debug)]
pub struct Store {
    home: Home,
    data: StoreFile,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    pub fn open(home: &Home) -> Result<Store> {
        let mut data: StoreFile = match fs::read(home.store_file()) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile::default(),
            Err(e) => return Err(e.into()),
        };
        if data.next_serial == 0 {
            data.next_serial = 1;
        }
        Ok(Store { home: home.clone(), data })
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.home.root)?;
        let tmp = self.home.root.join("store.json.tmp");
        let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&self.data)?)?;
        f.sync_all()?;
        fs::rename(tmp, self.home.store_file())?;
        Ok(())
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.data.profiles
    }

    /// Look a profile up by id, or by serial number when `key` is all digits.
    pub fn find(&self, key: &str) -> Option<&Profile> {
        let key = key.trim();
        self.data.profiles.iter().find(|p| p.profile_id == key).or_else(|| {
            let serial: u64 = key.parse().ok()?;
            self.data.profiles.iter().find(|p| p.serial_number == serial)
        })
    }

    pub fn get(&self, key: &str) -> Result<&Profile> {
        self.find(key).ok_or_else(|| Error::ProfileNotFound(key.to_owned()))
    }

    fn fresh_id(&self) -> String {
        loop {
            let id = format!("k{}", Rng::from_entropy().token(6, b"abcdefghijklmnopqrstuvwxyz0123456789"));
            if self.find(&id).is_none() {
                return id;
            }
        }
    }

    /// Assigns the id, serial number and creation time.
    pub fn create(&mut self, mut p: Profile) -> Result<Profile> {
        if p.group_id.is_empty() {
            p.group_id = "0".into();
        }
        p.profile_id = self.fresh_id();
        p.serial_number = self.data.next_serial;
        self.data.next_serial += 1;
        p.created_time = now();
        self.data.profiles.push(p.clone());
        self.save()?;
        Ok(p)
    }

    pub fn update(&mut self, key: &str, f: impl FnOnce(&mut Profile)) -> Result<Profile> {
        let id = self.get(key)?.profile_id.clone();
        let p = self.data.profiles.iter_mut().find(|p| p.profile_id == id).expect("found above");
        f(p);
        let out = p.clone();
        self.save()?;
        Ok(out)
    }

    /// Removes the records and returns them (the caller removes data dirs / stops browsers).
    pub fn delete(&mut self, keys: &[String]) -> Result<Vec<Profile>> {
        let ids: Vec<String> = keys.iter().filter_map(|k| self.find(k).map(|p| p.profile_id.clone())).collect();
        let (gone, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.data.profiles).into_iter().partition(|p| ids.contains(&p.profile_id));
        self.data.profiles = kept;
        self.save()?;
        Ok(gone)
    }

    pub fn touch_opened(&mut self, key: &str) -> Result<()> {
        self.update(key, |p| p.last_open_time = now()).map(|_| ())
    }

    pub fn groups(&self) -> &[Group] {
        &self.data.groups
    }

    pub fn create_group(&mut self, name: &str, remark: &str) -> Result<Group> {
        let next = self.data.groups.iter().filter_map(|g| g.group_id.parse::<u64>().ok()).max().unwrap_or(0) + 1;
        let g = Group { group_id: next.to_string(), group_name: name.to_owned(), remark: remark.to_owned() };
        self.data.groups.push(g.clone());
        self.save()?;
        Ok(g)
    }

    pub fn group_name(&self, id: &str) -> String {
        if id == "0" || id.is_empty() {
            return "Ungrouped".into();
        }
        self.data.groups.iter().find(|g| g.group_id == id).map(|g| g.group_name.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("citro-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Home { root: dir }
    }

    #[test]
    fn create_find_update_delete_roundtrip() {
        let home = tmp_home("crud");
        let _g = lock(&home).unwrap();
        let mut s = Store::open(&home).unwrap();
        let a = s.create(Profile { name: "a".into(), ..Default::default() }).unwrap();
        let b = s.create(Profile { name: "b".into(), ..Default::default() }).unwrap();
        assert_eq!((a.serial_number, b.serial_number), (1, 2));
        assert_ne!(a.profile_id, b.profile_id);
        assert_eq!(s.get(&b.serial_number.to_string()).unwrap().name, "b");
        assert_eq!(s.get(&a.profile_id).unwrap().group_id, "0");

        s.update(&a.profile_id, |p| p.remark = "hello".into()).unwrap();
        let reopened = Store::open(&home).unwrap();
        assert_eq!(reopened.get(&a.profile_id).unwrap().remark, "hello");

        let mut s = reopened;
        let gone = s.delete(&[a.profile_id.clone(), "nope".into()]).unwrap();
        assert_eq!(gone.len(), 1);
        assert!(Store::open(&home).unwrap().find(&a.profile_id).is_none());
        // serials are never reused
        let c = s.create(Profile::default()).unwrap();
        assert_eq!(c.serial_number, 3);
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn store_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tmp_home("perm");
        let mut s = Store::open(&home).unwrap();
        s.create(Profile::default()).unwrap();
        let mode = fs::metadata(home.root.join("store.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn missing_profile_is_an_error() {
        let home = tmp_home("missing");
        let s = Store::open(&home).unwrap();
        assert!(matches!(s.get("zzz"), Err(Error::ProfileNotFound(_))));
    }
}
