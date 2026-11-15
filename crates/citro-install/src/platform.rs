//! Which prebuilt Clearcote archive runs on which machine.
//!
//! Releases carry `clearcote-<version>-linux-x64.tar.xz` and `clearcote-<version>-windows-x64.zip`: nothing for
//! macOS and nothing for arm64.

use crate::error::{Error, Result};
use crate::version::is_valid_version;
use serde::Serialize;
use std::fmt;

/// Every asset starts with this.
pub const ASSET_PREFIX: &str = "clearcote-";

/// How an archive is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `.tar.xz`, entries directly at the archive root.
    TarXz,
    /// `.zip`
    Zip,
}

/// A platform Clearcote publishes a kernel for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Platform {
    /// Linux on x86-64.
    #[serde(rename = "linux-x64")]
    LinuxX64,
    /// Windows on x86-64.
    #[serde(rename = "windows-x64")]
    WindowsX64,
}

impl Platform {
    /// Every supported platform.
    pub const ALL: [Platform; 2] = [Platform::LinuxX64, Platform::WindowsX64];

    /// The platform this program runs on, or [`Error::UnsupportedPlatform`] (macOS, arm64, ...).
    pub fn detect() -> Result<Platform> {
        Self::from_os_arch(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Map `std::env::consts::{OS, ARCH}` values to a platform.
    pub fn from_os_arch(os: &str, arch: &str) -> Result<Platform> {
        match (os, arch) {
            ("linux", "x86_64") => Ok(Platform::LinuxX64),
            ("windows", "x86_64") => Ok(Platform::WindowsX64),
            _ => Err(Error::UnsupportedPlatform { os: os.to_owned(), arch: arch.to_owned() }),
        }
    }

    /// `linux-x64` / `windows-x64`
    pub fn name(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "linux-x64",
            Platform::WindowsX64 => "windows-x64",
        }
    }

    /// Parse [`Platform::name`] back.
    pub fn parse(s: &str) -> Option<Platform> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The end of every asset name for this platform: `linux-x64.tar.xz` / `windows-x64.zip`.
    pub fn asset_suffix(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "linux-x64.tar.xz",
            Platform::WindowsX64 => "windows-x64.zip",
        }
    }

    /// How the archive is packed.
    pub fn archive_kind(self) -> ArchiveKind {
        match self {
            Platform::LinuxX64 => ArchiveKind::TarXz,
            Platform::WindowsX64 => ArchiveKind::Zip,
        }
    }

    /// The browser executable at the top level of the unpacked archive.
    pub fn binary_name(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "chrome",
            Platform::WindowsX64 => "chrome.exe",
        }
    }

    /// `clearcote-<version>-<suffix>`
    pub fn asset_name(self, version: &str) -> String {
        format!("{ASSET_PREFIX}{version}-{}", self.asset_suffix())
    }

    /// The version out of an asset name, if `name` is exactly `clearcote-<version>-<suffix>` for this platform.
    pub fn version_of_asset(self, name: &str) -> Option<String> {
        let v = name.strip_prefix(ASSET_PREFIX)?.strip_suffix(self.asset_suffix())?.strip_suffix('-')?;
        is_valid_version(v).then(|| v.to_owned())
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The binary name of this machine's platform (`chrome`, or `chrome.exe` on Windows).
pub(crate) fn host_binary_name() -> &'static str {
    if cfg!(windows) { Platform::WindowsX64.binary_name() } else { Platform::LinuxX64.binary_name() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_two_published_platforms_and_nothing_else() {
        assert_eq!(Platform::from_os_arch("linux", "x86_64").unwrap(), Platform::LinuxX64);
        assert_eq!(Platform::from_os_arch("windows", "x86_64").unwrap(), Platform::WindowsX64);
        for (os, arch) in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("windows", "aarch64")] {
            let e = Platform::from_os_arch(os, arch).unwrap_err();
            assert!(matches!(e, Error::UnsupportedPlatform { .. }));
            let msg = e.to_string();
            assert!(msg.contains(os) && msg.contains(arch) && msg.contains("CITRO_KERNEL"), "{msg}");
        }
    }

    #[test]
    fn asset_names_round_trip() {
        let p = Platform::LinuxX64;
        assert_eq!(p.asset_name("151.0.7922.108"), "clearcote-151.0.7922.108-linux-x64.tar.xz");
        assert_eq!(p.version_of_asset("clearcote-151.0.7922.108-linux-x64.tar.xz").as_deref(), Some("151.0.7922.108"));
        assert_eq!(
            Platform::WindowsX64.version_of_asset("clearcote-151.0.7922.108-windows-x64.zip").as_deref(),
            Some("151.0.7922.108")
        );
        for name in [
            "clearcote-151.0.7922.108-linux-x64.tar.xz.sha256",
            "clearcote-151.0.7922.108-windows-x64.zip",
            "clearcote--linux-x64.tar.xz",
            "clearcote-../x-linux-x64.tar.xz",
            "clearcote-1.2.3-linux-x64.tar.xz/../../etc",
            "chrome",
            "SHA256SUMS.txt",
            "clearcote-1.2.3linux-x64.tar.xz",
        ] {
            assert_eq!(p.version_of_asset(name), None, "{name}");
        }
        assert_eq!(Platform::parse("windows-x64"), Some(Platform::WindowsX64));
        assert_eq!(Platform::parse("macos-arm64"), None);
    }
}
