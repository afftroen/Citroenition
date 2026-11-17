//! Self-update: replace the unpacked release this app runs from with a newer one.
//!
//! Finding, downloading, checking and unpacking a release is `citro_install::app_update`. This module knows the
//! release layout (one folder holding `citro`, `citro-hub`, `citro-install`, the docs and either `citro-gui` or
//! `Citroenition.app`) and swaps it entry by entry: every old entry is moved aside first, every new one moved in,
//! and any failure moves everything back. Moves are renames inside the install folder, so a half-copied app is never
//! left behind. The running program keeps its (now unlinked) files open and is restarted afterwards.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use citro_install::app_update::AppRelease;

/// Where downloads are staged, inside the install folder (same file system, so the swap is a rename).
const STAGING: &str = ".citroenition-update";
/// Where the old entries wait while the swap runs.
const BACKUP: &str = ".citroenition-previous";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A release folder this app runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The folder that holds `citro`, the docs and the app.
    pub root: PathBuf,
    /// `Citroenition.app` on macOS.
    pub app_bundle: Option<PathBuf>,
}

impl Install {
    /// The installation the running program belongs to, or why it cannot update itself (a `cargo build`, an app
    /// moved out of its folder, a read-only location).
    pub fn current() -> Result<Install, String> {
        let exe =
            std::env::current_exe().and_then(fs::canonicalize).map_err(|e| format!("cannot locate the app: {e}"))?;
        Install::from_exe(&exe)
    }

    pub fn from_exe(exe: &Path) -> Result<Install, String> {
        let bundle = exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf);
        let root = match &bundle {
            Some(b) => b.parent(),
            None => exe.parent(),
        }
        .ok_or("the app has no folder")?
        .to_path_buf();
        let is_release = ["citro", "citro-install", "CHANGELOG.md"].iter().all(|f| root.join(f).is_file());
        if !is_release {
            return Err(format!(
                "{} is not an unpacked Citroenition release (a development build, or the app was moved out of its \
                 folder): update by downloading the new release",
                root.display()
            ));
        }
        let probe = root.join(".citroenition-write-test");
        fs::write(&probe, b"").map_err(|e| format!("cannot write to {}: {e}", root.display()))?;
        let _ = fs::remove_file(&probe);
        Ok(Install { root, app_bundle: bundle })
    }

    /// Ask GitHub for the newest release (`None` = this version is the newest).
    pub fn check() -> Result<Option<AppRelease>, String> {
        let r = citro_install::app_update::latest_app_release(LOOKUP_TIMEOUT).map_err(|e| e.to_string())?;
        Ok(r.is_newer_than(env!("CARGO_PKG_VERSION")).then_some(r))
    }

    /// Download, verify, unpack and swap in `release`. `progress(done, total)` follows the download.
    pub fn update(&self, release: &AppRelease, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
        let staging = self.root.join(STAGING);
        let new_root = citro_install::app_update::download_app_release(release, &staging, DOWNLOAD_TIMEOUT, progress)
            .map_err(|e| e.to_string())?;
        let result = swap(&self.root, &new_root);
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Start the (new) app and let the caller quit this one.
    pub fn relaunch(&self) -> Result<(), String> {
        let spawned = match &self.app_bundle {
            Some(b) => std::process::Command::new("open").arg("-n").arg(b).spawn(),
            None => std::process::Command::new(self.root.join("citro-gui")).spawn(),
        };
        spawned.map(drop).map_err(|e| format!("could not start the new version: {e}"))
    }
}

/// Start this program again (a new instance of the app bundle on macOS) so the caller can quit.
pub fn relaunch_current() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf);
    let spawned = match bundle {
        Some(b) => std::process::Command::new("open").arg("-n").arg(b).spawn(),
        None => std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn(),
    };
    spawned.map(drop).map_err(|e| format!("could not start Citroenition again: {e}"))
}

/// Replace every entry of `root` that `new_root` has with the new one. All or nothing.
fn swap(root: &Path, new_root: &Path) -> Result<(), String> {
    let backup = root.join(BACKUP);
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|e| format!("cannot clear {}: {e}", backup.display()))?;
    }
    fs::create_dir(&backup).map_err(|e| format!("cannot create {}: {e}", backup.display()))?;
    let mut names: Vec<_> = fs::read_dir(new_root)
        .map_err(|e| format!("cannot read the new release: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    names.sort();
    // (name, had an old entry) for each entry moved in so far
    let mut done: Vec<(std::ffi::OsString, bool)> = Vec::new();
    let mut failure = None;
    for name in names {
        let (old, new) = (root.join(&name), new_root.join(&name));
        let had_old = fs::symlink_metadata(&old).is_ok();
        if had_old && let Err(e) = fs::rename(&old, backup.join(&name)) {
            failure = Some(format!("cannot move {} aside: {e}", old.display()));
            break;
        }
        if let Err(e) = fs::rename(&new, &old) {
            if had_old {
                let _ = fs::rename(backup.join(&name), &old);
            }
            failure = Some(format!("cannot move the new {} in place: {e}", name.to_string_lossy()));
            break;
        }
        done.push((name, had_old));
    }
    if let Some(e) = failure {
        for (name, had_old) in done.into_iter().rev() {
            let _ = fs::remove_dir_all(root.join(&name)).or_else(|_| fs::remove_file(root.join(&name)));
            if had_old {
                let _ = fs::rename(backup.join(&name), root.join(&name));
            }
        }
        let _ = fs::remove_dir_all(&backup);
        return Err(format!("{e}; the previous version was kept"));
    }
    // the running program's files stay readable through their open handles until it exits
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_dir(dir: &Path, version: &str) {
        fs::create_dir_all(dir.join("docs")).unwrap();
        for f in ["citro", "citro-install", "citro-hub", "CHANGELOG.md"] {
            fs::write(dir.join(f), version).unwrap();
        }
        fs::write(dir.join("docs/GUI.md"), version).unwrap();
        fs::create_dir_all(dir.join("Citroenition.app/Contents/MacOS")).unwrap();
        fs::write(dir.join("Citroenition.app/Contents/MacOS/citro-gui"), version).unwrap();
    }

    #[test]
    fn the_install_is_found_from_the_program_inside_the_app() {
        let t = tempfile::tempdir().unwrap();
        release_dir(t.path(), "old");
        let exe = t.path().join("Citroenition.app/Contents/MacOS/citro-gui");
        let i = Install::from_exe(&exe).unwrap();
        assert_eq!(i.root, t.path());
        assert_eq!(i.app_bundle.as_deref(), Some(t.path().join("Citroenition.app").as_path()));
        // a development build is not a release
        assert!(Install::from_exe(&t.path().join("target/debug/citro-gui")).is_err());
    }

    #[test]
    fn swapping_replaces_every_entry_and_keeps_unrelated_files() {
        let t = tempfile::tempdir().unwrap();
        let (root, new) = (t.path().join("install"), t.path().join("new"));
        release_dir(&root, "old");
        release_dir(&new, "new");
        fs::write(root.join("my-notes.txt"), "mine").unwrap();
        swap(&root, &new).unwrap();
        assert_eq!(fs::read_to_string(root.join("citro")).unwrap(), "new");
        assert_eq!(fs::read_to_string(root.join("Citroenition.app/Contents/MacOS/citro-gui")).unwrap(), "new");
        assert_eq!(fs::read_to_string(root.join("docs/GUI.md")).unwrap(), "new");
        assert_eq!(fs::read_to_string(root.join("my-notes.txt")).unwrap(), "mine");
        assert!(!root.join(BACKUP).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_puts_the_old_version_back() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let (root, new) = (t.path().join("install"), t.path().join("new"));
        release_dir(&root, "old");
        release_dir(&new, "new");
        // `docs` is swapped before `zzz`, which cannot be moved in: its source folder is not writable
        fs::create_dir_all(new.join("zzz-locked/inner")).unwrap();
        let locked = new.join("zzz-locked");
        fs::set_permissions(&new, fs::Permissions::from_mode(0o555)).unwrap();
        let r = swap(&root, &new);
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).unwrap();
        if r.is_ok() {
            // running as root: permissions do not stop the rename, nothing to check
            assert!(root.join("zzz-locked").exists() || locked.exists());
            return;
        }
        assert_eq!(fs::read_to_string(root.join("citro")).unwrap(), "old");
        assert_eq!(fs::read_to_string(root.join("docs/GUI.md")).unwrap(), "old");
        assert!(!root.join(BACKUP).exists());
    }
}

/// The whole update against the real GitHub release, on a copy of an unpacked older release:
/// `CITRO_UPDATE_E2E=/path/to/copy-of-citroenition-vX cargo test -p citro-gui real_update -- --ignored`.
#[cfg(test)]
mod e2e {
    use super::*;

    #[test]
    #[ignore = "downloads the newest release from GitHub and rewrites the folder in CITRO_UPDATE_E2E"]
    fn real_update_replaces_an_older_release() {
        let dir = PathBuf::from(std::env::var("CITRO_UPDATE_E2E").expect("set CITRO_UPDATE_E2E"));
        let exe = if cfg!(target_os = "macos") {
            dir.join("Citroenition.app/Contents/MacOS/citro-gui")
        } else {
            dir.join("citro-gui")
        };
        let install = Install::from_exe(&exe).unwrap();
        let release = citro_install::app_update::latest_app_release(LOOKUP_TIMEOUT).unwrap();
        let mut last = (0, 0);
        install.update(&release, &mut |d, t| last = (d, t)).unwrap();
        assert!(last.0 > 1_000_000 && (last.1 == 0 || last.0 == last.1), "progress {last:?}");
        let out = std::process::Command::new(dir.join("citro")).arg("--version").output().unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(printed.contains(&release.version), "citro --version printed {printed}");
        assert!(!dir.join(STAGING).exists() && !dir.join(BACKUP).exists());
    }
}
