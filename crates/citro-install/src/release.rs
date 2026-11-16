//! Finding out which kernel to install and where its files are.
//!
//! * **GitHub** (default): the releases API of `clearcotelabs/clearcote-browser` says which assets a release has.
//!   Nothing in that JSON is trusted beyond locating files: what gets installed is decided by the signed
//!   `SHA256SUMS.txt`.
//! * **Mirror** (`InstallOptions::base_url`): a plain directory listing `<base>/SHA256SUMS.txt`,
//!   `<base>/SHA256SUMS.txt.asc` and `<base>/<asset-name>`. No JSON is involved; the archive is picked from the
//!   signed list.

use crate::error::{Error, Result};
use crate::http::{Http, display_url};
use crate::install::InstallOptions;
use crate::platform::Platform;
use crate::verify::{ChecksumList, ListedArchive, verify_with_key};
use crate::version::is_valid_version;
use pgp::composed::SignedPublicKey;
use serde::{Deserialize, Serialize};

/// The release repository.
pub const REPO: &str = "clearcotelabs/clearcote-browser";
/// The GitHub API root.
pub const API_ROOT: &str = "https://api.github.com";
/// Name of the signed checksum list in every release.
pub const SUMS_NAME: &str = "SHA256SUMS.txt";
/// Name of its detached OpenPGP signature.
pub const SIG_NAME: &str = "SHA256SUMS.txt.asc";
/// Name of the release's copy of the signing key. Informational: it is NEVER used to verify anything.
pub const KEY_NAME: &str = "clearcote-signing-key.asc";

/// Downloads of metadata (release JSON) are capped at this size.
const MAX_JSON_BYTES: u64 = 8 << 20;
/// Release assets are only fetched from here (the API's `browser_download_url`s all start like this).
const DOWNLOAD_PREFIX: &str = "https://github.com/";
/// How many releases `--version <chrome version>` looks through when the version is not the latest one.
const SEARCH_RELEASES: u32 = 100;

/// A file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Asset {
    /// File name, e.g. `clearcote-151.0.7922.108-linux-x64.tar.xz`.
    pub name: String,
    /// Where to download it.
    pub url: String,
    /// Size in bytes as announced by the API (`None` for a mirror: it is learnt from the download).
    pub size: Option<u64>,
}

/// A Clearcote release as far as this host is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    /// Release tag (`v0.1.0-pre.25`); empty for a mirror.
    pub tag: String,
    /// Chromium version of the kernel (`151.0.7922.108`), taken from the archive name.
    pub version: String,
    /// The platform whose archive this is.
    pub platform: Platform,
    /// The archive for [`Release::platform`].
    pub asset: Asset,
    /// URL of `SHA256SUMS.txt`.
    pub sums_url: String,
    /// URL of `SHA256SUMS.txt.asc`.
    pub sig_url: String,
    /// URL of the release's copy of the signing key (informational; the pinned key is the only one ever used).
    pub key_url: Option<String>,
    /// Web page of the release.
    pub page_url: Option<String>,
}

/// A release plus everything that was verified about it: the checksum list (signature checked) and the archive's line.
#[derive(Debug)]
pub(crate) struct Plan {
    pub release: Release,
    pub archive: ListedArchive,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    state: Option<String>,
}

/// Latest release (`/releases/latest`) as GitHub reports it, or, with `opts.base_url`, the newest kernel in the
/// mirror's signed list. `opts.version` is ignored here.
pub fn latest_release(opts: &InstallOptions) -> Result<Release> {
    let platform = Platform::detect()?;
    let http = Http::new(opts.timeout);
    match mirror_base(opts)? {
        Some(base) => Ok(plan_mirror(&http, &base, platform, None, crate::verify::pinned_key()?)?.release),
        None => github_latest(&http, API_ROOT, platform),
    }
}

/// The release with git tag `tag` (GitHub only; a mirror has no tags).
pub fn release_by_tag(tag: &str, opts: &InstallOptions) -> Result<Release> {
    let platform = Platform::detect()?;
    if mirror_base(opts)?.is_some() {
        return Err(Error::InvalidInput {
            what: "release tag (a mirror serves a single release; use --version)",
            value: tag.to_owned(),
        });
    }
    github_by_tag(&Http::new(opts.timeout), API_ROOT, platform, tag)
}

/// `base_url` without trailing slashes, if one is configured. Only `http(s)://` is accepted.
pub(crate) fn mirror_base(opts: &InstallOptions) -> Result<Option<String>> {
    let Some(raw) = opts.base_url.as_deref().map(str::trim).filter(|b| !b.is_empty()) else { return Ok(None) };
    let lower = raw.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) || raw.contains(char::is_whitespace) {
        return Err(Error::InvalidInput { what: "base URL (expected http:// or https://)", value: display_url(raw) });
    }
    Ok(Some(raw.trim_end_matches('/').to_owned()))
}

/// Resolve what `opts` asks for and verify the checksum list against `anchor`: the entry point of `install`.
pub(crate) fn plan(http: &Http, opts: &InstallOptions, platform: Platform, anchor: &SignedPublicKey) -> Result<Plan> {
    let want = opts.version.as_deref().map(str::trim).filter(|v| !v.is_empty());
    if let Some(base) = mirror_base(opts)? {
        if let Some(v) = want
            && !is_valid_version(v)
        {
            return Err(Error::InvalidInput { what: "kernel version (e.g. 151.0.7922.108)", value: v.to_owned() });
        }
        return plan_mirror(http, &base, platform, want, anchor);
    }
    let release = match want {
        None => github_latest(http, API_ROOT, platform)?,
        Some(v) if is_valid_version(v) => github_by_version(http, API_ROOT, platform, v)?,
        Some(tag) => github_by_tag(http, API_ROOT, platform, tag)?,
    };
    plan_release(http, release, anchor)
}

/// Fetch and verify the signed list a release points to, and find the release's archive in it.
pub(crate) fn plan_release(http: &Http, release: Release, anchor: &SignedPublicKey) -> Result<Plan> {
    let list = fetch_signed_list(http, &release.sums_url, &release.sig_url, anchor)?;
    let archive = list.find_archive(release.platform, Some(&release.version))?;
    if archive.name != release.asset.name {
        return Err(Error::NoArchive { suffix: release.asset.name });
    }
    Ok(Plan { release, archive })
}

fn plan_mirror(
    http: &Http,
    base: &str,
    platform: Platform,
    want: Option<&str>,
    anchor: &SignedPublicKey,
) -> Result<Plan> {
    let sums_url = format!("{base}/{SUMS_NAME}");
    let sig_url = format!("{base}/{SIG_NAME}");
    let list = fetch_signed_list(http, &sums_url, &sig_url, anchor)?;
    let archive = list.find_archive(platform, want)?;
    let release = Release {
        tag: String::new(),
        version: archive.version.clone(),
        platform,
        asset: Asset { name: archive.name.clone(), url: format!("{base}/{}", archive.name), size: None },
        sums_url,
        sig_url,
        key_url: None,
        page_url: None,
    };
    Ok(Plan { release, archive })
}

fn fetch_signed_list(http: &Http, sums_url: &str, sig_url: &str, anchor: &SignedPublicKey) -> Result<ChecksumList> {
    let sums = http.get_bytes(sums_url, crate::verify::MAX_SUMS_BYTES as u64)?;
    let sig = http.get_bytes(sig_url, crate::verify::MAX_SIGNATURE_BYTES as u64)?;
    verify_with_key(anchor, &sums, &sig)
}

fn releases_url(api_root: &str, tail: &str) -> String {
    format!("{}/repos/{REPO}/releases{tail}", api_root.trim_end_matches('/'))
}

fn fetch_json<T: for<'de> Deserialize<'de>>(http: &Http, url: &str) -> Result<T> {
    let body = http.get_bytes(url, MAX_JSON_BYTES)?;
    serde_json::from_slice(&body)
        .map_err(|e| Error::BadResponse { url: display_url(url), message: format!("not the expected JSON: {e}") })
}

pub(crate) fn github_latest(http: &Http, api_root: &str, platform: Platform) -> Result<Release> {
    let url = releases_url(api_root, "/latest");
    release_from_raw(fetch_json(http, &url)?, platform, None)
}

pub(crate) fn github_by_tag(http: &Http, api_root: &str, platform: Platform, tag: &str) -> Result<Release> {
    let valid = !tag.is_empty()
        && tag.len() <= 100
        && !tag.starts_with(['.', '-'])
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'));
    if !valid {
        return Err(Error::InvalidInput { what: "release tag", value: tag.to_owned() });
    }
    let url = releases_url(api_root, &format!("/tags/{tag}"));
    release_from_raw(fetch_json(http, &url)?, platform, None)
}

/// The release that ships `clearcote-<version>-<suffix>`: the latest one if it does, else the first of the
/// most recent [`SEARCH_RELEASES`] that does.
pub(crate) fn github_by_version(http: &Http, api_root: &str, platform: Platform, version: &str) -> Result<Release> {
    match github_latest(http, api_root, platform) {
        Ok(r) if r.version == version => return Ok(r),
        Ok(_) | Err(Error::NoArchive { .. } | Error::MissingAsset { .. }) => {}
        Err(e) => return Err(e),
    }
    let url = releases_url(api_root, &format!("?per_page={SEARCH_RELEASES}"));
    let all: Vec<RawRelease> = fetch_json(http, &url)?;
    all.into_iter().filter_map(|r| release_from_raw(r, platform, Some(version)).ok()).next().ok_or_else(|| {
        Error::MissingAsset {
            tag: format!("(the latest {SEARCH_RELEASES} releases)"),
            name: platform.asset_name(version),
        }
    })
}

/// Build a [`Release`] from the API's JSON: the platform's archive (the one of version `want`, else the newest) and
/// the checksum files. Download URLs must live on github.com.
fn release_from_raw(raw: RawRelease, platform: Platform, want: Option<&str>) -> Result<Release> {
    let tag = raw.tag_name;
    if raw.draft {
        return Err(Error::InvalidInput { what: "release (it is a draft)", value: tag });
    }
    let usable = |a: &&RawAsset| a.state.as_deref().is_none_or(|s| s == "uploaded");
    let find = |name: &str| raw.assets.iter().filter(usable).find(|a| a.name == name);
    let missing = |name: &str| Error::MissingAsset { tag: tag.clone(), name: name.to_owned() };
    let checked = |a: &RawAsset| -> Result<String> {
        if a.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
            Ok(a.browser_download_url.clone())
        } else {
            Err(Error::BadResponse {
                url: display_url(&a.browser_download_url),
                message: format!("asset `{}` is not hosted on {DOWNLOAD_PREFIX}", a.name),
            })
        }
    };

    let mut best: Option<(String, &RawAsset)> = None;
    for a in raw.assets.iter().filter(usable) {
        let Some(version) = platform.version_of_asset(&a.name) else { continue };
        if want.is_some_and(|w| w != version) {
            continue;
        }
        if best.as_ref().is_none_or(|(v, _)| crate::version::compare_versions(&version, v).is_gt()) {
            best = Some((version, a));
        }
    }
    let Some((version, archive)) = best else {
        return Err(missing(&match want {
            Some(v) => platform.asset_name(v),
            None => format!("clearcote-<version>-{}", platform.asset_suffix()),
        }));
    };
    let sums = find(SUMS_NAME).ok_or_else(|| missing(SUMS_NAME))?;
    let sig = find(SIG_NAME).ok_or_else(|| missing(SIG_NAME))?;
    Ok(Release {
        tag: tag.clone(),
        version,
        platform,
        asset: Asset { name: archive.name.clone(), url: checked(archive)?, size: Some(archive.size) },
        sums_url: checked(sums)?,
        sig_url: checked(sig)?,
        key_url: find(KEY_NAME).and_then(|k| checked(k).ok()),
        page_url: raw.html_url.filter(|u| u.starts_with("https://")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> RawRelease {
        serde_json::from_str(json).unwrap()
    }

    fn asset(name: &str, size: u64, host: &str) -> String {
        format!(
            r#"{{"name":"{name}","size":{size},"state":"uploaded","browser_download_url":"{host}/clearcote/{name}"}}"#
        )
    }

    fn release_json(assets: &[String], draft: bool) -> String {
        format!(
            r#"{{"tag_name":"v0.1.0-pre.25","draft":{draft},"prerelease":true,"html_url":"https://github.com/x/y/releases/tag/v0.1.0-pre.25","assets":[{}]}}"#,
            assets.join(",")
        )
    }

    const GH: &str = "https://github.com/clearcotelabs/clearcote-browser/releases/download/v0.1.0-pre.25";

    fn full_assets() -> Vec<String> {
        vec![
            asset("clearcote-150.9.1-linux-x64.tar.xz", 5, GH),
            asset("clearcote-151.0.7922.108-linux-x64.tar.xz", 122_845_968, GH),
            asset("clearcote-151.0.7922.108-linux-x64.tar.xz.sha256", 100, GH),
            asset("clearcote-151.0.7922.108-windows-x64.zip", 7, GH),
            asset("SHA256SUMS.txt", 1, GH),
            asset("SHA256SUMS.txt.asc", 1, GH),
            asset("clearcote-signing-key.asc", 1, GH),
        ]
    }

    #[test]
    fn picks_the_platform_archive_and_the_checksum_files() {
        let r = release_from_raw(raw(&release_json(&full_assets(), false)), Platform::LinuxX64, None).unwrap();
        assert_eq!(r.tag, "v0.1.0-pre.25");
        assert_eq!(r.version, "151.0.7922.108");
        assert_eq!(r.asset.name, "clearcote-151.0.7922.108-linux-x64.tar.xz");
        assert_eq!(r.asset.size, Some(122_845_968));
        assert!(r.asset.url.ends_with("/clearcote-151.0.7922.108-linux-x64.tar.xz"));
        assert!(r.sums_url.ends_with("/SHA256SUMS.txt") && r.sig_url.ends_with("/SHA256SUMS.txt.asc"));
        assert!(r.key_url.unwrap().ends_with("/clearcote-signing-key.asc"));

        let w = release_from_raw(raw(&release_json(&full_assets(), false)), Platform::WindowsX64, None).unwrap();
        assert_eq!(w.asset.name, "clearcote-151.0.7922.108-windows-x64.zip");

        let old =
            release_from_raw(raw(&release_json(&full_assets(), false)), Platform::LinuxX64, Some("150.9.1")).unwrap();
        assert_eq!(old.version, "150.9.1");
    }

    #[test]
    fn refuses_what_it_cannot_use() {
        let json = |assets: &[String], draft| raw(&release_json(assets, draft));
        // draft
        assert!(matches!(
            release_from_raw(json(&full_assets(), true), Platform::LinuxX64, None),
            Err(Error::InvalidInput { .. })
        ));
        // no archive for the asked version / platform
        let e = release_from_raw(json(&full_assets(), false), Platform::LinuxX64, Some("1.2.3")).unwrap_err();
        assert!(matches!(&e, Error::MissingAsset { name, .. } if name == "clearcote-1.2.3-linux-x64.tar.xz"), "{e}");
        // missing checksum files
        let only_archive = vec![asset("clearcote-151.0.7922.108-linux-x64.tar.xz", 1, GH)];
        let e = release_from_raw(json(&only_archive, false), Platform::LinuxX64, None).unwrap_err();
        assert!(matches!(&e, Error::MissingAsset { name, .. } if name == SUMS_NAME), "{e}");
        // a download host that is not github.com
        let mut evil = full_assets();
        evil[1] = asset("clearcote-151.0.7922.108-linux-x64.tar.xz", 1, "https://evil.example/x");
        let e = release_from_raw(json(&evil, false), Platform::LinuxX64, None).unwrap_err();
        assert!(matches!(e, Error::BadResponse { .. }), "{e}");
        // assets that are not fully uploaded do not count
        let pending = full_assets().join(",").replace(r#""state":"uploaded""#, r#""state":"starter""#);
        let r = raw(&format!(r#"{{"tag_name":"t","assets":[{pending}]}}"#));
        assert!(release_from_raw(r, Platform::LinuxX64, None).is_err());
    }

    #[test]
    fn mirror_base_is_validated_and_trimmed() {
        let with = |u: &str| InstallOptions { base_url: Some(u.to_owned()), ..InstallOptions::default() };
        assert_eq!(mirror_base(&InstallOptions::default()).unwrap(), None);
        assert_eq!(mirror_base(&with("")).unwrap(), None);
        assert_eq!(mirror_base(&with("http://127.0.0.1:8765/")).unwrap().as_deref(), Some("http://127.0.0.1:8765"));
        assert_eq!(mirror_base(&with(" HTTPS://m.example/a//")).unwrap().as_deref(), Some("HTTPS://m.example/a"));
        for bad in ["ftp://x/", "file:///etc", "m.example/kernels", "http://a b/"] {
            assert!(matches!(mirror_base(&with(bad)), Err(Error::InvalidInput { .. })), "{bad}");
        }
        // credentials in a bad URL never reach the message
        let e = mirror_base(&with("ftp://user:secret@host/")).unwrap_err().to_string();
        assert!(!e.contains("secret"), "{e}");
    }

    #[test]
    fn api_urls() {
        assert_eq!(
            releases_url("https://api.github.com/", "/latest"),
            "https://api.github.com/repos/clearcotelabs/clearcote-browser/releases/latest"
        );
    }
}
