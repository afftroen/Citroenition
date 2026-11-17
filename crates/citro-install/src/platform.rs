//! Which prebuilt Clearcote archive runs on which machine.
//!
//! Releases carry `clearcote-<version>-linux-x64.tar.xz` and `clearcote-<version>-windows-x64.zip`, and some also
//! `clearcote-<version>-macos-arm64.zip` (a `Chromium.app` bundle). There is nothing for Intel Macs and nothing for
//! arm64 Linux or Windows.

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
    /// `.zip` made on Windows: `\` separates names too, and there are no links.
    Zip,
    /// `.zip` made on Unix (the macOS app bundle): `/` names, symbolic links inside the tree, execute bits.
    ZipUnix,
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
    /// macOS on Apple Silicon.
    #[serde(rename = "macos-arm64")]
    MacosArm64,
}

impl Platform {
    /// Every supported platform.
    pub const ALL: [Platform; 3] = [Platform::LinuxX64, Platform::WindowsX64, Platform::MacosArm64];

    /// The platform this program runs on, or [`Error::UnsupportedPlatform`] (Intel Macs, arm64 Linux, ...).
    pub fn detect() -> Result<Platform> {
        Self::from_os_arch(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Map `std::env::consts::{OS, ARCH}` values to a platform.
    pub fn from_os_arch(os: &str, arch: &str) -> Result<Platform> {
        match (os, arch) {
            ("linux", "x86_64") => Ok(Platform::LinuxX64),
            ("windows", "x86_64") => Ok(Platform::WindowsX64),
            ("macos", "aarch64") => Ok(Platform::MacosArm64),
            _ => Err(Error::UnsupportedPlatform { os: os.to_owned(), arch: arch.to_owned() }),
        }
    }

    /// `linux-x64` / `windows-x64` / `macos-arm64`
    pub fn name(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "linux-x64",
            Platform::WindowsX64 => "windows-x64",
            Platform::MacosArm64 => "macos-arm64",
        }
    }

    /// Parse [`Platform::name`] back.
    pub fn parse(s: &str) -> Option<Platform> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The end of every asset name for this platform: `linux-x64.tar.xz` / `windows-x64.zip` / `macos-arm64.zip`.
    pub fn asset_suffix(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "linux-x64.tar.xz",
            Platform::WindowsX64 => "windows-x64.zip",
            Platform::MacosArm64 => "macos-arm64.zip",
        }
    }

    /// How the archive is packed.
    pub fn archive_kind(self) -> ArchiveKind {
        match self {
            Platform::LinuxX64 => ArchiveKind::TarXz,
            Platform::WindowsX64 => ArchiveKind::Zip,
            Platform::MacosArm64 => ArchiveKind::ZipUnix,
        }
    }

    /// The browser executable, relative to the unpacked archive: at the top level on Linux and Windows, inside the
    /// app bundle on macOS.
    pub fn binary_path(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "chrome",
            Platform::WindowsX64 => "chrome.exe",
            Platform::MacosArm64 => "Chromium.app/Contents/MacOS/Chromium",
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

/// Where the browser executable of this machine's platform lives in an install (`chrome`, `chrome.exe`, or the path
/// inside `Chromium.app` on macOS).
pub(crate) fn host_binary_path() -> &'static str {
    if cfg!(windows) {
        Platform::WindowsX64.binary_path()
    } else if cfg!(target_os = "macos") {
        Platform::MacosArm64.binary_path()
    } else {
        Platform::LinuxX64.binary_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_three_published_platforms_and_nothing_else() {
        assert_eq!(Platform::from_os_arch("linux", "x86_64").unwrap(), Platform::LinuxX64);
        assert_eq!(Platform::from_os_arch("windows", "x86_64").unwrap(), Platform::WindowsX64);
        assert_eq!(Platform::from_os_arch("macos", "aarch64").unwrap(), Platform::MacosArm64);
        for (os, arch) in [("macos", "x86_64"), ("linux", "aarch64"), ("windows", "aarch64")] {
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
        let mac = Platform::MacosArm64;
        assert_eq!(mac.asset_name("150.0.7871.114"), "clearcote-150.0.7871.114-macos-arm64.zip");
        assert_eq!(mac.version_of_asset("clearcote-150.0.7871.114-macos-arm64.zip").as_deref(), Some("150.0.7871.114"));
        assert_eq!(mac.version_of_asset("clearcote-150.0.7871.114-windows-x64.zip"), None);
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
        for name in [
            "clearcote-1.2.3-macos-arm64.zip.sha256",
            "clearcote-1.2.3-macos-x64.zip",
            "clearcote-1.2.3macos-arm64.zip",
        ] {
            assert_eq!(mac.version_of_asset(name), None, "{name}");
        }
        assert_eq!(Platform::parse("windows-x64"), Some(Platform::WindowsX64));
        assert_eq!(Platform::parse("macos-arm64"), Some(Platform::MacosArm64));
        assert_eq!(Platform::parse("macos-x64"), None);
    }

    #[test]
    fn the_browser_lives_where_each_platform_puts_it() {
        assert_eq!(Platform::LinuxX64.binary_path(), "chrome");
        assert_eq!(Platform::WindowsX64.binary_path(), "chrome.exe");
        assert_eq!(Platform::MacosArm64.binary_path(), "Chromium.app/Contents/MacOS/Chromium");
        assert_eq!(Platform::MacosArm64.archive_kind(), ArchiveKind::ZipUnix);
        assert_eq!(Platform::WindowsX64.archive_kind(), ArchiveKind::Zip);
    }
}
