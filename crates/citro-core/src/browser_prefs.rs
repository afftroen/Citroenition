//! Browser preferences Citroenition sets before a launch: the default search engine, and (macOS) no automatic font
//! downloads.
//!
//! **Search engine.** Clearcote ships without Google: the prepopulated engine with Google's id is "No Search"
//! (`http://{searchTerms}`), so typing a search into the address bar goes nowhere. The default engine is the
//! `default_search_provider_data.template_url_data` preference. On macOS (and Windows) Chromium keeps it in the
//! profile's `Secure Preferences` and resets it unless it carries a valid MAC: HMAC-SHA256 with an empty key (an
//! unbranded Chromium has no seed) over `device id + pref path + canonical JSON of the value`, the device id being the
//! machine's `IOPlatformUUID`; a `super_mac` covers all MACs the same way. That is what [`set_default_search`] writes,
//! so the browser takes the engine as if the user had picked it. It is written on the first launch and whenever the
//! choice in Citroenition changes; a change made inside the browser is kept until then. On Linux Chromium does not
//! enforce protected preferences and keeps them in `Preferences`.
//!
//! **Fonts.** On macOS a page that asks for a font macOS can download ("STKaiti", "Osaka") makes the renderer show a
//! "needs to download the font" dialog. [`disable_font_downloads`] turns CoreText's auto-activation off for the
//! kernel's bundle identifiers, so a missing font is simply missing, as on a machine that never downloaded it.

use crate::error::Result;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// A search engine Citroenition can make the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchEngine {
    /// What `Profile::search_engine` stores (`google`).
    pub key: &'static str,
    pub name: &'static str,
    pub keyword: &'static str,
    pub url: &'static str,
    pub suggest: &'static str,
    pub favicon: &'static str,
}

pub const SEARCH_ENGINES: &[SearchEngine] = &[
    SearchEngine {
        key: "google",
        name: "Google",
        keyword: "google.com",
        url: "https://www.google.com/search?q={searchTerms}",
        suggest: "https://www.google.com/complete/search?client=chrome&q={searchTerms}",
        favicon: "https://www.google.com/favicon.ico",
    },
    SearchEngine {
        key: "bing",
        name: "Microsoft Bing",
        keyword: "bing.com",
        url: "https://www.bing.com/search?q={searchTerms}",
        suggest: "https://www.bing.com/osjson.aspx?query={searchTerms}",
        favicon: "https://www.bing.com/favicon.ico",
    },
    SearchEngine {
        key: "duckduckgo",
        name: "DuckDuckGo",
        keyword: "duckduckgo.com",
        url: "https://duckduckgo.com/?q={searchTerms}",
        suggest: "https://duckduckgo.com/ac/?q={searchTerms}&type=list",
        favicon: "https://duckduckgo.com/favicon.ico",
    },
    SearchEngine {
        key: "brave",
        name: "Brave Search",
        keyword: "search.brave.com",
        url: "https://search.brave.com/search?q={searchTerms}",
        suggest: "https://search.brave.com/api/suggest?q={searchTerms}",
        favicon: "https://search.brave.com/favicon.ico",
    },
    SearchEngine {
        key: "yahoo",
        name: "Yahoo",
        keyword: "yahoo.com",
        url: "https://search.yahoo.com/search?p={searchTerms}",
        suggest: "",
        favicon: "https://search.yahoo.com/favicon.ico",
    },
    SearchEngine {
        key: "ecosia",
        name: "Ecosia",
        keyword: "ecosia.org",
        url: "https://www.ecosia.org/search?q={searchTerms}",
        suggest: "https://ac.ecosia.org/autocomplete?q={searchTerms}&type=list",
        favicon: "https://www.ecosia.org/favicon.ico",
    },
    SearchEngine {
        key: "startpage",
        name: "Startpage",
        keyword: "startpage.com",
        url: "https://www.startpage.com/sp/search?query={searchTerms}",
        suggest: "",
        favicon: "https://www.startpage.com/favicon.ico",
    },
];

/// The engine for `key`; empty or unknown = Google. `"browser"` = leave the browser's own choice alone.
pub fn search_engine(key: &str) -> Option<&'static SearchEngine> {
    let key = key.trim().to_ascii_lowercase();
    if key == "browser" {
        return None;
    }
    Some(SEARCH_ENGINES.iter().find(|e| e.key == key).unwrap_or(&SEARCH_ENGINES[0]))
}

fn template(e: &SearchEngine) -> Value {
    // a stable GUID per engine: the same engine in two profiles is the same engine
    let n = SEARCH_ENGINES.iter().position(|x| x.key == e.key).unwrap_or(0) + 1;
    let mut t = json!({
        "short_name": e.name,
        "keyword": e.keyword,
        "url": e.url,
        "favicon_url": e.favicon,
        "safe_for_autoreplace": false,
        "prepopulate_id": 0,
        "synced_guid": format!("c1700000-0000-4000-8000-{n:012}"),
        "input_encodings": ["UTF-8"],
        "id": "0",
        "date_created": "0",
        "last_modified": "0",
        "created_by_policy": 0,
        "enforced_by_policy": false,
        "is_active": 1,
        "starter_pack_id": 0,
    });
    if !e.suggest.is_empty() {
        t["suggestions_url"] = json!(e.suggest);
    }
    t
}

/// Chromium's `PrefHashCalculator` form of a value: sorted keys, no whitespace, empty dictionaries and lists inside
/// dictionaries left out.
fn canonical(v: &Value) -> String {
    fn strip(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut out = Map::new();
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                for k in keys {
                    let s = strip(&m[k]);
                    let empty =
                        matches!(&s, Value::Object(o) if o.is_empty()) || matches!(&s, Value::Array(a) if a.is_empty());
                    if !empty {
                        out.insert(k.clone(), s);
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(strip).collect()),
            other => other.clone(),
        }
    }
    // serde_json's map is ordered by key here (no `preserve_order`), so this is the sorted form
    serde_json::to_string(&strip(v)).unwrap_or_default()
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> String {
    const B: usize = 64;
    let mut k = [0u8; B];
    if key.len() > B {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.iter().map(|b| b ^ x).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    let outer = Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize();
    outer.iter().map(|b| format!("{b:02X}")).collect()
}

fn pref_mac(device: &str, path: &str, v: &Value) -> String {
    hmac_sha256(b"", format!("{device}{path}{}", canonical(v)).as_bytes())
}

/// The machine id Chromium hashes protected preferences with (`IOPlatformUUID`).
#[cfg(target_os = "macos")]
fn device_id() -> Option<String> {
    let out =
        std::process::Command::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.contains("\"IOPlatformUUID\""))
        .and_then(|l| l.rsplit('"').nth(1).map(str::to_owned))
}

fn read_json(path: &Path) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_json(path: &Path, m: &Map<String, Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("citro-tmp");
    fs::write(&tmp, serde_json::to_vec(m)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Make `engine` the default search engine of the browser profile in `data_dir` (the `--user-data-dir`).
pub fn set_default_search(data_dir: &Path, engine: &SearchEngine) -> Result<()> {
    let t = template(engine);
    #[cfg(target_os = "macos")]
    {
        let Some(device) = device_id() else {
            return Err(crate::Error::Invalid("cannot read this Mac's IOPlatformUUID".into()));
        };
        write_secure_prefs(&data_dir.join("Default/Secure Preferences"), &device, t)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let path = data_dir.join("Default/Preferences");
        let mut p = read_json(&path);
        let dsp = p.entry("default_search_provider_data").or_insert_with(|| json!({}));
        if !dsp.is_object() {
            *dsp = json!({});
        }
        dsp["template_url_data"] = t;
        write_json(&path, &p)
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn write_secure_prefs(path: &Path, device: &str, t: Value) -> Result<()> {
    const PATH: &str = "default_search_provider_data.template_url_data";
    let mut p = read_json(path);
    let mac = pref_mac(device, PATH, &t);
    let dsp = p.entry("default_search_provider_data").or_insert_with(|| json!({}));
    if !dsp.is_object() {
        *dsp = json!({});
    }
    dsp["template_url_data"] = t;
    let prot = p.entry("protection").or_insert_with(|| json!({}));
    if !prot.is_object() {
        *prot = json!({});
    }
    let macs = prot.as_object_mut().expect("object").entry("macs").or_insert_with(|| json!({}));
    if !macs.is_object() {
        *macs = json!({});
    }
    let d = macs.as_object_mut().expect("object").entry("default_search_provider_data").or_insert_with(|| json!({}));
    if !d.is_object() {
        *d = json!({});
    }
    let d = d.as_object_mut().expect("object");
    d.insert("template_url_data".into(), json!(mac));
    // the OS-encrypted twin of the MAC would no longer match; without it Chromium checks the MAC above
    d.remove("template_url_data_encrypted_hash");
    let super_mac = pref_mac(device, "", &prot["macs"]);
    prot["super_mac"] = json!(super_mac);
    write_json(path, &p)
}

/// Apply the profile's search-engine choice when it differs from what was applied last (`marker` remembers it), so
/// a change made inside the browser stays until the choice in Citroenition changes.
pub fn apply_search_choice(data_dir: &Path, marker: &Path, choice: &str) -> Result<bool> {
    let Some(engine) = search_engine(choice) else { return Ok(false) };
    let first_run = !data_dir.join("Default/Preferences").exists();
    if !first_run && fs::read_to_string(marker).is_ok_and(|m| m.trim() == engine.key) {
        return Ok(false);
    }
    set_default_search(data_dir, engine)?;
    fs::write(marker, engine.key)?;
    Ok(true)
}

/// macOS: no "needs to download the font" dialogs from the kernel's processes. `bundle_ids` are the kernel's.
#[cfg(target_os = "macos")]
pub fn disable_font_downloads(bundle_ids: &[String]) {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(a: *const c_void, b: *const u8, n: isize, enc: u32, ext: u8) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerSetAutoActivationSetting(bundle: *const c_void, setting: u32);
    }
    const UTF8: u32 = 0x0800_0100;
    const DISABLED: u32 = 1;
    for id in bundle_ids {
        // SAFETY: a CFString made from valid UTF-8 bytes, released after use.
        unsafe {
            let s = CFStringCreateWithBytes(std::ptr::null(), id.as_ptr(), id.len() as isize, UTF8, 0);
            if !s.is_null() {
                CTFontManagerSetAutoActivationSetting(s, DISABLED);
                CFRelease(s);
            }
        }
    }
}

/// macOS: the CoreText auto-activation setting of `bundle_id` (0 default, 1 disabled, 2 enabled, 3 prompt).
#[cfg(target_os = "macos")]
pub fn font_download_setting(bundle_id: &str) -> u32 {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(a: *const c_void, b: *const u8, n: isize, enc: u32, ext: u8) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerGetAutoActivationSetting(bundle: *const c_void) -> u32;
    }
    // SAFETY: as above.
    unsafe {
        let s = CFStringCreateWithBytes(std::ptr::null(), bundle_id.as_ptr(), bundle_id.len() as isize, 0x0800_0100, 0);
        if s.is_null() {
            return 0;
        }
        let v = CTFontManagerGetAutoActivationSetting(s);
        CFRelease(s);
        v
    }
}

#[cfg(not(target_os = "macos"))]
pub fn disable_font_downloads(_bundle_ids: &[String]) {}

/// The bundle identifiers inside a macOS kernel (`Chromium.app` and its helper apps).
pub fn kernel_bundle_ids(app: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let mut stack = vec![app.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == "Info.plist")
                && let Ok(text) = fs::read_to_string(&p)
                && let Some(id) = plist_string(&text, "CFBundleIdentifier")
            {
                ids.push(id);
            } else if e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_type().is_ok_and(|t| t.is_symlink()) {
                stack.push(p);
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// The `<string>` after `<key>name</key>` in an XML plist.
fn plist_string(text: &str, name: &str) -> Option<String> {
    let after = &text[text.find(&format!("<key>{name}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_are_chromiums() {
        let dev = "00000000-1111-2222-3333-444444444444";
        let mac = pref_mac(dev, "pinned_tabs", &json!([]));
        assert_eq!(mac.len(), 64);
        assert_eq!(mac, hmac_sha256(b"", format!("{dev}pinned_tabs[]").as_bytes()));
        // RFC 4231 test case 2
        let known = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(known, "5BDCC146BF60754E6A042426089575C75A003F089D2739839DEC58B964EC3843");
    }

    #[test]
    fn canonical_json_sorts_and_drops_empty_containers() {
        let v = json!({"b": 1, "a": {"z": {}, "y": [], "x": "s"}, "c": [{}, 2]});
        assert_eq!(canonical(&v), r#"{"a":{"x":"s"},"b":1,"c":[{},2]}"#);
    }

    #[test]
    fn secure_prefs_get_the_engine_its_mac_and_a_new_super_mac() {
        let t = tempfile_dir();
        let path = t.join("Default/Secure Preferences");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"extensions":{"settings":{}},"protection":{"macs":{"pinned_tabs":"AA","default_search_provider_data":{"template_url_data":"OLD","template_url_data_encrypted_hash":"X"}},"super_mac":"OLD"}}"#,
        )
        .unwrap();
        let dev = "DEV";
        write_secure_prefs(&path, dev, template(search_engine("").unwrap())).unwrap();
        let p: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let t_data = &p["default_search_provider_data"]["template_url_data"];
        assert_eq!(t_data["short_name"], "Google");
        let macs = &p["protection"]["macs"];
        assert_eq!(
            macs["default_search_provider_data"]["template_url_data"],
            json!(pref_mac(dev, "default_search_provider_data.template_url_data", t_data))
        );
        assert!(macs["default_search_provider_data"].get("template_url_data_encrypted_hash").is_none());
        assert_eq!(macs["pinned_tabs"], "AA", "other MACs are kept");
        assert_eq!(p["protection"]["super_mac"], json!(pref_mac(dev, "", macs)));
        assert!(p["extensions"]["settings"].is_object(), "other preferences are kept");
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn engine_choice() {
        assert_eq!(search_engine("").unwrap().name, "Google");
        assert_eq!(search_engine("Bing").unwrap().keyword, "bing.com");
        assert_eq!(search_engine("nonsense").unwrap().key, "google");
        assert!(search_engine("browser").is_none());
    }

    #[test]
    fn bundle_ids_come_from_the_plists() {
        let t = tempfile_dir();
        let helper = t.join("Contents/Frameworks/X.framework/Helpers/Chromium Helper (Renderer).app/Contents");
        fs::create_dir_all(&helper).unwrap();
        fs::create_dir_all(t.join("Contents")).unwrap();
        let plist =
            |id: &str| format!("<plist><dict><key>CFBundleIdentifier</key>\n<string>{id}</string></dict></plist>");
        fs::write(t.join("Contents/Info.plist"), plist("org.chromium.Chromium")).unwrap();
        fs::write(helper.join("Info.plist"), plist("org.chromium.Chromium.helper.renderer")).unwrap();
        assert_eq!(kernel_bundle_ids(&t), ["org.chromium.Chromium", "org.chromium.Chromium.helper.renderer"]);
        let _ = fs::remove_dir_all(&t);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("citro-prefs-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
}
