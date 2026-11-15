//! The hosted data of a profile (cookies + session archive) on disk.
//!
//! Layout: `<data>/blobs/<profile_id>/v<version>.bin`, one file per kept version; the highest version is the
//! current one. An upload is streamed into `upload-<random>` first and renamed to its final name only after every
//! check passed, so a reader never sees a half-written file. Older versions are pruned to the newest `keep`.
//!
//! Crash safety (see [`reconcile`]): a file newer than the recorded version was never acknowledged to a client and
//! is dropped; a missing current file falls back to the newest older one; leftover `upload-*` files are removed.

use super::fsio::sync_dir;
use super::state::State;
use crate::model::DataMeta;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// How many versions of one profile are kept when the config does not say.
pub const DEFAULT_KEEP: usize = 5;

/// `v0000000007.bin`: zero padded so that a directory listing sorts numerically.
pub fn file_name(version: u64) -> String {
    format!("v{version:010}.bin")
}

pub fn file_path(dir: &Path, version: u64) -> PathBuf {
    dir.join(file_name(version))
}

fn version_of(name: &str) -> Option<u64> {
    let digits = name.strip_prefix('v')?.strip_suffix(".bin")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok())?
}

/// A version file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobFile {
    pub version: u64,
    pub size: u64,
    /// Modification time, unix seconds.
    pub modified: i64,
}

/// The version files of a profile, oldest first. Anything that is not a version file is ignored.
pub fn list(dir: &Path) -> Vec<BlobFile> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<BlobFile> = entries
        .flatten()
        .filter_map(|e| {
            let version = version_of(&e.file_name().to_string_lossy())?;
            let md = e.metadata().ok().filter(|m| m.is_file())?;
            let modified = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            Some(BlobFile { version, size: md.len(), modified })
        })
        .collect();
    out.sort_by_key(|f| f.version);
    out
}

/// Put a finished upload in place as `version`. The rename is atomic; the directory is synced afterwards.
pub fn install(dir: &Path, tmp: &Path, version: u64) -> io::Result<()> {
    let target = file_path(dir, version);
    if target.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "this version already exists on disk"));
    }
    fs::rename(tmp, &target)?;
    sync_dir(dir);
    Ok(())
}

/// Delete everything but the newest `keep` versions (at least one is always kept). Returns how many went.
pub fn prune(dir: &Path, keep: usize) -> usize {
    let files = list(dir);
    let keep = keep.max(1);
    if files.len() <= keep {
        return 0;
    }
    files[..files.len() - keep].iter().filter(|f| fs::remove_file(file_path(dir, f.version)).is_ok()).count()
}

/// Size and lower-case hex SHA-256 of a file.
pub fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut f = fs::File::open(path)?;
    let (mut h, mut n, mut buf) = (Sha256::new(), 0u64, vec![0u8; 256 * 1024]);
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((n, super::util::hex(&h.finalize())))
}

/// Repair what a crash can leave behind and make the recorded metadata describe the files. Returns whether the
/// state changed. Runs at start-up, before the first request.
pub fn reconcile(data_dir: &Path, st: &mut State, now: i64, keep: usize) -> bool {
    let mut changed = false;
    for (pid, hp) in st.profiles.iter_mut() {
        let bd = data_dir.join("blobs").join(pid);
        let Ok(entries) = fs::read_dir(&bd) else {
            if hp.data.version > 0 {
                tracing::warn!("data of profile {pid} is missing on disk; resetting its metadata");
                hp.data = DataMeta::default();
                changed = true;
            }
            continue;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("upload-") {
                let _ = fs::remove_file(e.path());
            }
        }
        // Never acknowledged to a client (the state was not written yet): drop it.
        for f in list(&bd).into_iter().filter(|f| f.version > hp.data.version) {
            tracing::warn!("dropping the unacknowledged upload v{} of profile {pid}", f.version);
            let _ = fs::remove_file(file_path(&bd, f.version));
        }
        let cur = hp.data.version;
        match list(&bd).last().cloned() {
            None if cur > 0 => {
                tracing::warn!("data of profile {pid} is missing on disk; resetting its metadata");
                hp.data = DataMeta::default();
                changed = true;
            }
            None => {}
            Some(f) if f.version == cur => {
                if f.size != hp.data.size {
                    // The file does not match what we recorded (edited by hand, or a torn disk): re-read it and
                    // publish it as a new version so that no client mistakes it for the one it has.
                    match hash_file(&file_path(&bd, cur)) {
                        Ok((size, sha256)) if fs::rename(file_path(&bd, cur), file_path(&bd, cur + 1)).is_ok() => {
                            tracing::warn!("data of profile {pid} did not match its metadata; now v{}", cur + 1);
                            hp.data = DataMeta { version: cur + 1, size, sha256, updated_at: now };
                            changed = true;
                        }
                        Ok(_) => tracing::error!("cannot rename the data of profile {pid}"),
                        Err(e) => tracing::error!("cannot read the data of profile {pid}: {e}"),
                    }
                }
            }
            Some(f) => {
                // The current file is gone; the newest older one is the best we have.
                match hash_file(&file_path(&bd, f.version)) {
                    Ok((size, sha256)) => {
                        tracing::warn!("data v{cur} of profile {pid} is missing; falling back to v{}", f.version);
                        hp.data = DataMeta { version: f.version, size, sha256, updated_at: now };
                        changed = true;
                    }
                    Err(e) => tracing::error!("cannot read the data of profile {pid}: {e}"),
                }
            }
        }
        prune(&bd, keep);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HubProfile;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citro-hub-blobs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, version: u64, content: &str) {
        let t = dir.join(format!("upload-{version}"));
        fs::write(&t, content).unwrap();
        install(dir, &t, version).unwrap();
    }

    #[test]
    fn names_round_trip_and_foreign_files_are_ignored() {
        assert_eq!(file_name(7), "v0000000007.bin");
        assert_eq!(version_of("v0000000007.bin"), Some(7));
        for bad in ["data.bin", "v.bin", "v12", "v-1.bin", "upload-1", "vx1.bin", "v1.bin.tmp"] {
            assert_eq!(version_of(bad), None, "{bad}");
        }
        let d = tmp("names");
        put(&d, 2, "b");
        fs::write(d.join("notes.txt"), "x").unwrap();
        fs::write(d.join("upload-zzz"), "x").unwrap();
        assert_eq!(list(&d).iter().map(|f| f.version).collect::<Vec<_>>(), [2]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_never_overwrites_and_prune_keeps_the_newest() {
        let d = tmp("prune");
        for v in 1..=5 {
            put(&d, v, &format!("content {v}"));
        }
        let t = d.join("upload-dup");
        fs::write(&t, "dup").unwrap();
        assert!(install(&d, &t, 3).is_err(), "an existing version is never replaced");
        assert_eq!(fs::read_to_string(file_path(&d, 3)).unwrap(), "content 3");
        assert_eq!(prune(&d, 2), 3);
        assert_eq!(list(&d).iter().map(|f| f.version).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(prune(&d, 0), 1, "at least one version always stays");
        assert_eq!(list(&d).iter().map(|f| f.version).collect::<Vec<_>>(), [5]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reconcile_repairs_crash_leftovers() {
        let root = tmp("reconcile");
        let mut st = State::default();
        let mut hp = HubProfile::default();
        hp.profile.profile_id = "kabc123".into();
        st.profiles.insert("kabc123".into(), hp);
        let bd = root.join("blobs").join("kabc123");
        fs::create_dir_all(&bd).unwrap();

        // nothing on disk, nothing recorded: nothing to do
        assert!(!reconcile(&root, &mut st, 100, 3));

        // v1 recorded and present, v2 was written but never acknowledged, an upload was cut off
        put(&bd, 1, "one");
        let (size, sha) = hash_file(&file_path(&bd, 1)).unwrap();
        st.profiles.get_mut("kabc123").unwrap().data =
            DataMeta { version: 1, size, sha256: sha.clone(), updated_at: 5 };
        put(&bd, 2, "two");
        fs::write(bd.join("upload-half"), "ha").unwrap();
        assert!(!reconcile(&root, &mut st, 100, 3));
        assert_eq!(list(&bd).iter().map(|f| f.version).collect::<Vec<_>>(), [1], "the unacknowledged v2 is dropped");
        assert!(!bd.join("upload-half").exists());
        assert_eq!(st.profiles["kabc123"].data.sha256, sha);

        // the current file vanished: fall back to the newest older version
        put(&bd, 2, "two");
        let (size2, sha2) = hash_file(&file_path(&bd, 2)).unwrap();
        st.profiles.get_mut("kabc123").unwrap().data =
            DataMeta { version: 3, size: 9, sha256: "x".into(), updated_at: 5 };
        assert!(reconcile(&root, &mut st, 100, 3));
        let d = &st.profiles["kabc123"].data;
        assert_eq!((d.version, d.size, d.sha256.as_str(), d.updated_at), (2, size2, sha2.as_str(), 100));

        // the file size disagrees with the record: re-read and publish as the next version
        st.profiles.get_mut("kabc123").unwrap().data.size = 1;
        assert!(reconcile(&root, &mut st, 200, 3));
        assert_eq!(st.profiles["kabc123"].data.version, 3);
        assert_eq!(fs::read_to_string(file_path(&bd, 3)).unwrap(), "two");
        assert!(!file_path(&bd, 2).exists());

        // everything gone: the record is reset
        fs::remove_dir_all(&bd).unwrap();
        assert!(reconcile(&root, &mut st, 300, 3));
        assert_eq!(st.profiles["kabc123"].data, DataMeta::default());
        let _ = fs::remove_dir_all(&root);
    }
}
