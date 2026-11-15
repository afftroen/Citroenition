//! Installing / updating the Clearcote kernel.
//!
//! The GUI talks to a [`KernelInstaller`] trait object. Right now that is a clearly labelled stub: the real
//! installer lives in the separate `citro-install` crate, which is swapped in later without touching any UI code
//! (implement the trait, pass the object to `LocalBackend::with_installer`). Meanwhile the Kernel page tells the
//! user to run `citro kernel install`.

use crate::backend::{BResult, BackendError, KernelInfo};
use std::path::Path;

/// What the Kernel page shows while the installer is a stub.
pub const STUB_HINT: &str = "citro kernel install";

pub trait KernelInstaller: Send + Sync {
    /// Short name for the UI (`citro-install`, `stub`).
    fn name(&self) -> &str;

    /// `true` for the placeholder: the UI labels the button accordingly.
    fn is_stub(&self) -> bool {
        false
    }

    /// The newest kernel version available upstream, `None` = unknown (offline / not checked).
    fn latest_version(&self) -> BResult<Option<String>>;

    /// Download, verify and unpack the newest kernel into `<data_dir>/kernels/clearcote-<version>/`.
    /// `progress(done_bytes, total_bytes)` is called from the worker thread; `total` is `0` while unknown.
    fn install(&self, data_dir: &Path, progress: &dyn Fn(u64, u64)) -> BResult<KernelInfo>;
}

/// Placeholder until `citro-install` is wired in. It never touches the network or the disk.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubInstaller;

impl KernelInstaller for StubInstaller {
    fn name(&self) -> &str {
        "stub (citro-install is not wired in yet)"
    }

    fn is_stub(&self) -> bool {
        true
    }

    fn latest_version(&self) -> BResult<Option<String>> {
        Ok(None)
    }

    fn install(&self, _data_dir: &Path, _progress: &dyn Fn(u64, u64)) -> BResult<KernelInfo> {
        Err(BackendError::Unsupported(format!(
            "the in-app installer is a stub for now - run `{STUB_HINT}` in a terminal, then press Refresh"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_says_what_to_run_and_touches_nothing() {
        let s = StubInstaller;
        assert!(s.is_stub());
        assert_eq!(s.latest_version().unwrap(), None);
        let e = s.install(Path::new("/nonexistent/never-created"), &|_, _| panic!("no progress from a stub"));
        let msg = e.unwrap_err().to_string();
        assert!(msg.contains("citro kernel install"), "{msg}");
        assert!(!Path::new("/nonexistent/never-created").exists());
    }
}
