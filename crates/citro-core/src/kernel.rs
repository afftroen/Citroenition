//! Locating a SunBrowser kernel binary.
//!
//! Citroenition does not ship a browser. It drives the SunBrowser (Chromium) kernel
//! that an AdsPower install already downloaded to the machine.

use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Used when the kernel directory name carries no version (`chrome_157` style).
pub const DEFAULT_MAJOR: u32 = 157;

#[derive(Debug, Clone)]
pub struct Kernel {
    pub binary: PathBuf,
    pub major: u32,
}

fn parse_major(dir_name: &str) -> Option<u32> {
    dir_name.strip_prefix("chrome_")?.parse().ok()
}

impl Kernel {
    pub fn from_path(binary: impl Into<PathBuf>) -> Result<Self> {
        let binary = binary.into();
        if !binary.is_file() {
            return Err(Error::KernelNotFound(binary.display().to_string()));
        }
        let major = binary
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .and_then(parse_major)
            .unwrap_or(DEFAULT_MAJOR);
        Ok(Self { binary, major })
    }

    pub fn dir(&self) -> &Path {
        self.binary.parent().unwrap_or_else(|| Path::new("."))
    }

    /// The `chromedriver` that AdsPower ships next to the kernel, if present.
    pub fn chromedriver(&self) -> Option<PathBuf> {
        let p = self.dir().join("chromedriver");
        p.is_file().then_some(p)
    }

    /// Explicit path, then `$CITRO_KERNEL`, then the newest kernel of an AdsPower install.
    pub fn discover(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Self::from_path(p);
        }
        if let Some(p) = std::env::var_os("CITRO_KERNEL").filter(|p| !p.is_empty()) {
            return Self::from_path(PathBuf::from(p));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let roots = [home.join(".adspowerCli"), home.join(".config/adspower_global/cwd_global")];
        let mut found: Vec<Kernel> = Vec::new();
        for root in &roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(major) = name.to_str().and_then(parse_major) else { continue };
                let binary = entry.path().join("SunBrowser");
                if binary.is_file() {
                    found.push(Kernel { binary, major });
                }
            }
        }
        found.sort_by_key(|k| k.major);
        found.pop().ok_or_else(|| {
            Error::KernelNotFound(roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_from_dir_name() {
        assert_eq!(parse_major("chrome_157"), Some(157));
        assert_eq!(parse_major("chrome_x"), None);
        assert_eq!(parse_major("source"), None);
    }

    #[test]
    fn missing_binary_is_an_error() {
        assert!(Kernel::from_path("/definitely/not/here/SunBrowser").is_err());
    }
}
