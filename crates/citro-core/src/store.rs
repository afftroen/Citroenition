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

use crate::profile::{Group, Profile, TAG_COLORS, TAG_MAX, Tag};
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

/// Where the "my data lives elsewhere" pointer is kept: a one-line file outside the data folder
/// (`~/Library/Application Support/Citroenition/data-folder` on macOS, `$XDG_CONFIG_HOME/citroenition/data-folder`
/// elsewhere). Written by [`move_data`].
pub fn data_pointer() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = env("HOME").unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Citroenition/data-folder")
    } else {
        env("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")).join("citroenition/data-folder")
    }
}

fn pointed_data_folder() -> Option<PathBuf> {
    let text = fs::read_to_string(data_pointer()).ok()?;
    let p = PathBuf::from(text.lines().next()?.trim());
    p.is_absolute().then_some(p)
}

impl Home {
    /// `--home`, else `$CITRO_HOME`, else the folder chosen with [`move_data`], else `$XDG_DATA_HOME/citroenition`,
    /// else `~/.local/share/citroenition`.
    pub fn discover(explicit: Option<&Path>) -> Home {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let root = explicit
            .map(Path::to_path_buf)
            .or_else(|| env("CITRO_HOME"))
            .or_else(pointed_data_folder)
            .or_else(|| env("XDG_DATA_HOME").map(|p| p.join("citroenition")))
            .unwrap_or_else(|| env("HOME").unwrap_or_default().join(".local/share/citroenition"));
        Home { root }
    }

    /// `true` when `$CITRO_HOME` decides (a moved folder is then not used).
    pub fn from_env() -> bool {
        std::env::var_os("CITRO_HOME").is_some_and(|v| !v.is_empty())
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

    /// Everything about this device's team hub: the login, the sync bookkeeping, one marker per bound profile.
    pub fn hub_dir(&self) -> PathBuf {
        self.root.join("hub")
    }

    /// Is the profile bound to a team hub? A bound profile is not launched without the hub's lock:
    /// [`crate::launch::start`] refuses unless the sync layer cleared it ([`crate::launch::LaunchOptions::hub_cleared`]).
    pub fn hub_bound(&self, id: &str) -> bool {
        valid_id(id) && self.hub_dir().join("bound").join(id).is_file()
    }
}

/// A profile id that is safe to use as a file name (the hub makes `k` + 6 letters or digits).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Home {
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

/// [`locked`], but `None` at once when another holder has the lock (a caller that may itself be inside [`locked`]
/// must not wait for it).
pub fn try_locked<T>(home: &Home, f: impl FnOnce(&mut Store) -> Result<T>) -> Option<Result<T>> {
    fs::create_dir_all(&home.root).ok()?;
    let file = OpenOptions::new().create(true).write(true).truncate(false).open(home.root.join("store.lock")).ok()?;
    // SAFETY: plain flock(2) on an fd we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let _guard = Locked(file);
    Some(Store::open(home).and_then(|mut s| f(&mut s)))
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
    #[serde(default)]
    tags: Vec<Tag>,
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

    /// Insert or replace a profile that comes from a team hub, **keeping its id** (the id is the persona seed: it
    /// must be the same on every device). A profile that exists already keeps its local serial number, creation
    /// time and the newer of the two open/close times; a new one gets the next serial number.
    pub fn upsert_synced(&mut self, mut p: Profile) -> Result<Profile> {
        if p.profile_id.is_empty() {
            return Err(Error::Invalid("a synced profile needs an id".into()));
        }
        if p.group_id.is_empty() {
            p.group_id = "0".into();
        }
        if let Some(old) = self.data.profiles.iter_mut().find(|x| x.profile_id == p.profile_id) {
            p.serial_number = old.serial_number;
            p.created_time = old.created_time;
            p.last_open_time = old.last_open_time.max(p.last_open_time);
            p.last_close_time = old.last_close_time.max(p.last_close_time);
            *old = p.clone();
        } else {
            p.serial_number = self.data.next_serial;
            self.data.next_serial += 1;
            self.data.profiles.push(p.clone());
        }
        self.save()?;
        Ok(p)
    }

    /// The id of the group called `name` (case-insensitive), created when there is none.
    pub fn group_id_for(&mut self, name: &str) -> Result<String> {
        let name = name.trim();
        if let Some(g) = self.data.groups.iter().find(|g| g.group_name.eq_ignore_ascii_case(name)) {
            return Ok(g.group_id.clone());
        }
        Ok(self.create_group(name, "")?.group_id)
    }

    /// Give a profile another id (a hub assigns its own when a local profile is uploaded). `f` may adjust the
    /// record in the same save. The caller moves the data directory.
    pub fn rekey(&mut self, key: &str, new_id: &str, f: impl FnOnce(&mut Profile)) -> Result<Profile> {
        if self.find(new_id).is_some() {
            return Err(Error::Invalid(format!("a profile with id {new_id} exists already")));
        }
        let old = self.get(key)?.profile_id.clone();
        self.update(&old, |p| {
            p.profile_id = new_id.to_owned();
            f(p);
        })
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

    /// Record when the browser was closed. Never moves the time backwards: a close detected late carries
    /// an estimate that must not overwrite a newer, exact one.
    pub fn touch_closed(&mut self, key: &str, at: i64) -> Result<()> {
        self.update(key, |p| p.last_close_time = p.last_close_time.max(at)).map(|_| ())
    }

    pub fn tags(&self) -> &[Tag] {
        &self.data.tags
    }

    /// Create (empty `id`) or rename / recolor a tag. Names are trimmed, at most 50 characters, unique ignoring case.
    pub fn save_tag(&mut self, mut t: Tag) -> Result<Tag> {
        t.name = t.name.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.name.is_empty() {
            return Err(Error::Invalid("a tag needs a name".into()));
        }
        if t.name.chars().count() > TAG_MAX {
            return Err(Error::Invalid(format!("a tag name is at most {TAG_MAX} characters")));
        }
        if !TAG_COLORS.iter().any(|(k, _)| *k == t.color) {
            t.color = TAG_COLORS[self.data.tags.len() % TAG_COLORS.len()].0.to_owned();
        }
        if self.data.tags.iter().any(|x| x.id != t.id && x.name.eq_ignore_ascii_case(&t.name)) {
            return Err(Error::Invalid(format!("a tag called `{}` already exists", t.name)));
        }
        if t.id.is_empty() {
            let next = self.data.tags.iter().filter_map(|x| x.id.parse::<u64>().ok()).max().unwrap_or(0) + 1;
            t.id = next.to_string();
            self.data.tags.push(t.clone());
        } else {
            let slot = self
                .data
                .tags
                .iter_mut()
                .find(|x| x.id == t.id)
                .ok_or_else(|| Error::Invalid(format!("no tag {}", t.id)))?;
            *slot = t.clone();
        }
        self.save()?;
        Ok(t)
    }

    /// The tag called `name` (any case), created with the next color when missing.
    pub fn tag_id_for(&mut self, name: &str) -> Result<String> {
        let name = name.trim();
        if let Some(t) = self.data.tags.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
            return Ok(t.id.clone());
        }
        let short: String = name.chars().take(TAG_MAX).collect();
        Ok(self.save_tag(Tag { name: short, ..Default::default() })?.id)
    }

    /// Delete a tag; profiles lose it.
    pub fn delete_tag(&mut self, id: &str) -> Result<()> {
        self.data.tags.retain(|t| t.id != id);
        for p in &mut self.data.profiles {
            p.tags.retain(|t| t != id);
        }
        self.save()
    }

    /// Add `add` and remove `remove` (tag ids) on the profiles `keys`.
    pub fn retag(&mut self, keys: &[String], add: &[String], remove: &[String]) -> Result<()> {
        let ids: Vec<String> = keys.iter().filter_map(|k| self.find(k).map(|p| p.profile_id.clone())).collect();
        for p in self.data.profiles.iter_mut().filter(|p| ids.contains(&p.profile_id)) {
            p.tags.retain(|t| !remove.contains(t));
            for a in add {
                if !p.tags.contains(a) {
                    p.tags.push(a.clone());
                }
            }
        }
        self.save()
    }

    pub fn groups(&self) -> &[Group] {
        &self.data.groups
    }

    pub fn create_group(&mut self, name: &str, remark: &str) -> Result<Group> {
        let next = self.data.groups.iter().filter_map(|g| g.group_id.parse::<u64>().ok()).max().unwrap_or(0) + 1;
        let g = Group {
            group_id: next.to_string(),
            group_name: name.to_owned(),
            remark: remark.to_owned(),
            tabs: Vec::new(),
            bookmarks: Vec::new(),
        };
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

/// Copy a tree exactly: directories, regular files with their permission bits, and symbolic links as links (a macOS
/// kernel bundle is full of them).
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(to)?;
    fs::set_permissions(to, fs::Permissions::from_mode(fs::metadata(from)?.permissions().mode()))?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else if t.is_dir() {
            copy_tree(&src, &dst)?;
        } else if t.is_file() {
            fs::copy(&src, &dst)?;
        }
        // sockets and fifos (a running browser's) are not data
    }
    Ok(())
}

/// Move all data (profiles and their browser data, kernels, extensions, the app's settings) from `home` to `to` and
/// make `to` the data folder from now on. `to` must not exist yet or be empty. Refused while a browser runs. A rename
/// when both are on one disk, else a full copy that replaces the old folder only once it is complete.
pub fn move_data(home: &Home, to: &Path) -> Result<()> {
    move_data_with(home, to, &data_pointer())
}

fn move_data_with(home: &Home, to: &Path, pointer: &Path) -> Result<()> {
    let to = if to.is_absolute() { to.to_path_buf() } else { std::env::current_dir()?.join(to) };
    let from = fs::canonicalize(&home.root).unwrap_or_else(|_| home.root.clone());
    if to == from {
        return Ok(());
    }
    if to.starts_with(&from) || from.starts_with(&to) {
        return Err(Error::Invalid(
            "the new data folder cannot be inside the current one, or the other way round".into(),
        ));
    }
    if crate::launch::any_running(home) {
        return Err(Error::Invalid("close every browser first: an open browser is using the data folder".into()));
    }
    if to.exists() && fs::read_dir(&to)?.next().is_some() {
        return Err(Error::Invalid(format!("{} is not empty: choose a new or empty folder", to.display())));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if to.exists() {
        fs::remove_dir(&to)?;
    }
    if fs::rename(&from, &to).is_err() {
        // another disk: copy everything, then drop the original
        let staging =
            to.with_file_name(format!(".{}.citro-moving", to.file_name().and_then(|n| n.to_str()).unwrap_or("data")));
        let _ = fs::remove_dir_all(&staging);
        if let Err(e) = copy_tree(&from, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(e.into());
        }
        fs::rename(&staging, &to)?;
        fs::remove_dir_all(&from)?;
    }
    if let Some(dir) = pointer.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(pointer, format!("{}\n", to.display()))?;
    Ok(())
}

#[cfg(test)]
mod tag_tests {
    use super::*;

    #[test]
    fn tags_have_rules_and_follow_profiles() {
        let root = std::env::temp_dir().join(format!("citro-tags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        locked(&home, |s| {
            let a = s.create(Profile { name: "a".into(), ..Default::default() })?;
            let t = s.save_tag(Tag { name: "  TikTok   ads ".into(), color: "nope".into(), ..Default::default() })?;
            assert_eq!(
                (t.name.as_str(), t.color.as_str()),
                ("TikTok ads", "blue"),
                "trimmed; unknown color -> palette"
            );
            assert!(
                s.save_tag(Tag { name: "tiktok ADS".into(), ..Default::default() }).is_err(),
                "unique ignoring case"
            );
            assert!(s.save_tag(Tag { name: "x".repeat(51), ..Default::default() }).is_err(), "50 characters at most");
            assert_eq!(s.tag_id_for("TIKTOK ADS")?, t.id);
            let b = s.tag_id_for("Banned")?;
            s.retag(std::slice::from_ref(&a.profile_id), &[t.id.clone(), b.clone()], &[])?;
            s.retag(std::slice::from_ref(&a.profile_id), &[], std::slice::from_ref(&t.id))?;
            assert_eq!(s.get(&a.profile_id)?.tags, std::slice::from_ref(&b));
            s.delete_tag(&b)?;
            assert!(s.get(&a.profile_id)?.tags.is_empty() && s.tags().len() == 1);
            Ok(())
        })
        .unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    #[test]
    fn data_moves_with_its_links_and_the_pointer_follows() {
        let t = std::env::temp_dir().join(format!("citro-move-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let old = t.join("old");
        fs::create_dir_all(old.join("kernels/k/App.app/Contents")).unwrap();
        fs::write(old.join("store.json"), "{}").unwrap();
        fs::write(old.join("kernels/k/App.app/Contents/bin"), "x").unwrap();
        std::os::unix::fs::symlink("App.app/Contents/bin", old.join("kernels/k/link")).unwrap();
        let home = Home { root: old.clone() };
        let pointer = t.join("cfg/data-folder");
        // inside itself: refused
        assert!(move_data_with(&home, &old.join("sub"), &pointer).is_err());
        // a non-empty target: refused
        fs::create_dir_all(t.join("busy")).unwrap();
        fs::write(t.join("busy/f"), "").unwrap();
        assert!(move_data_with(&home, &t.join("busy"), &pointer).is_err());
        let new = t.join("elsewhere/data");
        move_data_with(&home, &new, &pointer).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("store.json")).unwrap(), "{}");
        assert_eq!(fs::read_link(new.join("kernels/k/link")).unwrap(), PathBuf::from("App.app/Contents/bin"));
        assert_eq!(fs::read_to_string(&pointer).unwrap().trim(), new.display().to_string());
        // the copy path (as across disks) keeps links and modes too
        let copy = t.join("copy");
        copy_tree(&new, &copy).unwrap();
        assert_eq!(fs::read_link(copy.join("kernels/k/link")).unwrap(), PathBuf::from("App.app/Contents/bin"));
        let _ = fs::remove_dir_all(&t);
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
