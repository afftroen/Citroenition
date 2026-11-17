//! Locating and probing the Clearcote kernel.
//!
//! Citroenition does not bundle a browser. The kernel is [Clearcote](https://github.com/clearcotelabs/clearcote-browser),
//! an open-source Chromium build whose identity surface (hardware, screen, GPU, canvas, audio, TLS, ...) is
//! decided inside the engine from one seed. `citro kernel install` downloads, verifies and unpacks it to
//! `<home>/kernels/clearcote-<version>/chrome` (on macOS the executable of the `Chromium.app` bundle in that
//! directory); this module only finds what is installed.

use crate::store::Home;
use crate::{Error, Result};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The executable inside a kernel directory: `chrome`, or on macOS the one inside the app bundle Clearcote's
/// archive unpacks to.
pub const BINARY: &str = if cfg!(target_os = "macos") { MACOS_BINARY } else { "chrome" };
/// Where the macOS archive puts the browser, relative to the install directory.
pub const MACOS_BINARY: &str = "Chromium.app/Contents/MacOS/Chromium";
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

/// The `X.app` directory when `binary` is `X.app/Contents/MacOS/<name>` (a macOS app bundle's executable).
fn app_bundle(binary: &Path) -> Option<&Path> {
    let macos = binary.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app").then_some(app)
}

/// The directory a kernel's files live in (`clearcote-<version>`): the one holding the executable, or, for an app
/// bundle, the one holding the bundle.
fn install_dir(binary: &Path) -> &Path {
    match app_bundle(binary) {
        Some(app) => app.parent().unwrap_or(app),
        None => binary.parent().unwrap_or_else(|| Path::new(".")),
    }
}

impl Kernel {
    pub fn from_path(binary: impl Into<PathBuf>) -> Result<Self> {
        let binary = binary.into();
        if !binary.is_file() {
            return Err(Error::KernelNotFound(binary.display().to_string()));
        }
        let version =
            install_dir(&binary).file_name().and_then(|n| n.to_str()).and_then(parse_version).unwrap_or_default();
        let major = version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        Ok(Self { binary, version, major })
    }

    /// The install directory: where the binary is, or, on macOS, where the app bundle is.
    pub fn dir(&self) -> &Path {
        install_dir(&self.binary)
    }

    /// Every kernel installed under `<home>/kernels`, newest first.
    pub fn installed(home: &Home) -> Vec<Kernel> {
        Self::installed_in(&kernels_dir(home), BINARY)
    }

    /// [`Kernel::installed`] for kernels whose executable is at `binary` (relative) inside each install directory.
    fn installed_in(root: &Path, binary: &str) -> Vec<Kernel> {
        let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
        let mut found: Vec<Kernel> = entries
            .flatten()
            .filter(|e| e.file_name().to_str().and_then(parse_version).is_some())
            .filter_map(|e| Kernel::from_path(e.path().join(binary)).ok())
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
        use std::collections::HashMap;
        use std::sync::Mutex;
        // the engine is hundreds of megabytes: look each name up once per engine file (and its modification time)
        type Seen = HashMap<(PathBuf, Option<std::time::SystemTime>, String), bool>;
        static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
        let image = self.engine_image();
        let key = (image.clone(), std::fs::metadata(&image).and_then(|m| m.modified()).ok(), name.to_owned());
        if let Some(&hit) = SEEN.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&key).copied())).as_ref() {
            return hit;
        }
        let needle = [&[0u8][..], name.as_bytes(), &[0u8][..]].concat();
        let found = file_contains(&image, &needle, 8 << 20).unwrap_or(false);
        if let Ok(mut m) = SEEN.lock() {
            m.get_or_insert_with(HashMap::new).insert(key, found);
        }
        found
    }

    /// The file that holds the engine's code, where switch names are looked up: the executable itself, except in a
    /// macOS app bundle, where `Contents/MacOS/Chromium` is a small launcher and the engine is the framework next
    /// to it (`Contents/Frameworks/Chromium Framework.framework/Chromium Framework`).
    pub fn engine_image(&self) -> PathBuf {
        let framework =
            app_bundle(&self.binary).zip(self.binary.file_name().and_then(|n| n.to_str())).map(|(app, name)| {
                let dir = app.join("Contents").join("Frameworks").join(format!("{name} Framework.framework"));
                dir.join(format!("{name} Framework"))
            });
        framework.filter(|p| p.is_file()).unwrap_or_else(|| self.binary.clone())
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

    /// A kernel in the layout this platform's installer leaves (`chrome`, or the app bundle's executable).
    fn install(home: &Home, version: &str) -> PathBuf {
        let binary = kernels_dir(home).join(format!("{DIR_PREFIX}{version}")).join(BINARY);
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, b"\0proxy-auth\0").unwrap();
        binary
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

    /// `<root>/clearcote-<version>` laid out like Clearcote's macOS archive: a launcher, and the engine in the
    /// framework next to it, reached through the framework's relative links.
    fn macos_install(root: &Path, version: &str, engine: &[u8]) -> PathBuf {
        use std::os::unix::fs::symlink;
        let dir = root.join(format!("{DIR_PREFIX}{version}"));
        let fw = dir.join("Chromium.app/Contents/Frameworks/Chromium Framework.framework");
        fs::create_dir_all(dir.join("Chromium.app/Contents/MacOS")).unwrap();
        fs::create_dir_all(fw.join("Versions").join(version)).unwrap();
        fs::write(dir.join(MACOS_BINARY), b"launcher: the engine is not in here").unwrap();
        fs::write(fw.join("Versions").join(version).join("Chromium Framework"), engine).unwrap();
        symlink(version, fw.join("Versions/Current")).unwrap();
        symlink("Versions/Current/Chromium Framework", fw.join("Chromium Framework")).unwrap();
        dir.join(MACOS_BINARY)
    }

    #[test]
    fn a_macos_app_bundle_is_versioned_and_probed_through_the_directory_and_framework_around_it() {
        let root = tmp("bundle");
        let launcher = macos_install(&root, "150.0.7871.114", b"xx\0persona-from-env\0yy");
        let k = Kernel::from_path(&launcher).unwrap();
        assert_eq!((k.version.as_str(), k.major), ("150.0.7871.114", 150));
        assert_eq!(k.dir(), root.join("clearcote-150.0.7871.114"));
        // the launcher stub holds no switch names; the engine does
        assert_eq!(k.engine_image().file_name().unwrap(), "Chromium Framework");
        assert!(k.supports_switch("persona-from-env"));
        assert!(!k.supports_switch("socks5-credentials"));
        // found by `installed` the way kernels are on this platform, or not at all
        macos_install(&root, "99.0.1.1", b"old");
        let found = Kernel::installed_in(&root, MACOS_BINARY);
        assert_eq!(found.iter().map(|k| k.version.as_str()).collect::<Vec<_>>(), ["150.0.7871.114", "99.0.1.1"]);
        assert!(Kernel::installed_in(&root, "chrome").is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn only_an_app_bundles_executable_counts_as_one() {
        for not_a_bundle in
            ["/k/clearcote-1.2.3/chrome", "/k/MacOS/Chromium", "/k/X.app/Contents/Resources/y", "/k/X/Contents/MacOS/y"]
        {
            assert_eq!(app_bundle(Path::new(not_a_bundle)), None, "{not_a_bundle}");
            assert_eq!(install_dir(Path::new(not_a_bundle)), Path::new(not_a_bundle).parent().unwrap());
        }
        let bundle = Path::new("/k/clearcote-1.2.3/Chromium.app/Contents/MacOS/Chromium");
        assert_eq!(app_bundle(bundle), Some(Path::new("/k/clearcote-1.2.3/Chromium.app")));
        assert_eq!(install_dir(bundle), Path::new("/k/clearcote-1.2.3"));
        // a flat install still has everything next to the binary, whatever its name (and whatever this platform's own
        // layout is: the file is made by hand)
        let root = tmp("flat");
        let flat = root.join("clearcote-151.0.7922.108").join("chrome");
        fs::create_dir_all(flat.parent().unwrap()).unwrap();
        fs::write(&flat, b"\0proxy-auth\0").unwrap();
        let k = Kernel::from_path(&flat).unwrap();
        assert_eq!((k.dir(), k.engine_image()), (flat.parent().unwrap(), flat.clone()));
        let _ = fs::remove_dir_all(&root);
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
