//! Updating Citroenition itself from its own GitHub releases (`afftroen/Citroenition`).
//!
//! A release carries one `.tar.gz` per platform (`citroenition-v0.1.2-aarch64-macos.tar.gz`) and a plain
//! `SHA256SUMS` over them. Trust model, weaker than the kernel's (there is no signing key for the app yet, so it is
//! said plainly here): the files are fetched over TLS from the repository's own release-download URLs only, and the
//! archive's SHA-256 must match the release's `SHA256SUMS`. That catches a corrupt or truncated download and a mirror
//! that swapped one file, not a compromised GitHub account. The unpack uses the same path-safe tar extractor as the
//! kernel.
//!
//! This module only finds, downloads, verifies and unpacks a release into a staging directory. Swapping the running
//! installation for the new one is the desktop app's job (it knows where it runs from).

use crate::error::{Error, IoContext, Result};
use crate::extract::{self, Limits};
use crate::http::{Http, read_error};
use crate::release::API_ROOT;
use crate::version::{compare_versions, is_valid_version};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The repository whose releases are Citroenition's.
pub const APP_REPO: &str = "afftroen/Citroenition";
/// Name of the checksum list in every app release.
pub const APP_SUMS_NAME: &str = "SHA256SUMS";
/// Archives are only ever downloaded from here.
const APP_DOWNLOAD_PREFIX: &str = "https://github.com/afftroen/Citroenition/releases/download/";
const MAX_JSON_BYTES: u64 = 4 << 20;
const MAX_SUMS_BYTES: u64 = 64 << 10;
/// An app archive is ~15 MB; anything near this is not one.
const MAX_ARCHIVE_BYTES: u64 = 512 << 20;
const CHUNK: usize = 256 * 1024;

/// A Citroenition release that has an archive for this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRelease {
    /// `0.1.2` (the tag without its `v`).
    pub version: String,
    /// `v0.1.2`.
    pub tag: String,
    /// Archive file name, e.g. `citroenition-v0.1.2-aarch64-macos.tar.gz`.
    pub archive_name: String,
    pub archive_url: String,
    /// Announced size in bytes (0 when unknown).
    pub archive_size: u64,
    pub sums_url: String,
    /// The release's web page.
    pub page_url: String,
    /// The release notes (Markdown).
    pub notes: String,
}

impl AppRelease {
    /// `true` when this release is newer than `current` (e.g. `env!("CARGO_PKG_VERSION")`).
    pub fn is_newer_than(&self, current: &str) -> bool {
        compare_versions(&self.version, current) == Ordering::Greater
    }
}

/// The platform part of the archive names this computer can run: `x86_64-linux` or `aarch64-macos`.
pub fn app_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-linux"),
        ("macos", "aarch64") => Some("aarch64-macos"),
        _ => None,
    }
}

fn unsupported() -> Error {
    Error::UnsupportedApp { os: std::env::consts::OS.to_owned(), arch: std::env::consts::ARCH.to_owned() }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

/// Pick the archive for `target` out of the API's JSON for one release.
fn parse_release(json: &[u8], target: &str, url: &str) -> Result<AppRelease> {
    let bad = |message: String| Error::BadResponse { url: url.to_owned(), message };
    let raw: RawRelease = serde_json::from_slice(json).map_err(|e| bad(e.to_string()))?;
    if raw.draft || raw.prerelease {
        return Err(bad(format!("{} is a draft or a pre-release", raw.tag_name)));
    }
    let version = raw.tag_name.strip_prefix('v').unwrap_or(&raw.tag_name).to_owned();
    if !is_valid_version(&version) {
        return Err(Error::InvalidInput { what: "release tag", value: raw.tag_name });
    }
    let archive_name = format!("citroenition-v{version}-{target}.tar.gz");
    let find = |name: &str| -> Result<&RawAsset> {
        let a = raw
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| Error::MissingAsset { tag: raw.tag_name.clone(), name: name.to_owned() })?;
        if !a.browser_download_url.starts_with(APP_DOWNLOAD_PREFIX) {
            return Err(bad(format!("`{name}` is not hosted in {APP_REPO}'s releases")));
        }
        Ok(a)
    };
    let archive = find(&archive_name)?;
    let sums = find(APP_SUMS_NAME)?;
    Ok(AppRelease {
        tag: raw.tag_name.clone(),
        archive_url: archive.browser_download_url.clone(),
        archive_size: archive.size,
        sums_url: sums.browser_download_url.clone(),
        page_url: raw.html_url.clone().unwrap_or_else(|| format!("https://github.com/{APP_REPO}/releases")),
        notes: raw.body.clone().unwrap_or_default(),
        archive_name,
        version,
    })
}

/// The newest published Citroenition release for this computer.
pub fn latest_app_release(timeout: Duration) -> Result<AppRelease> {
    let target = app_target().ok_or_else(unsupported)?;
    let url = format!("{API_ROOT}/repos/{APP_REPO}/releases/latest");
    let json = Http::new(timeout).get_bytes(&url, MAX_JSON_BYTES)?;
    parse_release(&json, target, &url)
}

/// The digest `SHA256SUMS` gives for `name` (`<64 hex>  <name>` lines, `*name` for binary mode too).
fn digest_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

/// Download `release` into `staging` (created, and emptied first), check its SHA-256 against the release's
/// `SHA256SUMS`, unpack it and return the unpacked top directory (`<staging>/unpacked/citroenition-v<ver>-<target>`).
/// `progress(done, total)` follows the archive download; `total` is 0 while unknown.
pub fn download_app_release(
    release: &AppRelease,
    staging: &Path,
    timeout: Duration,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf> {
    let target = app_target().ok_or_else(unsupported)?;
    for url in [&release.archive_url, &release.sums_url] {
        if !url.starts_with(APP_DOWNLOAD_PREFIX) {
            return Err(Error::InvalidInput { what: "download URL", value: url.clone() });
        }
    }
    if staging.exists() {
        fs::remove_dir_all(staging).context(|| format!("cannot clear {}", staging.display()))?;
    }
    fs::create_dir_all(staging).context(|| format!("cannot create {}", staging.display()))?;
    let http = Http::new(timeout);

    let sums = http.get_bytes(&release.sums_url, MAX_SUMS_BYTES)?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = digest_for(&sums, &release.archive_name)
        .ok_or_else(|| Error::BadSums(format!("no line for {}", release.archive_name)))?;

    let archive = staging.join(&release.archive_name);
    let mut download = http.open(&release.archive_url)?;
    let total = download.content_length.unwrap_or(release.archive_size);
    let mut out = File::create(&archive).context(|| format!("cannot create {}", archive.display()))?;
    let (mut hasher, mut done, mut buf) = (Sha256::new(), 0u64, vec![0u8; CHUNK]);
    progress(0, total);
    loop {
        let n = download.reader.read(&mut buf).map_err(|e| read_error(&release.archive_url, &e))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > MAX_ARCHIVE_BYTES {
            return Err(Error::ArchiveLimit(format!("the download is larger than {MAX_ARCHIVE_BYTES} bytes")));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).context(|| format!("cannot write {}", archive.display()))?;
        progress(done, total);
    }
    out.flush().context(|| format!("cannot write {}", archive.display()))?;
    drop(out);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(Error::ChecksumMismatch { what: release.archive_name.clone(), expected, actual });
    }

    let unpacked = staging.join("unpacked");
    fs::create_dir_all(&unpacked).context(|| format!("cannot create {}", unpacked.display()))?;
    let file = File::open(&archive).context(|| format!("cannot open {}", archive.display()))?;
    let limits = Limits { max_total_bytes: 2 << 30, max_entries: 50_000 };
    extract::extract_tar(flate2::read::GzDecoder::new(std::io::BufReader::new(file)), &unpacked, limits)?;
    let _ = fs::remove_file(&archive);
    let root = unpacked.join(format!("citroenition-v{}-{target}", release.version));
    extract::require_binary(&root, "citro")?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release_json(tag: &str, prefix: &str) -> Vec<u8> {
        let v = tag.trim_start_matches('v');
        serde_json::to_vec(&json!({
            "tag_name": tag, "draft": false, "prerelease": false,
            "html_url": format!("https://github.com/{APP_REPO}/releases/tag/{tag}"),
            "body": "## notes",
            "assets": [
                {"name": format!("citroenition-v{v}-aarch64-macos.tar.gz"), "size": 15,
                 "browser_download_url": format!("{prefix}{tag}/citroenition-v{v}-aarch64-macos.tar.gz")},
                {"name": format!("citroenition-v{v}-x86_64-linux.tar.gz"), "size": 16,
                 "browser_download_url": format!("{prefix}{tag}/citroenition-v{v}-x86_64-linux.tar.gz")},
                {"name": "SHA256SUMS", "size": 1, "browser_download_url": format!("{prefix}{tag}/SHA256SUMS")}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn the_archive_for_this_platform_is_picked() {
        let r = parse_release(&release_json("v0.1.2", APP_DOWNLOAD_PREFIX), "aarch64-macos", "u").unwrap();
        assert_eq!((r.version.as_str(), r.archive_size), ("0.1.2", 15));
        assert_eq!(r.archive_name, "citroenition-v0.1.2-aarch64-macos.tar.gz");
        assert!(r.sums_url.ends_with("/v0.1.2/SHA256SUMS"));
        assert!(r.is_newer_than("0.1.1") && !r.is_newer_than("0.1.2") && !r.is_newer_than("0.2.0"));
        let linux = parse_release(&release_json("v0.1.2", APP_DOWNLOAD_PREFIX), "x86_64-linux", "u").unwrap();
        assert_eq!(linux.archive_size, 16);
    }

    #[test]
    fn foreign_hosts_bad_tags_and_missing_archives_are_refused() {
        let elsewhere = release_json("v0.1.2", "https://evil.example/releases/download/");
        assert!(parse_release(&elsewhere, "aarch64-macos", "u").is_err());
        assert!(parse_release(&release_json("v0.1.2-rc1", APP_DOWNLOAD_PREFIX), "aarch64-macos", "u").is_err());
        assert!(matches!(
            parse_release(&release_json("v0.1.2", APP_DOWNLOAD_PREFIX), "x86_64-windows", "u"),
            Err(Error::MissingAsset { .. })
        ));
    }

    #[test]
    fn sums_lines_are_matched_by_exact_name() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums =
            format!("{a}  citroenition-v0.1.2-x86_64-linux.tar.gz\n{b} *citroenition-v0.1.2-aarch64-macos.tar.gz\n");
        assert_eq!(digest_for(&sums, "citroenition-v0.1.2-x86_64-linux.tar.gz"), Some(a));
        assert_eq!(digest_for(&sums, "citroenition-v0.1.2-aarch64-macos.tar.gz"), Some("b".repeat(64)));
        assert_eq!(digest_for(&sums, "citroenition-v0.1.2-x86_64-linux.tar"), None);
        assert_eq!(digest_for("short  x.tar.gz", "x.tar.gz"), None);
    }
}
