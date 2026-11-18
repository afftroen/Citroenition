//! Plugins from GitHub: a repository link -> the newest commit of its branch -> that commit's files.
//!
//! Accepted links: `https://github.com/owner/repo`, `github.com/owner/repo`, `owner/repo`, with `.git`, and
//! `…/tree/<branch-or-tag>/<sub/folder>` for a plugin in a folder of a bigger repository. The files are fetched for one
//! exact commit, so what is installed is what was looked at; an update is a new commit, fetched only when asked.

use crate::error::{Error, Result};
use crate::extract::{self, Limits};
use crate::http::Http;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubSource {
    pub owner: String,
    pub repo: String,
    /// Branch or tag; `None` = the default branch.
    pub git_ref: Option<String>,
    /// Folder inside the repository; empty = the top.
    pub subdir: String,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && s != "."
        && s != ".."
}

impl GithubSource {
    pub fn parse(link: &str) -> Result<GithubSource> {
        let bad = || Error::InvalidInput { what: "GitHub link", value: link.to_owned() };
        let t = link.trim().trim_end_matches('/');
        let rest = t
            .strip_prefix("https://")
            .or_else(|| t.strip_prefix("http://"))
            .unwrap_or(t)
            .strip_prefix("www.")
            .unwrap_or_else(|| t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t));
        let rest = rest.strip_prefix("github.com/").unwrap_or(rest);
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() < 2 {
            return Err(bad());
        }
        let owner = parts[0].to_owned();
        let repo = parts[1].trim_end_matches(".git").to_owned();
        if !name_ok(&owner) || !name_ok(&repo) || owner.contains('.') && owner.starts_with('.') {
            return Err(bad());
        }
        let (git_ref, subdir) = match parts.get(2) {
            None => (None, String::new()),
            Some(&"tree") | Some(&"blob") => {
                let r = parts.get(3).ok_or_else(bad)?;
                let sub: Vec<&str> = parts[4..].to_vec();
                if sub.iter().any(|p| *p == ".." || *p == ".") {
                    return Err(bad());
                }
                (Some((*r).to_owned()), sub.join("/"))
            }
            Some(_) => return Err(bad()),
        };
        if git_ref.as_deref().is_some_and(|r| !r.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))) {
            return Err(bad());
        }
        Ok(GithubSource { owner, repo, git_ref, subdir })
    }

    /// The link in its plain form (what a plugin's `source` records).
    pub fn link(&self) -> String {
        let mut s = format!("https://github.com/{}/{}", self.owner, self.repo);
        if let Some(r) = &self.git_ref {
            s.push_str(&format!("/tree/{r}"));
            if !self.subdir.is_empty() {
                s.push('/');
                s.push_str(&self.subdir);
            }
        }
        s
    }

    /// GitHub's page with every change between two commits.
    pub fn compare_url(&self, old: &str, new: &str) -> String {
        format!("https://github.com/{}/{}/compare/{old}...{new}", self.owner, self.repo)
    }
}

/// The newest commit of the link's branch (or its default branch).
pub fn latest_commit(src: &GithubSource, timeout: Duration) -> Result<String> {
    let r = src.git_ref.as_deref().unwrap_or("HEAD");
    let url = format!("https://api.github.com/repos/{}/{}/commits/{r}", src.owner, src.repo);
    let body = Http::new(timeout).get_bytes(&url, 4 << 20)?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Error::BadArchive(e.to_string()))?;
    let sha = v["sha"].as_str().unwrap_or("");
    if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha.to_owned())
    } else {
        Err(Error::BadArchive(format!("GitHub sent no commit for {}", src.link())))
    }
}

/// The files of `commit`, unpacked under `work`; returns the plugin's folder (the link's sub folder).
pub fn download(src: &GithubSource, commit: &str, work: &Path, timeout: Duration) -> Result<PathBuf> {
    let url = format!("https://codeload.github.com/{}/{}/tar.gz/{commit}", src.owner, src.repo);
    let bytes = Http::new(timeout).get_bytes(&url, 200 << 20)?;
    let unpacked = work.join("unpacked");
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked).map_err(|e| Error::BadArchive(e.to_string()))?;
    let limits = Limits { max_total_bytes: 500 << 20, max_entries: 20_000 };
    extract::extract_tar(flate2::read::GzDecoder::new(&bytes[..]), &unpacked, limits)?;
    // the archive has one top folder, `<repo>-<commit>`
    let top = fs::read_dir(&unpacked)
        .map_err(|e| Error::BadArchive(e.to_string()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or_else(|| Error::BadArchive("the GitHub archive is empty".into()))?;
    Ok(if src.subdir.is_empty() { top } else { top.join(&src.subdir) })
}

/// A plugin given as a local folder, `.zip`, `.tar.gz` or `.tgz`: the folder to install from (archives are unpacked
/// under `work`).
pub fn unpack_local(path: &Path, work: &Path) -> Result<PathBuf> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let unpacked = work.join("unpacked");
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked).map_err(|e| Error::BadArchive(e.to_string()))?;
    let limits = Limits { max_total_bytes: 500 << 20, max_entries: 20_000 };
    if name.ends_with(".zip") {
        extract::extract_zip(path, &unpacked, limits, extract::Naming::Posix)?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let f = fs::File::open(path).map_err(|e| Error::BadArchive(format!("cannot open {}: {e}", path.display())))?;
        extract::extract_tar(flate2::read::GzDecoder::new(std::io::BufReader::new(f)), &unpacked, limits)?;
    } else {
        return Err(Error::InvalidInput {
            what: "plugin file (a folder, .zip, .tar.gz or .tgz)",
            value: path.display().to_string(),
        });
    }
    Ok(unpacked)
}

/// A plugin fetched for installing: its files, where it came from and the commit it is pinned to (empty for a
/// local plugin).
#[derive(Debug, Clone)]
pub struct Fetched {
    pub dir: PathBuf,
    pub source: String,
    pub commit: String,
}

/// Get a plugin from a GitHub link (its newest commit) or a local folder / archive, into `work`.
pub fn fetch(input: &str, work: &Path, timeout: Duration) -> Result<Fetched> {
    let t = input.trim().trim_matches('"');
    let local = Path::new(t);
    if local.is_absolute() && local.exists() {
        return Ok(Fetched { dir: unpack_local(local, work)?, source: t.to_owned(), commit: String::new() });
    }
    let src = GithubSource::parse(t)?;
    let commit = latest_commit(&src, timeout)?;
    Ok(Fetched { dir: download(&src, &commit, work, timeout)?, source: src.link(), commit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_in_every_usual_shape() {
        let p = |s: &str| GithubSource::parse(s).unwrap();
        let plain =
            GithubSource { owner: "acme".into(), repo: "tiktok-pack".into(), git_ref: None, subdir: String::new() };
        for s in [
            "https://github.com/acme/tiktok-pack",
            "github.com/acme/tiktok-pack/",
            "acme/tiktok-pack",
            "https://github.com/acme/tiktok-pack.git",
            "https://www.github.com/acme/tiktok-pack",
        ] {
            assert_eq!(p(s), plain, "{s}");
        }
        let sub = p("https://github.com/acme/plugins/tree/main/tiktok/pack");
        assert_eq!((sub.git_ref.as_deref(), sub.subdir.as_str()), (Some("main"), "tiktok/pack"));
        assert_eq!(sub.link(), "https://github.com/acme/plugins/tree/main/tiktok/pack");
        assert_eq!(plain.compare_url("a1", "b2"), "https://github.com/acme/tiktok-pack/compare/a1...b2");
        for bad in [
            "",
            "acme",
            "https://github.com/acme/x/issues",
            "https://github.com/acme/x/tree/main/../../etc",
            "acme/x y",
            "a/b/tree",
        ] {
            assert!(GithubSource::parse(bad).is_err(), "{bad}");
        }
    }
}
