//! Bookmarks a profile (and its group) puts on the browser's bookmarks bar.
//!
//! Before a launch, [`apply`] writes them into the profile's Chromium `Default/Bookmarks` file (the group's first, then
//! the profile's) and turns the bookmarks bar on. Only bookmarks Citroenition added are ever removed or replaced
//! (`bookmarks-applied.json` in the profile folder remembers them), so the ones the user adds in the browser stay.
//! The file is written without a `checksum`, which Chromium then does not check.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub name: String,
    pub url: String,
}

/// `Name | https://…` or a bare URL per line; lines without a usable URL are left out.
pub fn parse_lines(text: &str) -> Vec<Bookmark> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            let (name, url) = match l.rsplit_once('|') {
                Some((n, u)) => (n.trim(), u.trim()),
                None => ("", l),
            };
            let url = if url.contains("://") || url.starts_with("about:") || url.starts_with("chrome:") {
                url.to_owned()
            } else if url.contains('.') && !url.contains(' ') {
                format!("https://{url}")
            } else {
                return None;
            };
            let name = if name.is_empty() {
                url.split("://").nth(1).unwrap_or(&url).trim_start_matches("www.").trim_end_matches('/').to_owned()
            } else {
                name.to_owned()
            };
            Some(Bookmark { name, url })
        })
        .collect()
}

pub fn to_lines(list: &[Bookmark]) -> String {
    list.iter().map(|b| format!("{} | {}", b.name, b.url)).collect::<Vec<_>>().join("\n")
}

fn chrome_now() -> String {
    // microseconds since 1601-01-01
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
    (unix + 11_644_473_600_000_000).to_string()
}

fn guid() -> String {
    let mut r = crate::rng::Rng::from_entropy();
    let h = |n: usize, r: &mut crate::rng::Rng| r.token(n, b"0123456789abcdef");
    let variant = ["8", "9", "a", "b"][r.range(0, 4) as usize];
    format!("{}-{}-4{}-{variant}{}-{}", h(8, &mut r), h(4, &mut r), h(3, &mut r), h(3, &mut r), h(12, &mut r))
}

fn root(name: &str, id: &str, guid: &str) -> Value {
    json!({ "children": [], "date_added": chrome_now(), "date_last_used": "0", "date_modified": "0", "guid": guid,
            "id": id, "name": name, "type": "folder" })
}

fn max_id(v: &Value) -> u64 {
    let own = v.get("id").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0);
    v.get("children").and_then(Value::as_array).map_or(own, |c| c.iter().map(max_id).fold(own, u64::max))
}

/// Put `wanted` on the bookmarks bar of the browser profile in `data_dir`; `marker` remembers what was added.
/// Returns `false` when nothing had to change.
pub fn apply(data_dir: &Path, marker: &Path, wanted: &[Bookmark]) -> Result<bool> {
    let before: Vec<Bookmark> = fs::read(marker).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if before == wanted {
        return Ok(false);
    }
    let path = data_dir.join("Default/Bookmarks");
    let mut doc: Value =
        fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    if !doc.is_object() {
        doc = json!({});
    }
    let roots = doc.as_object_mut().expect("object").entry("roots").or_insert_with(|| json!({}));
    if !roots.is_object() {
        *roots = json!({});
    }
    let r = roots.as_object_mut().expect("object");
    r.entry("bookmark_bar").or_insert_with(|| root("Bookmarks bar", "1", "0bc5d13f-2cba-5d74-951f-3f233fe6c908"));
    r.entry("other").or_insert_with(|| root("Other bookmarks", "2", "82b081ec-3dd3-529c-8475-ab6c344590dd"));
    r.entry("synced").or_insert_with(|| root("Mobile bookmarks", "3", "4cf2e351-0e85-532b-bb37-df045d8f8d0f"));
    let mut next = roots_max(r) + 1;
    let bar = r.get_mut("bookmark_bar").expect("present");
    let children = bar.as_object_mut().expect("object").entry("children").or_insert_with(|| json!([]));
    if !children.is_array() {
        *children = json!([]);
    }
    let list = children.as_array_mut().expect("array");
    // drop what Citroenition put there last time, keep everything the user added
    list.retain(|c| !before.iter().any(|b| c.get("url").and_then(Value::as_str) == Some(b.url.as_str())));
    let mut added = Vec::new();
    for b in wanted {
        if list.iter().any(|c| c.get("url").and_then(Value::as_str) == Some(b.url.as_str())) {
            continue; // the user has it already
        }
        added.push(json!({ "date_added": chrome_now(), "date_last_used": "0", "guid": guid(), "id": next.to_string(),
                           "name": b.name, "type": "url", "url": b.url }));
        next += 1;
    }
    let n = added.len();
    list.splice(0..0, added);
    let o = doc.as_object_mut().expect("object");
    o.remove("checksum");
    o.insert("version".into(), json!(1));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
    // show the bar
    if n > 0 {
        let prefs_path = data_dir.join("Default/Preferences");
        let mut prefs: Map<String, Value> =
            fs::read(&prefs_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let bb = prefs.entry("bookmark_bar").or_insert_with(|| json!({}));
        if !bb.is_object() {
            *bb = json!({});
        }
        bb["show_on_all_tabs"] = json!(true);
        fs::write(&prefs_path, serde_json::to_vec(&prefs)?)?;
    }
    fs::write(marker, serde_json::to_vec(wanted)?)?;
    Ok(true)
}

fn roots_max(r: &Map<String, Value>) -> u64 {
    r.values().map(max_id).max().unwrap_or(3).max(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let b = parse_lines("TikTok Ads | https://ads.tiktok.com/\nwww.example.com/\n\nnot a url\n | about:blank");
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].name.as_str(), b[0].url.as_str()), ("TikTok Ads", "https://ads.tiktok.com/"));
        assert_eq!((b[1].name.as_str(), b[1].url.as_str()), ("example.com", "https://www.example.com/"));
        assert_eq!(parse_lines(&to_lines(&b)), b);
    }

    #[test]
    fn only_our_own_bookmarks_are_replaced() {
        let t = std::env::temp_dir().join(format!("citro-bm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let (data, marker) = (t.join("data"), t.join("bookmarks-applied.json"));
        fs::create_dir_all(&t).unwrap();
        let a = parse_lines("A | https://a.example/\nB | https://b.example/");
        assert!(apply(&data, &marker, &a).unwrap());
        assert!(!apply(&data, &marker, &a).unwrap(), "unchanged: the file is left alone");
        // the user adds one in the browser
        let path = data.join("Default/Bookmarks");
        let mut doc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        doc["roots"]["bookmark_bar"]["children"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"99","guid":"x","name":"Mine","type":"url","url":"https://mine.example/"}));
        fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        // the profile's list changes: B goes, C comes, the user's stays
        assert!(apply(&data, &marker, &parse_lines("A | https://a.example/\nC | https://c.example/")).unwrap());
        let doc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let urls: Vec<&str> = doc["roots"]["bookmark_bar"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls, ["https://a.example/", "https://c.example/", "https://mine.example/"]);
        let ids: std::collections::HashSet<&str> = doc["roots"]["bookmark_bar"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 3, "ids are unique");
        assert!(doc.get("checksum").is_none());
        let prefs: Value = serde_json::from_slice(&fs::read(data.join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(prefs["bookmark_bar"]["show_on_all_tabs"], true);
        let _ = fs::remove_dir_all(&t);
    }
}
