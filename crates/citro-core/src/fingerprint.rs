//! Persona model: AdsPower's `fingerprint_config` (plus the profile's country, proxy exit IP and OS) -> the
//! command-line switches the Clearcote kernel reads.
//!
//! Clearcote is an open-source Chromium build whose identity surface lives in the engine, not in page
//! scripts. One `--fingerprint=<seed>` is one coherent machine: hardware, screen, GPU and per-site render
//! noise all derive from the seed. What must *follow the proxy* -- language, time zone, location, the WebRTC
//! address -- is passed as explicit switches. See `docs/KERNEL.md`.
//!
//! Field mapping (AdsPower API name -> effect):
//!
//! | `fingerprint_config`                         | effect                                                        |
//! |----------------------------------------------|---------------------------------------------------------------|
//! | *(profile)* `persona_os`                     | `--fingerprint-platform` (`windows` / `macos` / `linux`)      |
//! | `seed`                                       | `--fingerprint` (default: the profile id: a stable identity)  |
//! | `brand`, or an `Edg/` / `OPR/` user agent    | `--fingerprint-brand`                                         |
//! | `language`, `language_switch`, country       | `--accept-lang`, `--lang` (Chrome's own defaults for the locale) |
//! | `automatic_timezone`, `timezone`, country    | `--timezone`                                                  |
//! | `latitude`, `longitude`                      | `--fingerprint-location`                                      |
//! | `screen_resolution` (`1366_768`)             | `--fingerprint-screen-width/-height`, `--window-size`         |
//! | `hardware_concurrency`, `device_memory`      | `--fingerprint-hardware-concurrency`, `--fingerprint-device-memory` |
//! | `webgl` = `2` + `webgl_config`               | `--fingerprint-gpu-vendor`, `--fingerprint-gpu-renderer`      |
//! | `webgl` = `0`                                | `--disable-gpu-fingerprint` (the machine's real GPU)          |
//! | `canvas` / `webgl_image` = `0`               | `--disable-canvas-noise` / `--disable-fingerprint-noise`      |
//! | `webrtc`                                     | `--webrtc-ip-handling-policy`, `--webrtc-ip`                  |
//!
//! Ignored on purpose, because the engine derives them and a hand-written value could only contradict the
//! persona: `ua` (beyond choosing OS and brand), `audio`, `client_rects`, `do_not_track`, `fonts`.

use crate::locale;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// AdsPower's `fingerprint_config`, kept as raw JSON so unknown keys round-trip untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FingerprintConfig(pub Map<String, Value>);

impl FingerprintConfig {
    /// A scalar as a string; numbers and booleans are accepted (`1`, `true` -> `"1"`).
    pub fn str(&self, key: &str) -> Option<String> {
        match self.0.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_owned()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    /// `"1"` / `"true"` -> true, `"0"` / `"false"` -> false, anything else -> `default`.
    pub fn on(&self, key: &str, default: bool) -> bool {
        match self.str(key).as_deref() {
            Some("1" | "true") => true,
            Some("0" | "false") => false,
            _ => default,
        }
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn obj(&self, key: &str) -> Option<&Map<String, Value>> {
        self.0.get(key)?.as_object()
    }

    /// Shallow merge: keys in `patch` replace keys in `self`.
    pub fn merge(&mut self, patch: &Map<String, Value>) {
        for (k, v) in patch {
            self.0.insert(k.clone(), v.clone());
        }
    }
}

/// The operating system a persona claims to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    /// The machine we are running on.
    pub fn host() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    pub fn from_name(s: &str) -> Option<Os> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" | "win" | "win32" | "win10" | "win11" => Some(Os::Windows),
            "macos" | "mac" | "darwin" | "osx" => Some(Os::Mac),
            "linux" => Some(Os::Linux),
            _ => None,
        }
    }

    pub fn from_ua(ua: &str) -> Option<Os> {
        if ua.contains("Windows") {
            Some(Os::Windows)
        } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
            Some(Os::Mac)
        } else if ua.contains("Linux") || ua.contains("X11") {
            Some(Os::Linux)
        } else {
            None
        }
    }

    /// The value of `--fingerprint-platform`.
    pub fn name(self) -> &'static str {
        match self {
            Os::Windows => "windows",
            Os::Mac => "macos",
            Os::Linux => "linux",
        }
    }
}

/// What the page sees as the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gpu {
    /// The seed's persona GPU (the default).
    Persona,
    /// The machine's real GPU, strings, limits and pixels alike.
    Real,
    /// A chosen `(unmasked vendor, unmasked renderer)` pair.
    Custom(String, String),
}

/// Render noise (canvas / WebGL read-back farbling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noise {
    On,
    /// 2D canvas read-back is left alone, WebGL keeps its noise.
    CanvasOff,
    /// No render noise at all.
    AllOff,
}

/// How WebRTC treats IP addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebRtc {
    /// No UDP outside the proxy: the leak-proof default (`disabled`, `disable_udp`).
    Leakproof,
    /// Candidates carry this address, the proxy's exit (`proxy`, `forward`).
    Address(String),
    /// The machine's real addresses (`local`).
    Local,
}

/// What identifies a profile when deriving its persona.
#[derive(Debug, Clone, Copy)]
pub struct Ident<'a> {
    pub profile_id: &'a str,
    /// ISO country of the proxy exit (drives language and time zone when they follow the IP).
    pub country: &'a str,
    /// The proxy's exit IP, when known (the WebRTC address).
    pub ip: &'a str,
    /// `Profile::persona_os`: `windows`, `macos`, `linux`, or empty.
    pub persona_os: &'a str,
}

/// The concrete persona of a profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub seed: String,
    pub os: Os,
    pub brand: String,
    /// The `--accept-lang` list.
    pub accept_lang: String,
    /// The `--lang` UI locale (also `LANGUAGE` in the kernel's environment).
    pub ui_locale: Option<String>,
    pub timezone: Option<String>,
    /// `None` = the seed decides.
    pub screen: Option<(u32, u32)>,
    pub hardware_concurrency: Option<u32>,
    pub device_memory: Option<u32>,
    pub gpu: Gpu,
    /// latitude, longitude.
    pub location: Option<(f64, f64)>,
    pub noise: Noise,
    pub webrtc: WebRtc,
}

fn parse_screen(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['_', 'x', 'X', ','])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 200 && h >= 200).then_some((w, h))
}

fn parse_u32(s: Option<String>) -> Option<u32> {
    s.and_then(|s| s.trim().parse().ok()).filter(|v| *v > 0)
}

/// The OS named by AdsPower's `random_ua.ua_system_version` list.
fn os_hint(fp: &FingerprintConfig) -> Option<Os> {
    let list = fp.obj("random_ua")?.get("ua_system_version")?.as_array()?;
    let all = list.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    if all.contains("mac") {
        Some(Os::Mac)
    } else if all.contains("linux") {
        Some(Os::Linux)
    } else if all.contains("windows") {
        Some(Os::Windows)
    } else {
        None
    }
}

fn brand_of(fp: &FingerprintConfig, ua: Option<&str>) -> String {
    let named = fp.str("brand").map(|b| b.trim().to_ascii_lowercase());
    if let Some(b @ ("chrome" | "edge" | "opera" | "vivaldi")) = named.as_deref() {
        return b.to_owned();
    }
    match ua {
        Some(u) if u.contains("Edg/") => "edge",
        Some(u) if u.contains("OPR/") => "opera",
        Some(u) if u.contains("Vivaldi/") => "vivaldi",
        _ => "chrome",
    }
    .to_owned()
}

impl Resolved {
    /// Derive the persona. Everything "random" comes from the seed, so a profile keeps its identity across
    /// launches without storing it.
    pub fn resolve(id: &Ident<'_>, fp: &FingerprintConfig) -> Resolved {
        let custom_ua = fp.str("ua").map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        let os = Os::from_name(id.persona_os)
            .or_else(|| custom_ua.as_deref().and_then(Os::from_ua))
            .or_else(|| os_hint(fp))
            .unwrap_or_else(Os::host);

        // Language: an explicit list wins unless the profile follows the IP; one tag is an OS locale and
        // expands to what a fresh Chrome on that OS shows.
        let row = locale::country(id.country);
        let explicit = fp.list("language");
        let by_ip = fp.str("language_switch").as_deref() == Some("1");
        let wanted = match row {
            Some((_, tag)) if by_ip || explicit.is_empty() => tag.to_owned(),
            _ if !explicit.is_empty() => explicit.join(","),
            _ => "en-US".to_owned(),
        };
        let (accept_lang, mut ui_locale) = locale::resolve_languages(&wanted);
        if !fp.on("page_language_switch", true)
            && let Some(l) = fp.str("page_language").filter(|l| !l.is_empty() && l != "native")
        {
            ui_locale = locale::chrome_ui_locale(&l).or(Some(l));
        }

        // Time zone: pinned, else the exit country's, else the country the first language tag names.
        let timezone = if fp.str("automatic_timezone").as_deref() == Some("0") {
            fp.str("timezone").map(|t| t.trim().replace(' ', "_")).filter(|t| !t.is_empty())
        } else {
            // the region of the language the user wrote (`de-AT`), not of Chrome's expanded list (`de-DE,...`)
            let first = wanted.split(',').next().unwrap_or("");
            row.or_else(|| locale::country_of_tag(first)).map(|(tz, _)| tz.to_owned())
        };

        let location = match (
            fp.str("latitude").and_then(|v| v.trim().parse::<f64>().ok()),
            fp.str("longitude").and_then(|v| v.trim().parse::<f64>().ok()),
        ) {
            (Some(la), Some(lo)) if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) => Some((la, lo)),
            _ => None,
        };

        let gpu = match fp.str("webgl").as_deref() {
            Some("0") => Gpu::Real,
            Some("2") => fp
                .obj("webgl_config")
                .and_then(|cfg| {
                    let s = |k: &str| cfg.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
                    s("unmasked_vendor").zip(s("unmasked_renderer"))
                })
                .map_or(Gpu::Persona, |(v, r)| Gpu::Custom(v, r)),
            _ => Gpu::Persona,
        };

        let (canvas, webgl_image) = (fp.on("canvas", true), fp.on("webgl_image", true));
        let noise = match (canvas, webgl_image) {
            (false, false) => Noise::AllOff,
            (false, true) => Noise::CanvasOff,
            _ => Noise::On,
        };

        let ip = id.ip.trim();
        let webrtc = match fp.str("webrtc").as_deref().unwrap_or("disabled") {
            "local" => WebRtc::Local,
            "proxy" | "forward" if !ip.is_empty() => WebRtc::Address(ip.to_owned()),
            _ => WebRtc::Leakproof,
        };

        Resolved {
            seed: fp
                .str("seed")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.profile_id.to_owned()),
            os,
            brand: brand_of(fp, custom_ua.as_deref()),
            accept_lang,
            ui_locale,
            timezone,
            screen: fp.str("screen_resolution").as_deref().and_then(parse_screen),
            hardware_concurrency: parse_u32(fp.str("hardware_concurrency")),
            device_memory: parse_u32(fp.str("device_memory")),
            gpu,
            location,
            noise,
            webrtc,
        }
    }

    /// The persona switches, in a fixed order.
    pub fn switches(&self) -> Vec<String> {
        let mut a = vec![
            format!("--fingerprint={}", self.seed),
            format!("--fingerprint-platform={}", self.os.name()),
            format!("--fingerprint-brand={}", self.brand),
            format!("--accept-lang={}", self.accept_lang),
        ];
        if let Some(ui) = &self.ui_locale {
            a.push(format!("--lang={ui}"));
        }
        if let Some(tz) = &self.timezone {
            a.push(format!("--timezone={tz}"));
        }
        if let Some(v) = self.hardware_concurrency {
            a.push(format!("--fingerprint-hardware-concurrency={v}"));
        }
        if let Some(v) = self.device_memory {
            a.push(format!("--fingerprint-device-memory={v}"));
        }
        if let Some((w, h)) = self.screen {
            a.push(format!("--fingerprint-screen-width={w}"));
            a.push(format!("--fingerprint-screen-height={h}"));
        }
        match &self.gpu {
            Gpu::Persona => {}
            Gpu::Real => a.push("--disable-gpu-fingerprint".into()),
            Gpu::Custom(vendor, renderer) => {
                a.push(format!("--fingerprint-gpu-vendor={vendor}"));
                a.push(format!("--fingerprint-gpu-renderer={renderer}"));
            }
        }
        if let Some((la, lo)) = self.location {
            a.push(format!("--fingerprint-location={la},{lo}"));
        }
        match self.noise {
            Noise::On => {}
            Noise::CanvasOff => a.push("--disable-canvas-noise".into()),
            Noise::AllOff => a.push("--disable-fingerprint-noise".into()),
        }
        match &self.webrtc {
            WebRtc::Leakproof => a.push("--webrtc-ip-handling-policy=disable_non_proxied_udp".into()),
            WebRtc::Address(ip) => {
                a.push(format!("--webrtc-ip={ip}"));
                a.push("--webrtc-ip-handling-policy=disable_non_proxied_udp".into());
            }
            WebRtc::Local => a.push("--webrtc-ip-handling-policy=default".into()),
        }
        // `navigator.bluetooth` must follow the CLAIM, in both directions: stable Chrome ships Web Bluetooth
        // on Windows and macOS but not on Linux, whatever OS the build itself runs on.
        a.push(
            match self.os {
                Os::Windows | Os::Mac => "--enable-features=WebBluetooth",
                Os::Linux => "--disable-features=WebBluetooth",
            }
            .into(),
        );
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fp(v: Value) -> FingerprintConfig {
        serde_json::from_value(v).unwrap()
    }

    fn ident<'a>(id: &'a str, country: &'a str, persona_os: &'a str) -> Ident<'a> {
        Ident { profile_id: id, country, ip: "", persona_os }
    }

    fn arg<'a>(a: &'a [String], prefix: &str) -> Option<&'a str> {
        a.iter().find_map(|x| x.strip_prefix(prefix))
    }

    #[test]
    fn french_profile_looks_like_a_fresh_french_chrome() {
        let r = Resolved::resolve(&ident("k1abc23", "FR", "windows"), &fp(json!({})));
        assert_eq!(r.accept_lang, "fr-FR,fr,en-US,en");
        assert_eq!((r.ui_locale.as_deref(), r.timezone.as_deref()), (Some("fr"), Some("Europe/Paris")));
        assert_eq!((r.os, r.brand.as_str(), r.seed.as_str()), (Os::Windows, "chrome", "k1abc23"));
        let s = r.switches();
        assert_eq!(arg(&s, "--fingerprint="), Some("k1abc23"));
        assert_eq!(arg(&s, "--fingerprint-platform="), Some("windows"));
        assert_eq!(arg(&s, "--lang="), Some("fr"));
        assert_eq!(arg(&s, "--timezone="), Some("Europe/Paris"));
        assert!(s.contains(&"--enable-features=WebBluetooth".to_owned()));
    }

    #[test]
    fn the_seed_is_the_profile_id_unless_pinned() {
        let a = Resolved::resolve(&ident("k1", "", "linux"), &fp(json!({})));
        let b = Resolved::resolve(&ident("k2", "", "linux"), &fp(json!({})));
        let pinned = Resolved::resolve(&ident("k1", "", "linux"), &fp(json!({"seed": " shared "})));
        assert_ne!(a.seed, b.seed);
        assert_eq!(a, Resolved::resolve(&ident("k1", "", "linux"), &fp(json!({}))));
        assert_eq!(pinned.seed, "shared");
    }

    #[test]
    fn explicit_config_wins_over_country_unless_ip_based() {
        let cfg = fp(json!({"language": ["it-IT", "it"], "automatic_timezone": "0", "timezone": "Europe/Rome",
                            "screen_resolution": "1366_768", "hardware_concurrency": "8", "device_memory": 4}));
        let r = Resolved::resolve(&ident("x", "fr", "linux"), &cfg);
        assert_eq!(r.accept_lang, "it-IT,it");
        assert_eq!(r.ui_locale.as_deref(), Some("it"));
        assert_eq!(r.timezone.as_deref(), Some("Europe/Rome"));
        assert_eq!(r.screen, Some((1366, 768)));
        assert_eq!((r.hardware_concurrency, r.device_memory), (Some(8), Some(4)));
        let s = r.switches();
        assert_eq!(arg(&s, "--fingerprint-screen-width="), Some("1366"));
        assert_eq!(arg(&s, "--fingerprint-hardware-concurrency="), Some("8"));
        assert_eq!(arg(&s, "--fingerprint-device-memory="), Some("4"));

        let by_ip = fp(json!({"language": ["it-IT", "it"], "language_switch": "1"}));
        assert_eq!(Resolved::resolve(&ident("x", "fr", "linux"), &by_ip).accept_lang, "fr-FR,fr,en-US,en");
    }

    #[test]
    fn no_country_falls_back_to_the_language_region_then_to_english() {
        let r = Resolved::resolve(&ident("x", "", "linux"), &fp(json!({"language": ["de-AT"]})));
        assert_eq!((r.accept_lang.as_str(), r.timezone.as_deref()), ("de-DE,de,en-US,en", Some("Europe/Vienna")));
        let none = Resolved::resolve(&ident("x", "", "linux"), &fp(json!({})));
        assert_eq!((none.accept_lang.as_str(), none.ui_locale.as_deref()), ("en-US,en", Some("en-US")));
        assert_eq!(none.timezone.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn the_os_comes_from_the_profile_then_the_user_agent_then_random_ua_then_the_host() {
        let mac_ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
        let with_ua = fp(json!({ "ua": mac_ua }));
        assert_eq!(Resolved::resolve(&ident("x", "", ""), &with_ua).os, Os::Mac);
        assert_eq!(Resolved::resolve(&ident("x", "", "windows"), &with_ua).os, Os::Windows);
        let random = fp(json!({"random_ua": {"ua_browser": "chrome", "ua_system_version": ["Mac OS X 13"]}}));
        assert_eq!(Resolved::resolve(&ident("x", "", ""), &random).os, Os::Mac);
        assert_eq!(Resolved::resolve(&ident("x", "", ""), &fp(json!({}))).os, Os::host());
        assert_eq!(Resolved::resolve(&ident("x", "", "garbage"), &fp(json!({}))).os, Os::host());
        assert_eq!(Os::from_name(" MacOS "), Some(Os::Mac));
    }

    #[test]
    fn brand_follows_the_config_or_the_user_agent() {
        let edge_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36 Edg/151.0.0.0";
        assert_eq!(Resolved::resolve(&ident("x", "", "windows"), &fp(json!({"ua": edge_ua}))).brand, "edge");
        assert_eq!(Resolved::resolve(&ident("x", "", "windows"), &fp(json!({"brand": "Vivaldi"}))).brand, "vivaldi");
        assert_eq!(Resolved::resolve(&ident("x", "", "windows"), &fp(json!({"brand": "netscape"}))).brand, "chrome");
    }

    #[test]
    fn linux_claims_hide_web_bluetooth() {
        let s = Resolved::resolve(&ident("x", "", "linux"), &fp(json!({}))).switches();
        assert!(s.contains(&"--disable-features=WebBluetooth".to_owned()));
        assert!(!s.iter().any(|a| a == "--enable-features=WebBluetooth"));
    }

    #[test]
    fn noise_and_gpu_modes() {
        let s = |v: Value| Resolved::resolve(&ident("x", "", "windows"), &fp(v)).switches();
        assert!(!s(json!({})).iter().any(|a| a.contains("noise") || a.contains("gpu")));
        assert!(s(json!({"canvas": "0"})).contains(&"--disable-canvas-noise".to_owned()));
        assert!(s(json!({"canvas": "0", "webgl_image": "0"})).contains(&"--disable-fingerprint-noise".to_owned()));
        assert!(s(json!({"webgl": "0"})).contains(&"--disable-gpu-fingerprint".to_owned()));
        let custom = s(
            json!({"webgl": "2", "webgl_config": {"unmasked_vendor": "Google Inc. (NVIDIA)", "unmasked_renderer": "ANGLE (NVIDIA)"}}),
        );
        assert_eq!(arg(&custom, "--fingerprint-gpu-renderer="), Some("ANGLE (NVIDIA)"));
        // "custom" without a usable config, and "random", keep the persona's own GPU
        assert!(!s(json!({"webgl": "2"})).iter().any(|a| a.contains("gpu")));
        assert!(!s(json!({"webgl": "3"})).iter().any(|a| a.contains("gpu")));
    }

    #[test]
    fn webrtc_modes() {
        let mk = |mode: &str, ip: &'static str| {
            let id = Ident { profile_id: "x", country: "", ip, persona_os: "linux" };
            Resolved::resolve(&id, &fp(json!({"webrtc": mode}))).switches()
        };
        let policy = "--webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned();
        for leakproof in [mk("disabled", "1.2.3.4"), mk("disable_udp", ""), mk("proxy", ""), mk("", "1.2.3.4")] {
            assert!(leakproof.contains(&policy) && !leakproof.iter().any(|a| a.starts_with("--webrtc-ip=")));
        }
        let fwd = mk("forward", "1.2.3.4");
        assert!(fwd.contains(&"--webrtc-ip=1.2.3.4".to_owned()) && fwd.contains(&policy));
        let local = mk("local", "1.2.3.4");
        assert!(local.contains(&"--webrtc-ip-handling-policy=default".to_owned()));
        assert!(!local.contains(&policy));
    }

    #[test]
    fn location_needs_both_coordinates_in_range() {
        let s = |v: Value| Resolved::resolve(&ident("x", "", "linux"), &fp(v)).switches();
        assert_eq!(
            arg(&s(json!({"latitude": "48.85", "longitude": 2.35})), "--fingerprint-location="),
            Some("48.85,2.35")
        );
        assert!(arg(&s(json!({"latitude": "48.85"})), "--fingerprint-location=").is_none());
        assert!(arg(&s(json!({"latitude": "99", "longitude": "2"})), "--fingerprint-location=").is_none());
    }

    #[test]
    fn page_language_can_be_pinned_apart_from_the_browser_language() {
        let cfg = fp(json!({"page_language_switch": "0", "page_language": "de"}));
        let r = Resolved::resolve(&ident("x", "fr", "linux"), &cfg);
        assert_eq!((r.accept_lang.as_str(), r.ui_locale.as_deref()), ("fr-FR,fr,en-US,en", Some("de")));
        let native = fp(json!({"page_language_switch": "0", "page_language": "native"}));
        assert_eq!(Resolved::resolve(&ident("x", "fr", "linux"), &native).ui_locale.as_deref(), Some("fr"));
    }

    #[test]
    fn lenient_scalar_types() {
        let c = fp(json!({"a": 1, "b": true, "c": "x", "d": null}));
        assert_eq!(
            (c.str("a").as_deref(), c.str("b").as_deref(), c.str("c").as_deref(), c.str("d")),
            (Some("1"), Some("1"), Some("x"), None)
        );
        assert!(c.on("b", false) && !c.on("zzz", false) && c.on("zzz", true));
    }
}
