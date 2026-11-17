//! Citroenition core: an AdsPower-compatible profile model, the persona (fingerprint) model and a launcher
//! for the open-source Clearcote (Chromium) kernel.
//!
//! The profile and API formats in here were reverse engineered from a local AdsPower install; see
//! `docs/REVERSE_ENGINEERING.md` in the repository root. The browser itself is Clearcote, see `docs/KERNEL.md`.

pub mod adspower;
pub mod bookmarks;
pub mod browser_prefs;
pub mod cdp;
pub mod codec;
pub mod cookies;
pub mod csv;
pub mod error;
pub mod extensions;
pub mod fingerprint;
pub mod import;
pub mod kernel;
pub mod kernel_env;
pub mod launch;
pub mod locale;
pub mod profile;
pub mod redact;
pub mod relay;
pub mod rng;
pub mod store;

pub use error::{Error, Result};
