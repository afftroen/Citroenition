//! File-system helpers: atomic writes, permissions, advisory locks, change stamps.

use super::state::State;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Create a directory (and parents) readable by the owner only.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// `OpenOptions` for a new file that only the owner may read.
pub fn private_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Flush a directory's entries (new/renamed files) to disk.
pub fn sync_dir(dir: &Path) {
    // Best effort: fsync of a directory is not possible everywhere.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Write `bytes` to `path` atomically: temp file in the same directory, fsync, rename, fsync of the directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = private_options().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    sync_dir(dir);
    Ok(())
}

/// What `state.json` looked like when we last read or wrote it; a different stamp means somebody else changed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    ino: u64,
    size: u64,
    mtime_s: i64,
    mtime_ns: i64,
}

pub fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(Stamp { ino: m.ino(), size: m.len(), mtime_s: m.mtime(), mtime_ns: m.mtime_nsec() })
    }
    #[cfg(not(unix))]
    {
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Stamp { ino: 0, size: m.len(), mtime_s: t.as_secs() as i64, mtime_ns: i64::from(t.subsec_nanos()) })
    }
}

/// An advisory lock on a file in the data directory, released on drop.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Block until the exclusive lock is ours.
    pub fn exclusive(path: &Path) -> io::Result<FileLock> {
        let file = private_options().read(true).write(true).create(true).truncate(false).open(path)?;
        file.lock()?;
        Ok(FileLock { file })
    }

    /// Take the lock or fail right away with `WouldBlock`.
    pub fn try_exclusive(path: &Path) -> io::Result<FileLock> {
        let file = private_options().read(true).write(true).create(true).truncate(false).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(FileLock { file }),
            Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(io::ErrorKind::WouldBlock, "lock is held")),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub fn state_lock_path(dir: &Path) -> PathBuf {
    dir.join("state.lock")
}

pub fn serve_lock_path(dir: &Path) -> PathBuf {
    dir.join("serve.lock")
}

pub fn read_state(dir: &Path) -> io::Result<State> {
    let bytes = fs::read(state_path(dir))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("{} is corrupt: {e}", state_path(dir).display()))
    })
}

pub fn write_state(dir: &Path, state: &State) -> io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(|e| io::Error::other(e.to_string()))?;
    write_atomic(&state_path(dir), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citro-hub-fsio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_files() {
        let d = tmp("atomic");
        let p = d.join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stamp_changes_when_the_file_is_replaced() {
        let d = tmp("stamp");
        let p = d.join("s.json");
        assert!(stamp_of(&p).is_none());
        write_atomic(&p, b"a").unwrap();
        let s1 = stamp_of(&p).unwrap();
        assert_eq!(stamp_of(&p), Some(s1));
        write_atomic(&p, b"bb").unwrap();
        assert_ne!(stamp_of(&p).unwrap(), s1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn locks_exclude_each_other_until_dropped() {
        let d = tmp("lock");
        let p = d.join("l.lock");
        let held = FileLock::try_exclusive(&p).unwrap();
        assert_eq!(FileLock::try_exclusive(&p).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop(held);
        assert!(FileLock::try_exclusive(&p).is_ok());
        let _ = fs::remove_dir_all(&d);
    }
}
