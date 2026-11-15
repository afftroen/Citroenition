//! Downloads, verifies and unpacks the open-source Clearcote Chromium kernel for Citroenition.
//!
//! Trust model: a kernel is installed only if (1) the detached OpenPGP signature of the release's `SHA256SUMS.txt`
//! verifies against the release-signing key **compiled into this crate**, and (2) the SHA-256 of the downloaded
//! archive equals its line in that signed list. Nothing else (TLS, GitHub, a key shipped next to the release) is
//! trusted. Both checks are hard failures.
//!
//! ```no_run
//! use citro_install::{InstallOptions, Progress, install, list_installed};
//! # fn main() -> citro_install::Result<()> {
//! let root = std::path::Path::new("/tmp/kernels");
//! let kernel = install(root, &InstallOptions::default(), &mut |p| {
//!     if let Progress::Downloading { done, total } = p {
//!         eprintln!("{done}/{total}");
//!     }
//! })?;
//! println!("{} -> {}", kernel.version, kernel.binary.display());
//! assert_eq!(list_installed(root)[0].version, kernel.version);
//! # Ok(())
//! # }
//! ```

mod error;
mod extract;
mod http;
mod install;
mod platform;
mod progress;
mod release;
mod verify;
mod version;

pub use error::{Error, Result};
pub use install::{DIR_PREFIX, InstallOptions, Installed, METADATA_FILE, Progress, install, list_installed};
pub use platform::{ArchiveKind, Platform};
pub use progress::ProgressLine;
pub use release::{API_ROOT, Asset, REPO, Release, latest_release, release_by_tag};
pub use verify::{
    ChecksumEntry, ChecksumList, ListedArchive, PINNED_KEY_ARMORED, pinned_key_fingerprint, verify_checksums,
};
pub use version::{compare_versions, is_valid_version};
