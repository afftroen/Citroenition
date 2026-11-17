//! The install pipeline, and the list of what is installed.
//!
//! Layout contract (shared with `citro-core`): `<kernels_root>/clearcote-<version>/chrome` (`chrome.exe` on
//! Windows, `Chromium.app/Contents/MacOS/Chromium` on macOS) next to an `INSTALLED.json`.

use crate::error::{Error, IoContext, Result};
use crate::extract::{self, Limits, Naming};
use crate::http::{Http, display_url, read_error};
use crate::platform::{ArchiveKind, Platform, host_binary_path};
use crate::release::{self, Plan};
use crate::verify::{hex_encode, pinned_key};
use crate::version::{compare_versions, is_valid_version};
use pgp::composed::SignedPublicKey;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Installed kernels live in directories called `clearcote-<version>`.
pub const DIR_PREFIX: &str = "clearcote-";
/// What is recorded about an install, next to the kernel.
pub const METADATA_FILE: &str = "INSTALLED.json";

/// The archives are 120 MB (Linux) to 230 MB (macOS); refuse anything beyond this.
const MAX_DOWNLOAD_BYTES: u64 = 2 << 30;
const IO_CHUNK: usize = 256 * 1024;
/// Leftovers of a killed install older than this are removed at the start of the next one.
const STALE_AFTER: Duration = Duration::from_secs(24 * 3600);
const TEMP_PREFIXES: [&str; 3] = [".tmp-", ".old-", ".download-"];

/// What to install and from where. Build it with `..InstallOptions::default()`.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Which kernel: a Chromium version (`151.0.7922.108`) or, from GitHub, a release tag (`v0.1.0-pre.25`).
    /// `None` is the latest release (or, with `base_url`, the newest kernel the mirror lists).
    pub version: Option<String>,
    /// Use a mirror serving `<base>/SHA256SUMS.txt`, `<base>/SHA256SUMS.txt.asc` and `<base>/<asset-name>` instead of
    /// GitHub. The signature is still checked against the pinned key.
    pub base_url: Option<String>,
    /// Reinstall even if this version is already there.
    pub force: bool,
    /// Bound on connecting and on every single read, so a stalled transfer fails and a long one does not.
    /// `Duration::ZERO` waits forever.
    pub timeout: Duration,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self { version: None, base_url: None, force: false, timeout: Duration::from_secs(30) }
    }
}

/// Where an install is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Looking up the release and checking the signature of its checksum list.
    Resolving,
    /// The archive is being downloaded; `total` is 0 when the size is unknown.
    Downloading {
        /// Bytes received so far.
        done: u64,
        /// Bytes expected.
        total: u64,
    },
    /// Comparing the archive's SHA-256 with the signed list.
    Verifying,
    /// Unpacking.
    Extracting,
    /// Finished (also reported when the kernel was already installed).
    Done,
}

/// An installed kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Installed {
    /// Chromium version, e.g. `151.0.7922.108`.
    pub version: String,
    /// `<kernels_root>/clearcote-<version>`
    pub dir: PathBuf,
    /// The browser executable inside `dir`.
    pub binary: PathBuf,
}

/// The kernels in `kernels_root`: directories named `clearcote-<version>` that hold the browser binary, newest
/// version first (numeric comparison: `151.0.7922.108` > `150.9.1` > `99.0.1`).
pub fn list_installed(kernels_root: &Path) -> Vec<Installed> {
    list_with(kernels_root, host_binary_path())
}

/// [`list_installed`] for a browser executable at `binary_path` inside each install directory.
fn list_with(kernels_root: &Path, binary_path: &str) -> Vec<Installed> {
    let Ok(entries) = fs::read_dir(kernels_root) else { return Vec::new() };
    let mut found: Vec<Installed> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let version = name.strip_prefix(DIR_PREFIX)?;
            if !is_valid_version(version) {
                return None;
            }
            let (dir, binary) = (e.path(), e.path().join(binary_path));
            binary.is_file().then(|| Installed { version: version.to_owned(), dir, binary })
        })
        .collect();
    found.sort_by(|a, b| compare_versions(&b.version, &a.version).then_with(|| b.version.cmp(&a.version)));
    found
}

/// Download, verify and unpack a Clearcote kernel into `kernels_root` (created if missing).
///
/// 1. find the release (GitHub API, or `opts.base_url`), fetch `SHA256SUMS.txt` and its signature and check the
///    signature against the pinned key: failure aborts before the big download;
/// 2. stream the archive to a temp file in `kernels_root`, hashing it on the way, and require the digest to equal
///    the signed list's line for that file;
/// 3. unpack into `<kernels_root>/.tmp-<random>/` (see the safety rules in `extract`), require the browser binary,
///    write `INSTALLED.json`, and `rename` the directory to `<kernels_root>/clearcote-<version>`.
///
/// Every temp file and directory is removed on failure. An existing install of the same version is returned as is
/// unless `opts.force`.
pub fn install(kernels_root: &Path, opts: &InstallOptions, progress: &mut dyn FnMut(Progress)) -> Result<Installed> {
    install_with(kernels_root, opts, pinned_key()?, Limits::default(), progress)
}

/// [`install`] with an explicit trust anchor and archive limits (the unit tests use a throw-away key).
pub(crate) fn install_with(
    root: &Path,
    opts: &InstallOptions,
    anchor: &SignedPublicKey,
    limits: Limits,
    progress: &mut dyn FnMut(Progress),
) -> Result<Installed> {
    progress(Progress::Resolving);
    let platform = Platform::detect()?;
    install_on(platform, root, opts, anchor, limits, progress)
}

/// [`install_with`] for a named platform instead of the one this program runs on (the unit tests install the macOS
/// kernel on Linux).
pub(crate) fn install_on(
    platform: Platform,
    root: &Path,
    opts: &InstallOptions,
    anchor: &SignedPublicKey,
    limits: Limits,
    progress: &mut dyn FnMut(Progress),
) -> Result<Installed> {
    fs::create_dir_all(root).context(|| format!("creating {}", root.display()))?;
    let http = Http::new(opts.timeout);
    let plan = release::plan(&http, opts, platform, anchor)?;

    let dir = root.join(format!("{DIR_PREFIX}{}", plan.release.version));
    let installed = Installed { version: plan.release.version.clone(), binary: dir.join(platform.binary_path()), dir };
    if !opts.force && installed.binary.is_file() {
        progress(Progress::Done);
        return Ok(installed);
    }
    cleanup_stale(root);

    let download = download_archive(&http, &plan, root, progress)?;

    progress(Progress::Verifying);
    if download.sha256 != plan.archive.sha256 {
        return Err(Error::ChecksumMismatch {
            what: plan.archive.name.clone(),
            expected: plan.archive.sha256.clone(),
            actual: download.sha256,
        });
    }

    progress(Progress::Extracting);
    let staging = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempdir_in(root)
        .context(|| format!("creating a temporary directory in {}", root.display()))?;
    match platform.archive_kind() {
        ArchiveKind::TarXz => extract::extract_tar_xz(download.file.path(), staging.path(), limits)?,
        ArchiveKind::Zip => extract::extract_zip(download.file.path(), staging.path(), limits, Naming::Windows)?,
        ArchiveKind::ZipUnix => extract::extract_zip(download.file.path(), staging.path(), limits, Naming::Posix)?,
    };
    extract::require_binary(staging.path(), platform.binary_path())?;
    write_metadata(staging.path(), &plan, &download.sha256)?;
    drop(download.file); // the archive is no longer needed; give the disk space back before the move
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `tempdir_in` makes the directory 0700; an install should look like any other directory.
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o755))
            .context(|| format!("setting permissions of {}", staging.path().display()))?;
    }
    publish(staging, &installed.dir, root, &installed.binary)?;
    progress(Progress::Done);
    Ok(installed)
}

struct Downloaded {
    file: tempfile::NamedTempFile,
    sha256: String,
}

/// Stream the archive into a temp file in `root`, hashing while it flows.
fn download_archive(http: &Http, plan: &Plan, root: &Path, progress: &mut dyn FnMut(Progress)) -> Result<Downloaded> {
    let url = plan.release.asset.url.as_str();
    let bad = |message: String| Error::BadResponse { url: display_url(url), message };
    let mut file = tempfile::Builder::new()
        .prefix(".download-")
        .suffix(".part")
        .tempfile_in(root)
        .context(|| format!("creating a temporary file in {}", root.display()))?;
    let mut body = http.open(url)?;
    let total = match (plan.release.asset.size, body.content_length) {
        (Some(listed), Some(sent)) if listed != sent => {
            return Err(bad(format!("the server announces {sent} bytes but the release lists {listed}")));
        }
        (listed, sent) => listed.or(sent),
    };
    if total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES) {
        return Err(bad(format!("the archive is larger than {MAX_DOWNLOAD_BYTES} bytes")));
    }
    let limit = total.unwrap_or(MAX_DOWNLOAD_BYTES);

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; IO_CHUNK];
    let mut done = 0u64;
    let mut last_report = Instant::now();
    progress(Progress::Downloading { done, total: total.unwrap_or(0) });
    loop {
        let n = match body.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(read_error(url, &e)),
        };
        done += n as u64;
        if done > limit {
            return Err(bad(format!("the server sends more than the {limit} bytes expected")));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).context(|| "writing the downloaded archive".to_owned())?;
        if last_report.elapsed() >= Duration::from_millis(100) {
            last_report = Instant::now();
            progress(Progress::Downloading { done, total: total.unwrap_or(0) });
        }
    }
    if let Some(t) = total
        && done != t
    {
        return Err(bad(format!("truncated download: received {done} of {t} bytes")));
    }
    file.flush().context(|| "writing the downloaded archive".to_owned())?;
    progress(Progress::Downloading { done, total: total.unwrap_or(done) });
    Ok(Downloaded { file, sha256: hex_encode(&hasher.finalize()) })
}

#[derive(Serialize)]
struct Metadata<'a> {
    version: &'a str,
    tarball_sha256: &'a str,
    installed_at: u64,
    source_url: String,
}

fn write_metadata(dir: &Path, plan: &Plan, sha256: &str) -> Result<()> {
    let meta = Metadata {
        version: &plan.release.version,
        tarball_sha256: sha256,
        installed_at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        source_url: display_url(&plan.release.asset.url),
    };
    let mut json = serde_json::to_vec_pretty(&meta).map_err(|e| Error::BadArchive(e.to_string()))?;
    json.push(b'\n');
    let path = dir.join(METADATA_FILE);
    fs::write(&path, json).context(|| format!("writing {}", path.display()))
}

/// A name nobody else is using: `<prefix><pid>-<nanos>-<counter>`.
fn unique_name(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{prefix}{}-{nanos:x}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Move the finished staging directory to `target` with one `rename`, so `target` is either absent or complete.
///
/// A previous `target` (only possible with `force`, or when it is broken: no binary) is moved aside first and
/// restored if the move fails. Losing a race to a concurrent install of the same version is not an error.
fn publish(staging: tempfile::TempDir, target: &Path, root: &Path, binary: &Path) -> Result<()> {
    let staged = staging.keep();
    let moving = |e| Error::io(format!("moving the unpacked kernel to {}", target.display()), e);
    let mut aside = None;
    if fs::symlink_metadata(target).is_ok() {
        let old = root.join(unique_name(".old-"));
        if let Err(e) = fs::rename(target, &old) {
            let _ = fs::remove_dir_all(&staged);
            return Err(moving(e));
        }
        aside = Some(old);
    }
    match fs::rename(&staged, target) {
        Ok(()) => {
            if let Some(old) = aside {
                let _ = fs::remove_dir_all(old);
            }
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&staged);
            match aside {
                Some(old) => {
                    let _ = fs::rename(&old, target);
                    Err(moving(e))
                }
                None if binary.is_file() => Ok(()),
                None => Err(moving(e)),
            }
        }
    }
}

/// Remove `.tmp-*`, `.old-*` and `.download-*` leftovers (a killed install) that are more than a day old.
fn cleanup_stale(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(|n| TEMP_PREFIXES.iter().any(|p| n.starts_with(p))) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let stale = meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = if meta.is_dir() { fs::remove_dir_all(entry.path()) } else { fs::remove_file(entry.path()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Mirror, Server, TestKey, ZipEntry, zip};

    const VERSION: &str = "150.0.7871.114";
    const FW: &str = "Chromium.app/Contents/Frameworks/Chromium Framework.framework";

    /// Clearcote's macOS archive in miniature: one `Chromium.app` with a launcher, the engine in its framework and
    /// the framework's relative links.
    fn macos_zip(with_launcher: bool) -> Vec<u8> {
        let mut entries = vec![
            ZipEntry::dir("Chromium.app/"),
            ZipEntry::exec(&format!("{FW}/Versions/{VERSION}/Chromium Framework"), b"engine"),
            ZipEntry::symlink(&format!("{FW}/Versions/Current"), VERSION),
            ZipEntry::symlink(&format!("{FW}/Chromium Framework"), "Versions/Current/Chromium Framework"),
        ];
        if with_launcher {
            entries.push(ZipEntry::exec("Chromium.app/Contents/MacOS/Chromium", b"launcher"));
        }
        zip(&entries)
    }

    fn from(server: &Server) -> InstallOptions {
        InstallOptions { base_url: Some(server.base.clone()), ..InstallOptions::default() }
    }

    fn install_macos(root: &Path, server: &Server, key: &TestKey, steps: &mut Vec<Progress>) -> Result<Installed> {
        let progress = &mut |p| steps.push(p);
        install_on(Platform::MacosArm64, root, &from(server), &key.public, Limits::default(), progress)
    }

    #[test]
    fn the_macos_kernel_installs_from_a_signed_mirror_into_its_app_bundle() {
        let key = TestKey::generate();
        let name = Platform::MacosArm64.asset_name(VERSION);
        let server = Server::mirror(&Mirror::signed(&key, &name, macos_zip(true)));
        let root = tempfile::tempdir().unwrap();

        let mut steps = Vec::new();
        let k = install_macos(root.path(), &server, &key, &mut steps).unwrap();
        assert_eq!(k.version, VERSION);
        assert_eq!(k.dir, root.path().join(format!("clearcote-{VERSION}")));
        assert_eq!(k.binary, k.dir.join("Chromium.app/Contents/MacOS/Chromium"));
        assert!(k.binary.is_file());
        // the framework's links came through as links
        assert_eq!(fs::read_link(k.dir.join(FW).join("Versions/Current")).unwrap(), Path::new(VERSION));
        assert_eq!(fs::read(k.dir.join(FW).join("Chromium Framework")).unwrap(), b"engine");
        assert!(k.dir.join(METADATA_FILE).is_file());
        assert!(steps.contains(&Progress::Extracting) && steps.last() == Some(&Progress::Done), "{steps:?}");

        // kernels are listed by their platform's executable path: found as macOS, not as Linux
        assert_eq!(list_with(root.path(), Platform::MacosArm64.binary_path()), vec![k.clone()]);
        assert!(list_with(root.path(), Platform::LinuxX64.binary_path()).is_empty());

        // a second install is a no-op: nothing is downloaded again
        let again = install_macos(root.path(), &server, &key, &mut Vec::new()).unwrap();
        assert_eq!(again, k);
        assert_eq!(server.hits(&format!("/{name}")), 1);
    }

    #[test]
    fn another_platforms_archive_never_stands_in_for_the_macos_one() {
        let key = TestKey::generate();
        let server = Server::mirror(&Mirror::signed(&key, &Platform::LinuxX64.asset_name(VERSION), b"x".to_vec()));
        let root = tempfile::tempdir().unwrap();
        let e = install_macos(root.path(), &server, &key, &mut Vec::new()).unwrap_err();
        assert!(matches!(e, Error::NoArchive { .. }), "{e}");
    }

    #[test]
    fn a_macos_archive_without_the_launcher_is_refused_and_leaves_nothing_behind() {
        let key = TestKey::generate();
        let name = Platform::MacosArm64.asset_name(VERSION);
        let server = Server::mirror(&Mirror::signed(&key, &name, macos_zip(false)));
        let root = tempfile::tempdir().unwrap();
        let e = install_macos(root.path(), &server, &key, &mut Vec::new()).unwrap_err();
        assert!(matches!(&e, Error::MissingBinary(b) if b == "Chromium.app/Contents/MacOS/Chromium"), "{e}");
        let left: Vec<_> = fs::read_dir(root.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }
}
