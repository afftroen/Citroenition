//! Installing / updating the Clearcote kernel.
//!
//! The GUI talks to a [`KernelInstaller`] trait object. [`CitroInstaller`] is the real one: it is `citro-install`
//! (a pinned OpenPGP key checks the signed checksum list, the archive's SHA-256 must match it, the unpack is
//! path-safe), the same code behind `citro kernel install`. [`StubInstaller`] never touches the network or the disk
//! and is what `LocalBackend::new` starts with, so tests and the demo backend stay offline;
//! `LocalBackend::with_installer` swaps the real one in.

use crate::backend::{BResult, BackendError, KernelInfo};
use citro_core::kernel::kernels_dir;
use citro_core::store::Home;
use citro_install::{InstallOptions, Progress};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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

/// How long "the newest kernel is X" is believed. The Kernel page asks on every refresh: GitHub is asked at most
/// this often.
const LATEST_FOR: Duration = Duration::from_secs(10 * 60);
/// A failed look-up (offline) is retried sooner, so "Check again" works when the network is back.
const LATEST_RETRY: Duration = Duration::from_secs(20);
/// Looking up the newest version must not hold the page up for long on a bad connection.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(6);

/// The real installer: downloads, verifies (pinned key + SHA-256) and unpacks the newest Clearcote kernel.
#[derive(Debug, Default)]
pub struct CitroInstaller {
    latest: Mutex<Option<(Instant, Option<String>)>>,
}

fn fail(e: citro_install::Error) -> BackendError {
    match e {
        citro_install::Error::UnsupportedPlatform { .. } => BackendError::Unsupported(e.to_string()),
        other => BackendError::Other(other.to_string()),
    }
}

impl KernelInstaller for CitroInstaller {
    fn name(&self) -> &str {
        "citro-install"
    }

    fn latest_version(&self) -> BResult<Option<String>> {
        {
            let cached = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, version)) = cached.as_ref() {
                let fresh = if version.is_some() { LATEST_FOR } else { LATEST_RETRY };
                if at.elapsed() < fresh {
                    return Ok(version.clone());
                }
            }
        }
        // offline, rate-limited, no build for this OS: "unknown" is an answer, not an error the page must show
        let probe = InstallOptions { timeout: LOOKUP_TIMEOUT, ..InstallOptions::default() };
        let found = citro_install::latest_release(&probe).ok().map(|r| r.version);
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), found.clone()));
        Ok(found)
    }

    fn install(&self, data_dir: &Path, progress: &dyn Fn(u64, u64)) -> BResult<KernelInfo> {
        let root = kernels_dir(&Home { root: data_dir.to_path_buf() });
        // the page shows a bar for the download; the look-up before it is "starting" (0 of unknown), and what comes
        // after it (verify, unpack) is the full bar
        let mut size = 0u64;
        let got = citro_install::install(&root, &InstallOptions::default(), &mut |p| match p {
            Progress::Resolving => progress(0, 0),
            Progress::Downloading { done, total } => {
                size = total.max(done);
                progress(done, total);
            }
            Progress::Verifying | Progress::Extracting => progress(size, size),
            Progress::Done => {}
        })
        .map_err(fail)?;
        // what is newest is known now: do not make the page ask again
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), Some(got.version.clone())));
        Ok(KernelInfo { version: got.version, path: got.binary.display().to_string() })
    }
}

/// Placeholder for tests and the demo: it never touches the network or the disk.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubInstaller;

impl KernelInstaller for StubInstaller {
    fn name(&self) -> &str {
        "stub (offline: the in-app installer is off)"
    }

    fn is_stub(&self) -> bool {
        true
    }

    fn latest_version(&self) -> BResult<Option<String>> {
        Ok(None)
    }

    fn install(&self, _data_dir: &Path, _progress: &dyn Fn(u64, u64)) -> BResult<KernelInfo> {
        Err(BackendError::Unsupported(format!(
            "the in-app installer is off in this mode - run `{STUB_HINT}` in a terminal, then press Refresh"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_installer_is_not_a_stub_and_remembers_what_it_asked() {
        let i = CitroInstaller::default();
        assert!(!i.is_stub());
        assert_eq!(i.name(), "citro-install");
        // a recent answer is believed: no second round trip to GitHub (this would otherwise hit the network)
        *i.latest.lock().unwrap() = Some((Instant::now(), Some("151.0.7922.108".into())));
        assert_eq!(i.latest_version().unwrap().as_deref(), Some("151.0.7922.108"));
    }

    #[test]
    fn a_host_without_a_prebuilt_kernel_is_told_what_to_do_instead() {
        let e = fail(citro_install::Error::UnsupportedPlatform { os: "freebsd".into(), arch: "x86_64".into() });
        assert!(matches!(&e, BackendError::Unsupported(m) if m.contains("CITRO_KERNEL")), "{e:?}");
        let e = fail(citro_install::Error::Network { url: "https://example.invalid/x".into(), message: "down".into() });
        assert!(matches!(e, BackendError::Other(_)));
    }

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
