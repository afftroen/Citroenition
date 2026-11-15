//! Citroenition core: an AdsPower-compatible profile model, fingerprint / kernel
//! config generation and a launcher for the SunBrowser (Chromium) kernel.
//!
//! The wire formats in here were reverse engineered from a local AdsPower
//! install; see `docs/REVERSE_ENGINEERING.md` in the repository root.

pub mod codec;
pub mod error;
pub mod fingerprint;
pub mod kernel;
pub mod launch;
pub mod profile;
pub mod rng;
pub mod store;

pub use error::{Error, Result};
