//! Locating and probing the Clearcote kernel.
//!
//! Citroenition does not bundle a browser. The kernel is [Clearcote](https://github.com/clearcotelabs/clearcote-browser),
//! an open-source Chromium build whose identity surface (hardware, screen, GPU, canvas, audio, TLS, ...) is
//! decided inside the engine from one seed. `citro kernel install` downloads, verifies and unpacks it to
//! `<home>/kernels/clearcote-<version>/chrome`; this module only finds what is installed.

use crate::store::Home;
use crate::{Error, Result};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The executable inside a kernel directory.
pub const BINARY: &str = "chrome";
/// Install directories are named `clearcote-<version>`.
pub const DIR_PREFIX: &str = "clearcote-";

#[derive(Debug, Clone)]
pub struct Kernel {
    pub binary: PathBuf,
    /// `151.0.7922.108`, or empty when the install directory name carries no version.
    pub version: String,
    /// The Chromium major (`151`); 0 when unknown.
    pub major: u32,
}

/// `clearcote-151.0.7922.108` and `clearcote-151.0.7922.108-linux-x64` -> `151.0.7922.108`.
pub fn parse_version(dir_name: &str) -> Option<String> {
    let v = dir_name.strip_prefix(DIR_PREFIX)?.split('-').next()?;
    v.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).then(|| v.to_owned())
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Where `citro kernel install` puts kernels.
pub fn kernels_dir(home: &Home) -> PathBuf {
    home.root.join("kernels")
}

impl Kernel {
    pub fn from_path(binary: impl Into<PathBuf>) -> Result<Self> {
        let binary = binary.into();
        if !binary.is_file() {
            return Err(Error::KernelNotFound(binary.display().to_string()));
        }
        let version = binary
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .and_then(parse_version)
            .unwrap_or_default();
        let major = version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        Ok(Self { binary, version, major })
    }

    pub fn dir(&self) -> &Path {
        self.binary.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Every kernel installed under `<home>/kernels`, newest first.
    pub fn installed(home: &Home) -> Vec<Kernel> {
        let Ok(entries) = std::fs::read_dir(kernels_dir(home)) else { return Vec::new() };
        let mut found: Vec<Kernel> = entries
            .flatten()
            .filter(|e| e.file_name().to_str().and_then(parse_version).is_some())
            .filter_map(|e| Kernel::from_path(e.path().join(BINARY)).ok())
            .collect();
        found.sort_by_key(|k| std::cmp::Reverse(version_key(&k.version)));
        found
    }

    /// An explicit path, then `$CITRO_KERNEL`, then the newest installed kernel.
    pub fn discover(home: &Home, explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Self::from_path(p);
        }
        if let Some(p) = std::env::var_os("CITRO_KERNEL").filter(|p| !p.is_empty()) {
            return Self::from_path(PathBuf::from(p));
        }
        Self::installed(home)
            .into_iter()
            .next()
            .ok_or_else(|| Error::KernelNotFound(kernels_dir(home).display().to_string()))
    }

    /// A `chromedriver` next to the kernel (Clearcote does not ship one), or `$CITRO_CHROMEDRIVER`.
    pub fn chromedriver(&self) -> Option<PathBuf> {
        let env = std::env::var_os("CITRO_CHROMEDRIVER").map(PathBuf::from).filter(|p| p.is_file());
        env.or_else(|| Some(self.dir().join("chromedriver")).filter(|p| p.is_file()))
    }

    /// The font bundle's fontconfig template, present in Linux releases.
    pub fn fonts_template(&self) -> Option<PathBuf> {
        Some(self.dir().join("fonts").join("fonts.conf.template")).filter(|p| p.is_file())
    }

    /// Does this engine implement the command-line switch `name`? Chromium keeps switch names as
    /// NUL-terminated string literals in the binary, so a NUL-delimited search is an exact capability probe
    /// (`proxy-auth` does not match the `proxy-authenticate` header name). Any failure answers `false`.
    pub fn supports_switch(&self, name: &str) -> bool {
        let needle = [&[0u8][..], name.as_bytes(), &[0u8][..]].concat();
        file_contains(&self.binary, &needle, 8 << 20).unwrap_or(false)
    }
}

/// Does the file contain `needle`? Reads in `chunk`-byte pieces, carrying the tail across boundaries.
fn file_contains(path: &Path, needle: &[u8], chunk: usize) -> std::io::Result<bool> {
    let finder = memchr::memmem::Finder::new(needle);
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; chunk + needle.len()];
    let mut carry = 0;
    loop {
        let n = file.read(&mut buf[carry..])?;
        if n == 0 {
            return Ok(false);
        }
        let end = carry + n;
        if finder.find(&buf[..end]).is_some() {
            return Ok(true);
        }
        carry = (needle.len() - 1).min(end);
        buf.copy_within(end - carry..end, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("citro-kernel-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn install(home: &Home, version: &str) -> PathBuf {
        let dir = kernels_dir(home).join(format!("{DIR_PREFIX}{version}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(BINARY), b"\0proxy-auth\0").unwrap();
        dir.join(BINARY)
    }

    #[test]
    fn versions_come_from_the_directory_name() {
        assert_eq!(parse_version("clearcote-151.0.7922.108").as_deref(), Some("151.0.7922.108"));
        assert_eq!(parse_version("clearcote-151.0.7922.108-linux-x64").as_deref(), Some("151.0.7922.108"));
        assert_eq!(parse_version("clearcote-"), None);
        assert_eq!(parse_version("clearcote-x.1"), None);
        assert_eq!(parse_version("chrome_157"), None);
    }

    #[test]
    fn newest_installed_kernel_wins_by_numeric_comparison() {
        let home = Home { root: tmp("installed") };
        install(&home, "99.0.1.1");
        let newest = install(&home, "151.0.7922.108");
        install(&home, "151.0.7922.9");
        fs::create_dir_all(kernels_dir(&home).join(".tmp-abc")).unwrap();
        fs::create_dir_all(kernels_dir(&home).join("clearcote-152.0.1.1")).unwrap(); // no binary: skipped
        let all = Kernel::installed(&home);
        assert_eq!(
            all.iter().map(|k| k.version.as_str()).collect::<Vec<_>>(),
            ["151.0.7922.108", "151.0.7922.9", "99.0.1.1"]
        );
        assert_eq!(Kernel::discover(&home, None).unwrap().binary, newest);
        assert_eq!(all[0].major, 151);
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn an_empty_home_says_where_it_looked() {
        let home = Home { root: tmp("empty") };
        let err = Kernel::discover(&home, None).unwrap_err().to_string();
        assert!(err.contains("kernels") && err.contains("citro kernel install"), "{err}");
        assert!(Kernel::from_path("/definitely/not/here/chrome").is_err());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn switch_probe_is_nul_delimited_and_crosses_chunk_boundaries() {
        let dir = tmp("probe");
        let bin = dir.join("chrome");
        fs::write(&bin, b"xx\0proxy-authenticate\0yy\0socks5-credentials\0zz").unwrap();
        let k = Kernel { binary: bin.clone(), version: String::new(), major: 0 };
        assert!(k.supports_switch("socks5-credentials"));
        assert!(!k.supports_switch("proxy-auth"), "a longer switch name is not a match");
        assert!(!k.supports_switch("persona-from-env"));
        // the needle straddles every possible chunk boundary
        let needle = b"\0persona-from-env\0";
        for chunk in 1..=needle.len() + 3 {
            for pad in 0..chunk + 2 {
                let mut data = vec![b'a'; pad];
                data.extend_from_slice(needle);
                data.extend_from_slice(b"tail");
                fs::write(&bin, &data).unwrap();
                assert!(file_contains(&bin, needle, chunk).unwrap(), "chunk {chunk} pad {pad}");
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
