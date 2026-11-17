//! Browser extensions: unpacked Chrome extensions under `<home>/extensions/<id>/`, and which profiles load them.
//!
//! An extension is loaded into **every** profile (`all_profiles`) or into the profiles of the groups it lists, like
//! The antidetect browser's team extensions. The launcher passes the folders as `--load-extension`. Downloading one from the Chrome
//! Web Store is `citro_install::chrome_store`; this module only keeps the list.
//!
//! `<home>/extensions/extensions.json` holds the list; each extension's files are in `<home>/extensions/<id>/`.

use crate::error::Result;
use crate::store::Home;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extension {
    /// The Chrome Web Store id (32 letters a-p).
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// Loaded into every profile.
    pub all_profiles: bool,
    /// Loaded into the profiles of these groups (when not `all_profiles`).
    pub groups: Vec<String>,
    /// Unix seconds.
    pub added: i64,
}

impl Extension {
    pub fn applies_to(&self, group_id: &str) -> bool {
        self.all_profiles || (!group_id.is_empty() && self.groups.iter().any(|g| g == group_id))
    }
}

pub fn root(home: &Home) -> PathBuf {
    home.root.join("extensions")
}

pub fn dir(home: &Home, id: &str) -> PathBuf {
    root(home).join(id)
}

fn index(home: &Home) -> PathBuf {
    root(home).join("extensions.json")
}

/// A Chrome Web Store id: 32 letters `a`-`p`.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// The id in a Chrome Web Store link (`https://chromewebstore.google.com/detail/<name>/<id>`) or a bare id.
pub fn id_from_input(input: &str) -> Option<String> {
    let t = input.trim();
    if valid_id(t) {
        return Some(t.to_owned());
    }
    t.split(['/', '?', '#', '&', '=']).find(|part| valid_id(part)).map(str::to_owned)
}

pub fn list(home: &Home) -> Vec<Extension> {
    fs::read(index(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(home: &Home, list: &[Extension]) -> Result<()> {
    fs::create_dir_all(root(home))?;
    let tmp = index(home).with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
    fs::rename(&tmp, index(home))?;
    Ok(())
}

/// The folders to load for a profile in `group_id` (only extensions whose files are present).
pub fn folders_for(home: &Home, group_id: &str) -> Vec<PathBuf> {
    list(home)
        .into_iter()
        .filter(|e| e.applies_to(group_id))
        .map(|e| dir(home, &e.id))
        .filter(|d| d.join("manifest.json").is_file())
        .collect()
}

/// A message from `_locales/<locale>/messages.json` for a `__MSG_name__` manifest value.
fn localized(folder: &Path, manifest: &Value, value: &str) -> String {
    let Some(key) = value.strip_prefix("__MSG_").and_then(|v| v.strip_suffix("__")) else { return value.to_owned() };
    let locale = manifest.get("default_locale").and_then(Value::as_str).unwrap_or("en");
    for loc in [locale, "en", "en_US"] {
        let path = folder.join("_locales").join(loc).join("messages.json");
        if let Some(msgs) = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            && let Some(obj) = msgs.as_object()
            && let Some((_, v)) = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))
            && let Some(m) = v.get("message").and_then(Value::as_str)
        {
            return m.to_owned();
        }
    }
    key.to_owned()
}

/// Read name, version and description from an unpacked extension.
pub fn describe(folder: &Path) -> Result<(String, String, String)> {
    let manifest: Value = serde_json::from_slice(&fs::read(folder.join("manifest.json"))?)?;
    let s = |k: &str| manifest.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let name = localized(folder, &manifest, &s("name"));
    let desc = localized(folder, &manifest, &s("description"));
    Ok((name, s("version"), desc))
}

/// Move the unpacked extension in `unpacked` into place as `id` (replacing an older version, keeping its
/// profile assignment) and record it.
pub fn install(home: &Home, id: &str, unpacked: &Path, now: i64) -> Result<Extension> {
    if !valid_id(id) {
        return Err(crate::Error::Invalid(format!("`{id}` is not a Chrome Web Store extension id")));
    }
    let (name, version, description) = describe(unpacked)?;
    let target = dir(home, id);
    fs::create_dir_all(root(home))?;
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    if fs::rename(unpacked, &target).is_err() {
        copy_dir(unpacked, &target)?;
        let _ = fs::remove_dir_all(unpacked);
    }
    let mut all = list(home);
    let previous = all.iter().position(|e| e.id == id).map(|i| all.remove(i));
    let ext = Extension {
        id: id.to_owned(),
        name,
        version,
        description,
        all_profiles: previous.as_ref().is_none_or(|p| p.all_profiles),
        groups: previous.map(|p| p.groups).unwrap_or_default(),
        added: now,
    };
    all.push(ext.clone());
    save(home, &all)?;
    Ok(ext)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let t = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &t)?;
        } else {
            fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

/// Remove an extension and its files.
pub fn remove(home: &Home, id: &str) -> Result<()> {
    let mut all = list(home);
    all.retain(|e| e.id != id);
    save(home, &all)?;
    if valid_id(id) {
        let _ = fs::remove_dir_all(dir(home, id));
    }
    Ok(())
}

/// Change which profiles load `id`.
pub fn assign(home: &Home, id: &str, all_profiles: bool, groups: Vec<String>) -> Result<()> {
    let mut all = list(home);
    if let Some(e) = all.iter_mut().find(|e| e.id == id) {
        e.all_profiles = all_profiles;
        e.groups = groups;
    }
    save(home, &all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("citro-ext-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    const ID: &str = "cjpalhdlnbpafiamejdnhcphjbkeiagm";

    #[test]
    fn ids_from_store_links() {
        assert_eq!(id_from_input(ID).as_deref(), Some(ID));
        let url = format!("https://chromewebstore.google.com/detail/ublock-origin/{ID}?hl=en");
        assert_eq!(id_from_input(&url).as_deref(), Some(ID));
        assert_eq!(id_from_input("https://chromewebstore.google.com/category/extensions"), None);
        assert!(!valid_id("cjpalhdlnbpafiamejdnhcphjbkeiagz"), "letters stop at p");
    }

    #[test]
    fn install_describe_assign_and_remove() {
        let h = home("lifecycle");
        let src = h.root.join("tmp-unpacked");
        fs::create_dir_all(src.join("_locales/en")).unwrap();
        fs::write(
            src.join("manifest.json"),
            r#"{"name":"__MSG_extName__","version":"1.2.3","default_locale":"en","description":"plain"}"#,
        )
        .unwrap();
        fs::write(src.join("_locales/en/messages.json"), r#"{"extName":{"message":"uBlock Origin"}}"#).unwrap();
        let e = install(&h, ID, &src, 7).unwrap();
        assert_eq!((e.name.as_str(), e.version.as_str(), e.all_profiles), ("uBlock Origin", "1.2.3", true));
        assert!(dir(&h, ID).join("manifest.json").is_file() && !src.exists());
        assert_eq!(folders_for(&h, ""), [dir(&h, ID)]);
        assign(&h, ID, false, vec!["3".into()]).unwrap();
        assert!(folders_for(&h, "").is_empty() && folders_for(&h, "2").is_empty());
        assert_eq!(folders_for(&h, "3").len(), 1);
        // a reinstall (update) keeps the assignment
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("manifest.json"), r#"{"name":"uBO","version":"1.3"}"#).unwrap();
        let e = install(&h, ID, &src, 8).unwrap();
        assert_eq!((e.version.as_str(), e.all_profiles, e.groups.as_slice()), ("1.3", false, &["3".to_owned()][..]));
        remove(&h, ID).unwrap();
        assert!(list(&h).is_empty() && !dir(&h, ID).exists());
        let _ = fs::remove_dir_all(&h.root);
    }
}
