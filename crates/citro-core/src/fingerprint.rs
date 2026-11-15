//! Fingerprint model: AdsPower's `fingerprint_config` -> the parameters the SunBrowser kernel reads.
//!
//! The launcher mirrors what AdsPower's `main.min.js` does (see `docs/REVERSE_ENGINEERING.md`):
//!
//! * **inline**: `--extended-parameters=<shuffled-base64 JSON>` carries every parameter plus the
//!   *paths* of the `StaticConfig`, `DynamicConfig` and `WebGLFP` files.
//! * **static file**: every inline key except [`NOT_IN_STATIC`], plus static-only keys
//!   (`UserAgentMetadata`, `CanvasMarkEx`, `WebGLMarkEx`, `command_line`).
//! * **dynamic file**: `TimeZone`, `Geoposition`, `WebRTCAddress`, `DisableWebRTC`.
//!
//! Which channel the kernel actually *honours* was measured on a real kernel 157 by launching it with
//! one key at a time and reading the result back over CDP (`tools/re-scripts/fp_matrix2.mjs`):
//!
//! | observed                                   | honoured from                                          |
//! |--------------------------------------------|--------------------------------------------------------|
//! | `navigator.language(s)`, `Accept-Language` | static `Langs` (+ `AcceptLang` for the header); inline `Langs` is ignored |
//! | `Intl` time zone                           | dynamic `TimeZone`; inline `TimeZone` is ignored       |
//! | `platform`, `hardwareConcurrency`, `deviceMemory`, `doNotTrack`, `screen.*`, canvas and audio noise | inline |
//! | `userAgentData.platform`                   | static `UserAgentMetadata`, only together with the `--user-agent` flag |
//! | HTTP proxy authentication                  | inline `ProxyUser`/`ProxyPassword` **or** static `ProxyChain` (both work; the launcher uses `ProxyChain`, `tools/re-scripts/px_matrix.mjs`); credentials inside the `--proxy-server` URL make the kernel not contact the proxy at all |
//!
//! Emitted exactly like AdsPower but with no effect observable on a headless test kernel:
//! `ClientRectFp`, `WebGLFP`, `Geoposition`, `WebRTC*`. Known gap: the `Sec-CH-UA-Platform` request
//! header still reports the host OS.

use crate::rng::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Inline keys that are *not* mirrored into the static file.
pub const NOT_IN_STATIC: [&str; 7] =
    ["CustomIcon", "WebGLFP", "StaticConfig", "CookiesFile", "DynamicConfig", "Geoposition", "TimeZone"];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
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

    /// `navigator.platform`.
    pub fn platform(self) -> &'static str {
        match self {
            Os::Windows => "Win32",
            Os::Mac => "MacIntel",
            Os::Linux => "Linux x86_64",
        }
    }

    /// `userAgentData` metadata in the shape AdsPower writes to `StaticConfig.UserAgentMetadata`.
    pub fn ua_metadata(self) -> Value {
        let (platform, version, arch) = match self {
            Os::Windows => ("Windows", "10.0.0", "x86"),
            Os::Mac => ("macOS", "14.5.0", "arm"),
            Os::Linux => ("Linux", "6.8.0", "x86"),
        };
        json!({"platform": platform, "platformVersion": version, "architecture": arch,
               "model": "", "mobile": false, "bitness": "64", "wow64": false})
    }

    /// A few plausible GPUs for `webgl` = random; (unmasked vendor, unmasked renderer).
    fn gpus(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Os::Windows => &[
                ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
                (
                    "Google Inc. (NVIDIA)",
                    "ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 SUPER Direct3D11 vs_5_0 ps_5_0, D3D11)",
                ),
                ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce GTX 1060 6GB Direct3D11 vs_5_0 ps_5_0, D3D11)"),
                ("Google Inc. (Intel)", "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
                ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon RX 580 Series Direct3D11 vs_5_0 ps_5_0, D3D11)"),
            ],
            Os::Mac => &[
                ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)"),
                ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)"),
            ],
            Os::Linux => &[
                (
                    "Google Inc. (NVIDIA Corporation)",
                    "ANGLE (NVIDIA Corporation, NVIDIA GeForce GTX 1050 Ti/PCIe/SSE2, OpenGL 4.5.0)",
                ),
                ("Google Inc. (Intel)", "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)"),
            ],
        }
    }
}

/// Chrome's reduced user agent for `os` at the given major version.
pub fn default_ua(os: Os, major: u32) -> String {
    let tail = format!("AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36");
    match os {
        Os::Windows => format!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) {tail}"),
        Os::Mac => format!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) {tail}"),
        Os::Linux => format!("Mozilla/5.0 (X11; Linux x86_64) {tail}"),
    }
}

/// `Accept-Language` for a language list (port of AdsPower's `getAccept`):
/// `["fr-FR","fr"]` -> `fr-FR,fr;q=0.9`.
pub fn accept_language(langs: &[String]) -> String {
    fn q(n: i32) -> i32 {
        if 10 - n < 1 { 1 } else { 10 - n }
    }
    let mut out = String::new();
    let mut n = 0i32;
    let mut seen: Vec<&str> = Vec::new();
    for (a, lang) in langs.iter().enumerate() {
        let next = langs.get(a + 1).map_or("", String::as_str);
        let short = lang.split('-').next().unwrap_or("");
        let next_short = next.split('-').next().unwrap_or("");
        if a == 0 {
            out.push_str(lang);
            n += 1;
        }
        if a > 0 && !seen.contains(&lang.as_str()) {
            out.push_str(&format!(",{lang};q=0.{}", q(n)));
            n += 1;
        }
        if !(lang == short || short == next_short || seen.contains(&short)) {
            out.push_str(&format!(",{short};q=0.{}", q(n)));
            n += 1;
            seen.push(short);
        }
        seen.push(lang);
    }
    out
}

/// country code, IANA time zone, `navigator.languages`.
type CountryRow = (&'static str, &'static str, &'static [&'static str]);

const COUNTRIES: &[CountryRow] = &[
    ("fr", "Europe/Paris", &["fr-FR", "fr"]),
    ("de", "Europe/Berlin", &["de-DE", "de"]),
    ("gb", "Europe/London", &["en-GB", "en"]),
    ("uk", "Europe/London", &["en-GB", "en"]),
    ("us", "America/New_York", &["en-US", "en"]),
    ("ca", "America/Toronto", &["en-CA", "en"]),
    ("es", "Europe/Madrid", &["es-ES", "es"]),
    ("it", "Europe/Rome", &["it-IT", "it"]),
    ("nl", "Europe/Amsterdam", &["nl-NL", "nl"]),
    ("be", "Europe/Brussels", &["fr-BE", "fr"]),
    ("ch", "Europe/Zurich", &["de-CH", "de"]),
    ("at", "Europe/Vienna", &["de-AT", "de"]),
    ("pt", "Europe/Lisbon", &["pt-PT", "pt"]),
    ("ie", "Europe/Dublin", &["en-IE", "en"]),
    ("pl", "Europe/Warsaw", &["pl-PL", "pl"]),
    ("se", "Europe/Stockholm", &["sv-SE", "sv"]),
    ("no", "Europe/Oslo", &["nb-NO", "nb"]),
    ("dk", "Europe/Copenhagen", &["da-DK", "da"]),
    ("fi", "Europe/Helsinki", &["fi-FI", "fi"]),
    ("ro", "Europe/Bucharest", &["ro-RO", "ro"]),
    ("gr", "Europe/Athens", &["el-GR", "el"]),
    ("cz", "Europe/Prague", &["cs-CZ", "cs"]),
    ("hu", "Europe/Budapest", &["hu-HU", "hu"]),
    ("tr", "Europe/Istanbul", &["tr-TR", "tr"]),
    ("ru", "Europe/Moscow", &["ru-RU", "ru"]),
    ("ua", "Europe/Kyiv", &["uk-UA", "uk"]),
    ("br", "America/Sao_Paulo", &["pt-BR", "pt"]),
    ("mx", "America/Mexico_City", &["es-MX", "es"]),
    ("ar", "America/Argentina/Buenos_Aires", &["es-AR", "es"]),
    ("au", "Australia/Sydney", &["en-AU", "en"]),
    ("nz", "Pacific/Auckland", &["en-NZ", "en"]),
    ("jp", "Asia/Tokyo", &["ja-JP", "ja"]),
    ("kr", "Asia/Seoul", &["ko-KR", "ko"]),
    ("cn", "Asia/Shanghai", &["zh-CN", "zh"]),
    ("hk", "Asia/Hong_Kong", &["zh-HK", "zh"]),
    ("sg", "Asia/Singapore", &["en-SG", "en"]),
    ("in", "Asia/Kolkata", &["en-IN", "en"]),
    ("id", "Asia/Jakarta", &["id-ID", "id"]),
    ("th", "Asia/Bangkok", &["th-TH", "th"]),
    ("vn", "Asia/Ho_Chi_Minh", &["vi-VN", "vi"]),
    ("ph", "Asia/Manila", &["en-PH", "en"]),
    ("my", "Asia/Kuala_Lumpur", &["ms-MY", "ms"]),
    ("ae", "Asia/Dubai", &["ar-AE", "ar"]),
    ("sa", "Asia/Riyadh", &["ar-SA", "ar"]),
    ("il", "Asia/Jerusalem", &["he-IL", "he"]),
    ("za", "Africa/Johannesburg", &["en-ZA", "en"]),
    ("eg", "Africa/Cairo", &["ar-EG", "ar"]),
    ("ma", "Africa/Casablanca", &["fr-MA", "fr"]),
    ("ng", "Africa/Lagos", &["en-NG", "en"]),
];

fn country_row(code: &str) -> Option<&'static CountryRow> {
    let code = code.trim().to_ascii_lowercase();
    COUNTRIES.iter().find(|c| c.0 == code)
}

const COMMON_SCREENS: &[(u32, u32)] =
    &[(1920, 1080), (1366, 768), (1536, 864), (1440, 900), (1600, 900), (2560, 1440), (1280, 720)];

/// AdsPower's `hashcode`: `t = 31 * t + charCode`, with JS number (f64) semantics and no 32-bit wrap.
pub fn js_hashcode(s: &str) -> f64 {
    s.encode_utf16().fold(0.0, |t, u| 31.0 * t + f64::from(u))
}

/// AdsPower's `setAudioNoise`: sum of char codes % 2000 - 1000 (never 0, which would mean "no noise").
pub fn audio_noise(id: &str) -> i64 {
    let sum: u64 = id.encode_utf16().map(u64::from).sum();
    let v = (sum % 2000) as i64 - 1000;
    if v == 0 { 1 } else { v }
}

fn parse_screen(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['_', 'x', 'X', ','])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 200 && h >= 200).then_some((w, h))
}

fn parse_u32(s: Option<String>) -> Option<u32> {
    s.and_then(|s| s.trim().parse().ok()).filter(|v| *v > 0)
}

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

/// What identifies a profile when deriving its fingerprint.
#[derive(Debug, Clone, Copy)]
pub struct Ident<'a> {
    pub profile_id: &'a str,
    /// ISO country of the proxy exit (drives language / time zone when they follow the IP).
    pub country: &'a str,
    /// The proxy's exit IP, when known (WebRTC address).
    pub ip: &'a str,
    pub kernel_major: u32,
}

/// The concrete identity of a profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub os: Os,
    pub user_agent: String,
    pub platform: String,
    pub ua_metadata: Value,
    /// `None` = follow the real display.
    pub screen: Option<(u32, u32)>,
    pub hardware_concurrency: Option<u32>,
    pub device_memory: Option<u32>,
    pub languages: Vec<String>,
    pub accept_language: String,
    pub ui_language: Option<String>,
    pub timezone: Option<String>,
    /// latitude, longitude, accuracy in metres.
    pub geo: Option<(f64, f64, f64)>,
    pub geo_setting: String,
    pub canvas_mark: Option<String>,
    pub webgl_mark: Option<String>,
    pub audio_fp: Option<i64>,
    pub client_rect_fp: Option<i64>,
    pub do_not_track: bool,
    /// Custom WebGL (unmasked vendor, unmasked renderer).
    pub webgl: Option<(String, String)>,
    pub webrtc: String,
    pub webrtc_ip: String,
}

/// What the kernel gets, split by channel. Values are plain JSON; file paths are added by the launcher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KernelParams {
    /// The `--extended-parameters` object (AdsPower's `sunBrowserParams`).
    pub inline: Map<String, Value>,
    /// Keys that only exist in the static file.
    pub static_only: Map<String, Value>,
    /// The dynamic file.
    pub dynamic: Map<String, Value>,
    /// Extra command-line flags the fingerprint needs.
    pub args: Vec<String>,
}

impl KernelParams {
    /// The static file: static-only keys plus every inline key except [`NOT_IN_STATIC`].
    pub fn static_config(&self) -> Map<String, Value> {
        let mut out = self.static_only.clone();
        for (k, v) in &self.inline {
            if !NOT_IN_STATIC.contains(&k.as_str()) {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}

impl Resolved {
    /// Derive the identity. Everything that is "random" comes from the profile id, so a profile keeps
    /// its identity across launches without storing it.
    pub fn resolve(id: &Ident<'_>, fp: &FingerprintConfig) -> Resolved {
        let seeded = |label: &str| Rng::from_str_seed(&format!("{}:{label}", id.profile_id));
        let row = country_row(id.country);

        let custom_ua = fp.str("ua").map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        let os = custom_ua.as_deref().and_then(Os::from_ua).or_else(|| os_hint(fp)).unwrap_or(Os::Windows);
        let user_agent = custom_ua.unwrap_or_else(|| default_ua(os, id.kernel_major));

        let screen = match fp.str("screen_resolution").as_deref() {
            None | Some("" | "none") => None,
            Some("random") => Some(*seeded("screen").pick(COMMON_SCREENS)),
            Some(s) => parse_screen(s),
        };

        let explicit_langs = fp.list("language");
        let by_ip = fp.str("language_switch").as_deref() == Some("1");
        let languages: Vec<String> = match row {
            Some(r) if by_ip || explicit_langs.is_empty() => r.2.iter().map(|s| (*s).to_owned()).collect(),
            _ if !explicit_langs.is_empty() => explicit_langs,
            _ => vec!["en-US".into(), "en".into()],
        };

        // UI language: follows `language` unless the profile pins one (`native` = system default).
        let ui_language = if fp.on("page_language_switch", true) {
            languages.first().cloned()
        } else {
            fp.str("page_language").filter(|l| !l.is_empty() && l != "native")
        };

        let timezone = if fp.str("automatic_timezone").as_deref() == Some("0") {
            fp.str("timezone").map(|t| t.trim().replace(' ', "_")).filter(|t| !t.is_empty())
        } else {
            row.map(|r| r.1.to_owned())
        };

        let geo = match (
            fp.str("latitude").and_then(|v| v.trim().parse::<f64>().ok()),
            fp.str("longitude").and_then(|v| v.trim().parse::<f64>().ok()),
        ) {
            (Some(la), Some(lo)) if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) => {
                let acc = fp.str("accuracy").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(100.0);
                Some((la, lo, acc))
            }
            _ => None,
        };

        let webgl = match fp.str("webgl").as_deref() {
            None | Some("" | "0") => None,
            Some(mode) => {
                let custom = fp.obj("webgl_config").and_then(|cfg| {
                    let s = |k: &str| cfg.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
                    s("unmasked_vendor").zip(s("unmasked_renderer"))
                });
                match (mode, custom) {
                    ("2", Some(c)) => Some(c),
                    _ => {
                        let (v, r) = *seeded("gpu").pick(os.gpus());
                        Some((v.to_owned(), r.to_owned()))
                    }
                }
            }
        };

        // Marks are the profile id itself (as AdsPower does) unless the profile pins an explicit id.
        let mark = |switch: &str, pinned: &str| {
            fp.on(switch, true)
                .then(|| fp.str(pinned).filter(|v| !v.is_empty()).unwrap_or_else(|| id.profile_id.to_owned()))
        };
        // A pinned id keeps its sign; a pinned 0 becomes 1 (0 would mean "no noise").
        let pinned =
            |key: &str| fp.str(key).and_then(|v| v.trim().parse::<i64>().ok()).map(|v| if v == 0 { 1 } else { v });
        let audio_fp = fp.on("audio", true).then(|| pinned("audio_id").unwrap_or_else(|| audio_noise(id.profile_id)));
        let client_rect_fp = fp
            .on("client_rects", true)
            .then(|| pinned("client_rects_id").unwrap_or_else(|| (js_hashcode(id.profile_id) % 2e4 - 1e4) as i64));

        Resolved {
            os,
            platform: os.platform().to_owned(),
            ua_metadata: os.ua_metadata(),
            user_agent,
            screen,
            hardware_concurrency: parse_u32(fp.str("hardware_concurrency")),
            device_memory: parse_u32(fp.str("device_memory")),
            accept_language: accept_language(&languages),
            languages,
            ui_language,
            timezone,
            geo,
            geo_setting: fp.str("location").filter(|s| !s.is_empty()).unwrap_or_else(|| "ask".into()),
            canvas_mark: mark("canvas", "canvas_id"),
            webgl_mark: mark("webgl_image", "webgl_image_id"),
            audio_fp,
            client_rect_fp,
            do_not_track: fp.on("do_not_track", false),
            webgl,
            webrtc: fp.str("webrtc").filter(|s| !s.is_empty()).unwrap_or_else(|| "disabled".into()),
            webrtc_ip: id.ip.trim().to_owned(),
        }
    }

    pub fn kernel_params(&self, start_time: i64, kernel_major: u32) -> KernelParams {
        let mut p = KernelParams::default();
        let i = &mut p.inline;
        i.insert("Platform".into(), json!(self.platform));
        if let Some((w, h)) = self.screen {
            i.insert("ScreenSize".into(), json!(format!("{w},{h}")));
        }
        if let Some(v) = self.hardware_concurrency {
            i.insert("HardwareConcurrency".into(), json!(v));
        }
        if let Some(v) = self.device_memory {
            i.insert("DeviceMemory".into(), json!(v));
        }
        if self.do_not_track {
            i.insert("EnableDoNotTrack".into(), json!(true));
        }
        if let Some(v) = &self.canvas_mark {
            i.insert("CanvasMark".into(), json!(v));
        }
        if let Some(v) = &self.webgl_mark {
            i.insert("WebGLMark".into(), json!(v));
        }
        i.insert("StartTime".into(), json!(start_time));
        if let Some(v) = self.audio_fp {
            i.insert("AudioFp".into(), json!(v));
        }
        if let Some(v) = self.client_rect_fp {
            i.insert("ClientRectFp".into(), json!(v));
        }
        i.insert("GeolocationSetting".into(), json!(self.geo_setting));
        i.insert("Langs".into(), json!(self.languages.join(",")));
        i.insert("AcceptLang".into(), json!(self.accept_language));
        i.insert("DisableBackgroundMode".into(), json!(true));
        i.insert("ForceProcessExit".into(), json!(true));
        let geo = self.geo.map(|(la, lo, ac)| format!("{la},{lo},{ac}"));
        if let Some(g) = &geo {
            i.insert("Geoposition".into(), json!(g));
        }
        if let Some(tz) = &self.timezone {
            i.insert("TimeZone".into(), json!(tz));
        }

        // Static-only keys. The `*Ex` copies are what newer kernels (>= 105) look at.
        p.static_only.insert("UserAgentMetadata".into(), self.ua_metadata.clone());
        if kernel_major >= 105 {
            if let Some(v) = &self.canvas_mark {
                p.static_only.insert("CanvasMarkEx".into(), json!(v));
            }
            if let Some(v) = &self.webgl_mark {
                p.static_only.insert("WebGLMarkEx".into(), json!(v));
            }
        }

        let d = &mut p.dynamic;
        if let Some(tz) = &self.timezone {
            d.insert("TimeZone".into(), json!(tz));
        }
        if let Some(g) = geo {
            d.insert("Geoposition".into(), json!(g));
        }

        // AdsPower's setWebRTC: "forward" additionally sets STUN/TURN servers it fetches from its cloud.
        match self.webrtc.as_str() {
            "disabled" => {
                p.inline.insert("DisableWebRTC".into(), json!(true));
            }
            "proxy" | "forward" => {
                if self.webrtc_ip.is_empty() {
                    p.dynamic.insert("DisableWebRTC".into(), json!(true)); // exit IP unknown: fail closed
                } else {
                    p.inline.insert("WebRTCAddress".into(), json!(self.webrtc_ip));
                }
                p.dynamic.insert("WebRTCAddress".into(), json!(self.webrtc_ip));
            }
            "disable_udp" => {
                p.static_only
                    .insert("command_line".into(), json!({"webrtc-ip-handling-policy": "disable_non_proxied_udp"}));
                p.args.push("--webrtc-ip-handling-policy=disable_non_proxied_udp".into());
            }
            _ => {} // "local": the real addresses
        }
        p
    }

    /// Contents of the `WebGLFP` file (plain JSON, as AdsPower writes it).
    pub fn webgl_file(&self) -> Option<Value> {
        let (vendor, renderer) = self.webgl.as_ref()?;
        Some(json!({"UNMASKED_VENDOR_WEBGL": vendor, "UNMASKED_RENDERER_WEBGL": renderer, "SUPPORTED_EXTENSIONS": []}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(v: Value) -> FingerprintConfig {
        serde_json::from_value(v).unwrap()
    }

    fn ident<'a>(id: &'a str, country: &'a str) -> Ident<'a> {
        Ident { profile_id: id, country, ip: "", kernel_major: 157 }
    }

    #[test]
    fn accept_language_matches_the_app() {
        let l = |a: &[&str]| accept_language(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(l(&["fr-FR", "fr"]), "fr-FR,fr;q=0.9"); // what the real kernel put on the wire
        assert_eq!(l(&["en-US", "en"]), "en-US,en;q=0.9");
        assert_eq!(l(&["de-DE"]), "de-DE,de;q=0.9");
        // hand-traced through the app's getAccept: "en" is inserted right after "en-US"
        assert_eq!(l(&["en-US", "fr-FR", "fr"]), "en-US,en;q=0.9,fr-FR;q=0.8,fr;q=0.7");
        assert_eq!(l(&[]), "");
    }

    #[test]
    fn hash_and_audio_follow_the_app() {
        assert_eq!(js_hashcode("abc"), 96354.0);
        // char codes 97 + 98 = 195, mod 2000, minus 1000
        assert_eq!(audio_noise("ab"), -805);
        assert_ne!(audio_noise("k1abc23"), 0);
        // no 32-bit wrap: a long id keeps growing like a JS number
        assert!(js_hashcode("kzzzzzzzz") > f64::from(i32::MAX));
        let r = Resolved::resolve(&ident("kzzzzzzzz", ""), &fp(json!({})));
        let cr = r.client_rect_fp.unwrap();
        assert!((-10_000..10_000).contains(&cr), "{cr}");
    }

    #[test]
    fn identity_is_stable_per_profile_and_differs_between_profiles() {
        let cfg = fp(json!({}));
        let a1 = Resolved::resolve(&ident("k1abc23", "fr"), &cfg);
        let a2 = Resolved::resolve(&ident("k1abc23", "fr"), &cfg);
        let b = Resolved::resolve(&ident("k9zzz99", "fr"), &cfg);
        assert_eq!(a1, a2);
        assert_eq!(a1.canvas_mark.as_deref(), Some("k1abc23"));
        assert_ne!(a1.canvas_mark, b.canvas_mark);
        assert_ne!(a1.audio_fp, b.audio_fp);
    }

    #[test]
    fn french_profile() {
        let r = Resolved::resolve(&ident("k1abc23", "FR"), &fp(json!({})));
        assert_eq!(r.languages, ["fr-FR", "fr"]);
        assert_eq!(r.accept_language, "fr-FR,fr;q=0.9");
        assert_eq!(r.timezone.as_deref(), Some("Europe/Paris"));
        assert_eq!((r.platform.as_str(), r.os), ("Win32", Os::Windows));
        assert!(r.user_agent.contains("Chrome/157.0.0.0"));
        assert_eq!(r.ui_language.as_deref(), Some("fr-FR"));
        assert!(r.screen.is_none());
    }

    #[test]
    fn explicit_config_wins_over_country_unless_ip_based() {
        let cfg = fp(json!({"language": ["it-IT", "it"], "automatic_timezone": "0", "timezone": "Europe/Rome",
                            "screen_resolution": "1366_768", "hardware_concurrency": "8", "device_memory": 4,
                            "ua": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"}));
        let r = Resolved::resolve(&ident("x", "fr"), &cfg);
        assert_eq!(r.languages, ["it-IT", "it"]);
        assert_eq!(r.timezone.as_deref(), Some("Europe/Rome"));
        assert_eq!(r.screen, Some((1366, 768)));
        assert_eq!((r.hardware_concurrency, r.device_memory), (Some(8), Some(4)));
        assert_eq!((r.os, r.platform.as_str()), (Os::Mac, "MacIntel"));

        let ip_based = fp(json!({"language": ["it-IT", "it"], "language_switch": "1"}));
        assert_eq!(Resolved::resolve(&ident("x", "fr"), &ip_based).languages, ["fr-FR", "fr"]);
    }

    #[test]
    fn os_can_come_from_random_ua() {
        let cfg = fp(json!({"random_ua": {"ua_browser": "chrome", "ua_system_version": ["Mac OS X 13"]}}));
        let r = Resolved::resolve(&ident("x", ""), &cfg);
        assert_eq!(r.os, Os::Mac);
        assert!(r.user_agent.contains("Macintosh"));
    }

    #[test]
    fn noise_can_be_switched_off() {
        let off = fp(json!({"canvas": "0", "audio": "0", "webgl_image": "0", "client_rects": "0"}));
        let r = Resolved::resolve(&ident("x", ""), &off);
        assert_eq!((&r.canvas_mark, r.audio_fp, &r.webgl_mark, r.client_rect_fp), (&None, None, &None, None));
        let p = r.kernel_params(1, 157);
        for k in ["CanvasMark", "WebGLMark", "AudioFp", "ClientRectFp"] {
            assert!(!p.inline.contains_key(k), "{k}");
        }
        assert!(!p.static_only.contains_key("CanvasMarkEx"));
    }

    #[test]
    fn kernel_params_land_in_the_channels_the_kernel_reads() {
        let cfg =
            fp(json!({"hardware_concurrency": "6", "do_not_track": "true", "latitude": "48.85", "longitude": "2.35"}));
        let p = Resolved::resolve(&ident("k1abc23", "fr"), &cfg).kernel_params(1_700_000_000, 157);
        assert_eq!(p.inline["Platform"], "Win32");
        assert_eq!(p.inline["HardwareConcurrency"], 6);
        assert_eq!(p.inline["EnableDoNotTrack"], true);
        assert_eq!(p.inline["CanvasMark"], "k1abc23");
        assert_eq!(p.inline["StartTime"], 1_700_000_000);
        assert_eq!((&p.inline["DisableBackgroundMode"], &p.inline["ForceProcessExit"]), (&json!(true), &json!(true)));
        assert_eq!(p.inline["TimeZone"], "Europe/Paris");
        assert_eq!(p.dynamic["TimeZone"], "Europe/Paris");
        assert_eq!(p.dynamic["Geoposition"], "48.85,2.35,100");
        assert_eq!(p.static_only["CanvasMarkEx"], "k1abc23");

        let st = p.static_config();
        assert_eq!(st["Langs"], "fr-FR,fr");
        assert_eq!(st["AcceptLang"], "fr-FR,fr;q=0.9");
        assert_eq!(st["UserAgentMetadata"]["platform"], "Windows");
        for k in NOT_IN_STATIC {
            assert!(!st.contains_key(k), "{k} must not be mirrored into the static file");
        }
    }

    #[test]
    fn old_kernels_get_no_ex_keys() {
        let p = Resolved::resolve(&ident("x", ""), &fp(json!({}))).kernel_params(1, 100);
        assert!(!p.static_only.contains_key("CanvasMarkEx") && !p.static_only.contains_key("WebGLMarkEx"));
    }

    #[test]
    fn webrtc_modes() {
        let mk = |mode: &str| {
            let id = Ident { profile_id: "x", country: "", ip: "1.2.3.4", kernel_major: 157 };
            Resolved::resolve(&id, &fp(json!({"webrtc": mode}))).kernel_params(1, 157)
        };
        assert_eq!(mk("disabled").inline["DisableWebRTC"], true);
        let proxy = mk("proxy");
        assert_eq!(
            (&proxy.inline["WebRTCAddress"], &proxy.dynamic["WebRTCAddress"]),
            (&json!("1.2.3.4"), &json!("1.2.3.4"))
        );
        assert!(!proxy.dynamic.contains_key("DisableWebRTC"));
        let no_ip = Resolved::resolve(&ident("x", ""), &fp(json!({"webrtc": "proxy"}))).kernel_params(1, 157);
        assert_eq!(no_ip.dynamic["DisableWebRTC"], true);
        assert!(mk("local").inline.get("DisableWebRTC").is_none() && mk("local").dynamic.is_empty());
        assert_eq!(mk("disable_udp").args, ["--webrtc-ip-handling-policy=disable_non_proxied_udp"]);
    }

    #[test]
    fn webgl_modes() {
        let custom = fp(
            json!({"webgl": "2", "webgl_config": {"unmasked_vendor": "Google Inc. (NVIDIA)", "unmasked_renderer": "ANGLE (NVIDIA)"}}),
        );
        let f = Resolved::resolve(&ident("x", ""), &custom).webgl_file().unwrap();
        assert_eq!(f["UNMASKED_RENDERER_WEBGL"], "ANGLE (NVIDIA)");
        let random = fp(json!({"webgl": "3"}));
        let a = Resolved::resolve(&ident("k1", ""), &random);
        assert_eq!(a, Resolved::resolve(&ident("k1", ""), &random));
        assert!(a.webgl_file().is_some());
        assert!(Resolved::resolve(&ident("x", ""), &fp(json!({"webgl": "0"}))).webgl_file().is_none());
        assert!(Resolved::resolve(&ident("x", ""), &fp(json!({}))).webgl_file().is_none());
    }

    #[test]
    fn pinned_ids_keep_their_sign_and_zero_becomes_one() {
        let cfg = fp(json!({"audio_id": "-77", "client_rects_id": 0, "canvas_id": "mine"}));
        let r = Resolved::resolve(&ident("x", ""), &cfg);
        assert_eq!((r.audio_fp, r.client_rect_fp, r.canvas_mark.as_deref()), (Some(-77), Some(1), Some("mine")));
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
