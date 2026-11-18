//! Plugins (new, in progress): a folder with a `citroenition-plugin.json` that adds things to the profiles of the
//! groups it is switched on for:
//!
//! * **fields**: values kept per profile (an account e-mail, a 2FA secret, an ad account id, a status), shown in the
//!   profile editor and stored in the profile itself (`Profile::plugin_data`), so team sync carries them;
//! * **start pages** and **bookmarks**, added to the profile's own;
//! * **extensions**: unpacked Chrome extensions inside the plugin, loaded into the browser.
//!
//! A plugin comes from a GitHub repository (pinned to one commit; updating is a deliberate step) or from a local
//! folder or archive. Nobody reviews plugins: an extension in one runs inside the browser and can read the sites it
//! is allowed on. Plugins cannot run programs on this computer (yet).
//!
//! `<home>/plugins/plugins.json` lists the installed plugins; each one's files are in `<home>/plugins/<id>/`.

use crate::bookmarks::Bookmark;
use crate::error::{Error, Result};
use crate::store::Home;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const MANIFEST: &str = "citroenition-plugin.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    #[default]
    Text,
    /// Shown as dots until revealed (passwords, 2FA secrets, API keys).
    Secret,
    /// Several lines.
    Note,
    /// One of `options`.
    Select,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    /// Letters, digits and `_`; unique in the plugin.
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    pub options: Vec<String>,
    /// A short help text under the field.
    pub hint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    /// Lowercase letters, digits and `-`: also the folder name.
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub homepage: String,
    pub fields: Vec<Field>,
    pub start_pages: Vec<String>,
    pub bookmarks: Vec<Bookmark>,
    /// Folders inside the plugin, each an unpacked Chrome extension (with its `manifest.json`).
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugin {
    pub manifest: Manifest,
    /// `https://github.com/owner/repo[/tree/ref/sub/dir]`, or the local path it was installed from.
    pub source: String,
    /// The pinned commit (GitHub); empty for a local install.
    pub commit: String,
    /// Switched on for every profile, or only for the profiles of `groups`.
    pub all_profiles: bool,
    pub groups: Vec<String>,
    /// Unix seconds.
    pub installed: i64,
    pub updated: i64,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn applies_to(&self, group_id: &str) -> bool {
        self.all_profiles || (!group_id.is_empty() && self.groups.iter().any(|g| g == group_id))
    }

    pub fn is_github(&self) -> bool {
        self.source.starts_with("https://github.com/")
    }
}

/// What the plugins switched on for a group add to a launch.
#[derive(Debug, Default)]
pub struct Applied {
    pub extensions: Vec<PathBuf>,
    pub start_pages: Vec<String>,
    pub bookmarks: Vec<Bookmark>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn root(home: &Home) -> PathBuf {
    home.root.join("plugins")
}

fn registry(home: &Home) -> PathBuf {
    root(home).join("plugins.json")
}

pub fn dir(home: &Home, id: &str) -> PathBuf {
    root(home).join(id)
}

pub fn list(home: &Home) -> Vec<Plugin> {
    fs::read(registry(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(home: &Home, list: &[Plugin]) -> Result<()> {
    fs::create_dir_all(root(home))?;
    let tmp = registry(home).with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
    fs::rename(&tmp, registry(home))?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// A path inside the plugin: relative, no `..`.
fn inside(rel: &str) -> bool {
    let p = Path::new(rel);
    !rel.trim().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// The folder holding the manifest: `dir` itself, or its only subfolder (a GitHub archive has one top folder).
pub fn find_root(dir: &Path) -> Option<PathBuf> {
    if dir.join(MANIFEST).is_file() {
        return Some(dir.to_path_buf());
    }
    let subs: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    match subs.as_slice() {
        [one] if one.join(MANIFEST).is_file() => Some(one.clone()),
        _ => None,
    }
}

/// Read and check a plugin's manifest. Every problem is named, so a plugin author can fix it.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let bad = |m: String| Error::Invalid(format!("{MANIFEST}: {m}"));
    let text = fs::read(dir.join(MANIFEST)).map_err(|_| bad(format!("not found in {}", dir.display())))?;
    let mut m: Manifest = serde_json::from_slice(&text).map_err(|e| bad(e.to_string()))?;
    m.id = m.id.trim().to_owned();
    if !valid_id(&m.id) {
        return Err(bad(format!("`id` must be 1-40 lowercase letters, digits or -, not `{}`", m.id)));
    }
    if m.name.trim().is_empty() {
        return Err(bad("`name` is empty".into()));
    }
    let mut keys = std::collections::HashSet::new();
    for f in &m.fields {
        if f.key.is_empty() || !f.key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(bad(format!("field key `{}` must be letters, digits or _", f.key)));
        }
        if !keys.insert(f.key.clone()) {
            return Err(bad(format!("field key `{}` is used twice", f.key)));
        }
        if f.kind == FieldKind::Select && f.options.is_empty() {
            return Err(bad(format!("field `{}` is a select without options", f.key)));
        }
    }
    for u in m.start_pages.iter().chain(m.bookmarks.iter().map(|b| &b.url)) {
        if !(u.starts_with("https://") || u.starts_with("http://")) {
            return Err(bad(format!("`{u}` is not an http(s) address")));
        }
    }
    for e in &m.extensions {
        if !inside(e) {
            return Err(bad(format!("extension folder `{e}` must be a path inside the plugin")));
        }
        if !dir.join(e).join("manifest.json").is_file() {
            return Err(bad(format!("extension folder `{e}` has no manifest.json")));
        }
    }
    Ok(m)
}

/// Copy regular files and folders only: a symbolic link in a downloaded plugin could point anywhere on this computer.
fn copy_plain(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let t = e.file_type()?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if t.is_dir() {
            copy_plain(&src, &dst)?;
        } else if t.is_file() {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Install (or update) the plugin in `src` (a folder with the manifest). An update keeps where it is switched on.
pub fn install(home: &Home, src: &Path, source: &str, commit: &str) -> Result<Plugin> {
    let m = read_manifest(src)?;
    let id = m.id.clone();
    let staging = root(home).join(format!(".staging-{id}"));
    let _ = fs::remove_dir_all(&staging);
    copy_plain(src, &staging)?;
    let target = dir(home, &id);
    let old = root(home).join(format!(".old-{id}"));
    let _ = fs::remove_dir_all(&old);
    if target.exists() {
        fs::rename(&target, &old)?;
    }
    if let Err(e) = fs::rename(&staging, &target) {
        let _ = fs::rename(&old, &target);
        return Err(e.into());
    }
    let _ = fs::remove_dir_all(&old);
    let mut all = list(home);
    let t = now();
    let p = match all.iter_mut().find(|p| p.id() == id) {
        Some(p) => {
            p.manifest = m;
            p.source = source.to_owned();
            p.commit = commit.to_owned();
            p.updated = t;
            p.clone()
        }
        None => {
            let p = Plugin {
                manifest: m,
                source: source.to_owned(),
                commit: commit.to_owned(),
                all_profiles: true,
                groups: Vec::new(),
                installed: t,
                updated: t,
            };
            all.push(p.clone());
            p
        }
    };
    save(home, &all)?;
    Ok(p)
}

/// Remove the plugin and its files. The values it stored in profiles stay (installing it again brings them back).
pub fn remove(home: &Home, id: &str) -> Result<()> {
    let mut all = list(home);
    all.retain(|p| p.id() != id);
    save(home, &all)?;
    if valid_id(id) {
        let _ = fs::remove_dir_all(dir(home, id));
    }
    Ok(())
}

pub fn set_scope(home: &Home, id: &str, all_profiles: bool, groups: Vec<String>) -> Result<()> {
    let mut all = list(home);
    let p = all.iter_mut().find(|p| p.id() == id).ok_or_else(|| Error::Invalid(format!("no plugin `{id}`")))?;
    p.all_profiles = all_profiles;
    p.groups = groups;
    save(home, &all)
}

/// The plugins switched on for a profile of `group_id`.
pub fn for_group(home: &Home, group_id: &str) -> Vec<Plugin> {
    list(home).into_iter().filter(|p| p.applies_to(group_id)).collect()
}

/// What those plugins add to a launch.
pub fn applied(home: &Home, group_id: &str) -> Applied {
    let mut a = Applied::default();
    for p in for_group(home, group_id) {
        let d = dir(home, p.id());
        for e in &p.manifest.extensions {
            let folder = d.join(e);
            if inside(e) && folder.join("manifest.json").is_file() {
                a.extensions.push(folder);
            }
        }
        a.start_pages.extend(p.manifest.start_pages.iter().cloned());
        a.bookmarks.extend(p.manifest.bookmarks.iter().cloned());
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citro-plugins-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn plugin_dir(base: &Path, manifest: &str) -> PathBuf {
        let d = base.join("src");
        fs::create_dir_all(d.join("ext")).unwrap();
        fs::write(d.join("ext/manifest.json"), r#"{"manifest_version":3,"name":"x","version":"1"}"#).unwrap();
        fs::write(d.join(MANIFEST), manifest).unwrap();
        d
    }

    const GOOD: &str = r#"{"id":"tiktok-pack","name":"TikTok pack","version":"1.0.0",
        "fields":[{"key":"email","label":"E-mail"},{"key":"twofa","label":"2FA","type":"secret"},
                  {"key":"status","label":"Status","type":"select","options":["new","warm"]}],
        "start_pages":["https://ads.tiktok.com"],"bookmarks":[{"name":"Ads","url":"https://ads.tiktok.com"}],
        "extensions":["ext"]}"#;

    #[test]
    fn install_scope_apply_update_remove() {
        let base = tmp("cycle");
        let home = Home { root: base.join("home") };
        let src = plugin_dir(&base, GOOD);
        std::os::unix::fs::symlink("/etc", src.join("escape")).unwrap();
        let p = install(&home, &src, "https://github.com/a/b", "abc123").unwrap();
        assert_eq!((p.id(), p.all_profiles, p.manifest.fields.len()), ("tiktok-pack", true, 3));
        assert!(!dir(&home, "tiktok-pack").join("escape").exists(), "symbolic links are not copied");
        let a = applied(&home, "7");
        assert_eq!((a.extensions.len(), a.start_pages.len(), a.bookmarks.len()), (1, 1, 1));

        set_scope(&home, "tiktok-pack", false, vec!["3".into()]).unwrap();
        assert!(applied(&home, "7").extensions.is_empty());
        assert_eq!(applied(&home, "3").start_pages, ["https://ads.tiktok.com"]);

        // an update keeps the scope and records the new commit
        let p = install(&home, &src, "https://github.com/a/b", "def456").unwrap();
        assert_eq!((p.commit.as_str(), p.all_profiles, p.groups.as_slice()), ("def456", false, &["3".to_owned()][..]));
        assert_eq!(list(&home).len(), 1);

        remove(&home, "tiktok-pack").unwrap();
        assert!(list(&home).is_empty() && !dir(&home, "tiktok-pack").exists());
    }

    #[test]
    fn bad_manifests_are_named() {
        let base = tmp("bad");
        for (json, says) in [
            (r#"{"id":"Bad Id","name":"x"}"#, "`id`"),
            (r#"{"id":"ok","name":""}"#, "`name`"),
            (r#"{"id":"ok","name":"x","fields":[{"key":"a"},{"key":"a"}]}"#, "twice"),
            (r#"{"id":"ok","name":"x","fields":[{"key":"s","type":"select"}]}"#, "without options"),
            (r#"{"id":"ok","name":"x","start_pages":["javascript:alert(1)"]}"#, "http(s)"),
            (r#"{"id":"ok","name":"x","extensions":["../../etc"]}"#, "inside the plugin"),
            (r#"{"id":"ok","name":"x","extensions":["missing"]}"#, "no manifest.json"),
            (r#"{"id":"ok","name":"x","fields":[{"key":"k","type":"script"}]}"#, "unknown variant"),
        ] {
            let d = plugin_dir(&base, json);
            let e = read_manifest(&d).unwrap_err().to_string();
            assert!(e.contains(says), "{json}: {e}");
        }
    }

    #[test]
    fn the_manifest_may_sit_in_the_only_subfolder() {
        let base = tmp("root");
        let top = base.join("unpacked");
        let inner = top.join("owner-repo-abc123");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join(MANIFEST), r#"{"id":"x","name":"x"}"#).unwrap();
        assert_eq!(find_root(&top), Some(inner));
        assert_eq!(find_root(&base), None);
    }
}
