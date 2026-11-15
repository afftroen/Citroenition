//! What the kernel needs around its command line: the process environment (fonts, UI locale, shader
//! dialect), the GL backend that makes WebGL agree with the claimed OS, and the persona transport that keeps
//! the seed and the proxy credentials off the browser's command line.
//!
//! Everything here mirrors what the Clearcote SDK does for its own launches; the sources are cited per item.

use crate::fingerprint::{Gpu, Os, Resolved};
use crate::kernel::Kernel;
use crate::store::Home;
use std::fs;
use std::path::{Path, PathBuf};

pub const PERSONA_ENV: &str = "CLEARCOTE_PERSONA_ARGS";
pub const FROM_ENV_SWITCH: &str = "persona-from-env";
pub const SHADER_DIALECT_ENV: &str = "CLEARCOTE_SHADER_DIALECT";
/// The engine republishes the payload for its children; Windows caps one variable at 32,767 characters.
const MAX_PAYLOAD: usize = 30_000;

/// Switches the engine accepts through [`PERSONA_ENV`], besides `fingerprint*` and `canvas-bridge-*`.
const TRANSPORTED: &[&str] = &[
    "disable-canvas-noise",
    "disable-fingerprint-noise",
    "disable-fingerprint-voices",
    "disable-gpu-fingerprint",
    "disable-gpu-string-spoof",
    "proxy-auth",
    "socks5-credentials",
    "webrtc-ip",
];

fn is_transported(name: &str) -> bool {
    name == "fingerprint"
        || name.starts_with("fingerprint-")
        || name.starts_with("canvas-bridge-")
        || TRANSPORTED.contains(&name)
}

/// `(name, value)` of a `--name[=value]` argument.
fn switch(arg: &str) -> Option<(&str, &str)> {
    let rest = arg.strip_prefix("--").filter(|r| !r.is_empty())?;
    let (name, value) = rest.split_once('=').unwrap_or((rest, ""));
    (!name.is_empty()).then_some((name, value))
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Move the persona switches (seed, every `fingerprint-*`, proxy credentials, ...) out of `args` into the
/// `CLEARCOTE_PERSONA_ARGS` value, and add `--persona-from-env`. A browser's command line is readable by
/// every local user (`/proc/<pid>/cmdline`); its environment is not.
///
/// Chromium keeps the LAST copy of a repeated switch while the engine adopts the FIRST payload entry of a
/// name, so each name goes in once with its last value. Returns `None` (args untouched) when nothing
/// qualifies or the payload would exceed the engine's cap.
pub fn persona_to_env(args: &[String]) -> Option<(Vec<String>, String)> {
    let mut last: Vec<(&str, usize)> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if let Some((name, _)) = switch(a).filter(|(n, _)| is_transported(n)) {
            match last.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = i,
                None => last.push((name, i)),
            }
        }
    }
    if last.is_empty() {
        return None;
    }
    let mut idx: Vec<usize> = last.iter().map(|(_, i)| *i).collect();
    idx.sort_unstable();
    let mut raw = Vec::new();
    for i in &idx {
        let (n, v) = switch(&args[*i])?;
        for part in [n, v] {
            raw.extend_from_slice(part.as_bytes());
            raw.push(0);
        }
    }
    let payload = base64(&raw);
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut kept: Vec<String> =
        args.iter().filter(|a| !switch(a).is_some_and(|(n, _)| is_transported(n))).cloned().collect();
    kept.push(format!("--{FROM_ENV_SWITCH}"));
    Some((kept, payload))
}

/// `LANGUAGE=<ui locale>` on Linux: Chrome there takes its UI locale from the environment, and older engines
/// ignore `--lang` for it. `LANGUAGE` only steers message catalogues (it leaves number parsing and fontconfig's
/// default language alone). `de` -> `de`, `en-GB` -> `en_GB`.
pub fn locale_env(args: &[String]) -> Option<(String, String)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let lang = args.iter().rev().find_map(|a| a.strip_prefix("--lang="))?.trim();
    (!lang.is_empty()).then(|| ("LANGUAGE".to_owned(), lang.replace('-', "_")))
}

/// `hlsl` for a Windows claim on a non-Windows host: the GPU process otherwise answers
/// `WEBGL_debug_shaders.getTranslatedShaderSource()` with SPIR-V or GLSL next to a "Direct3D11" renderer string,
/// a pairing no real machine produces. Not applied when the page sees the real GPU or a non-Direct3D renderer.
pub fn shader_dialect(r: &Resolved) -> Option<&'static str> {
    if Os::host() == Os::Windows || r.os != Os::Windows {
        return None;
    }
    match &r.gpu {
        Gpu::Real => None,
        Gpu::Custom(_, renderer) if !renderer.to_ascii_lowercase().contains("direct3d") => None,
        _ => Some("hlsl"),
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The fontconfig file that points the kernel at its bundled, metric-compatible Windows font clones (Segoe UI ->
/// Selawik, Arial -> Arimo, ...). Without it a Linux host shows the page its own font list, which contradicts a
/// Windows claim. Written to `<home>/cache/`, not next to the binary, so a read-only or shared kernel directory
/// works. `None` when this kernel has no font bundle.
pub fn fonts_conf(home: &Home, kernel: &Kernel) -> Option<PathBuf> {
    let template = fs::read_to_string(kernel.fonts_template()?).ok()?;
    let cache = home.root.join("cache");
    let fc_cache = cache.join("fontconfig");
    fs::create_dir_all(&fc_cache).ok()?;
    let fonts_dir = fs::canonicalize(kernel.dir().join("fonts")).ok()?;
    let conf = template
        .replace("@FONTS_DIR@", &xml(&fonts_dir.to_string_lossy()))
        .replace("@CACHE_DIR@", &xml(&fc_cache.to_string_lossy()));
    let path = cache.join(format!("fonts-{}.conf", if kernel.version.is_empty() { "kernel" } else { &kernel.version }));
    if fs::read_to_string(&path).ok().as_deref() != Some(conf.as_str()) {
        let tmp = cache.join(format!(".fonts-{}.tmp", std::process::id()));
        fs::write(&tmp, &conf).ok()?;
        fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}

/// Does `DISPLAY` name an X server this process can reach? ANGLE's OpenGL backend opens an X display even for
/// a headless browser.
pub fn x_display_available(display: Option<&str>) -> bool {
    let Some(d) = display.map(str::trim).filter(|d| !d.is_empty()) else { return false };
    match d.strip_prefix(':') {
        Some(rest) => {
            let n = rest.split('.').next().unwrap_or("");
            !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
        }
        None => true, // `host:N` cannot be probed cheaply; trust it
    }
}

/// The WebGL backend flags for a Linux host, chosen from the OS the page is told it runs on.
///
/// * every claim -> `--ignore-gpu-blocklist`: a GPU-less host (a VPS, a container, Xvfb) has no allow-listed
///   GPU, and without the override the engine switches WebGL off entirely, which is itself a loud signal.
/// * Windows claim -> SwiftShader: it reports the 4096 vertex-uniform limit real Direct3D11 machines report
///   (Mesa's OpenGL backend clamps to 1024), and the HLSL shader dialect makes its translations match.
/// * Linux / macOS claim -> the engine's default backend. Measured on the real kernel: the default and
///   `--use-angle=gl` give working WebGL, `--use-angle=gl-egl` gives none on a display-only host.
///
/// A backend the caller chose in `user_args` is kept; only the blocklist override is added.
pub fn gpu_backend_args(claim: Os, user_args: &[String]) -> Vec<String> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !user_args.iter().any(|a| a == "--ignore-gpu-blocklist") {
        out.push("--ignore-gpu-blocklist".to_owned());
    }
    let own_backend = user_args.iter().any(|a| a.starts_with("--use-angle=") || a.starts_with("--use-gl="));
    if claim == Os::Windows && !own_backend {
        out.push("--use-angle=swiftshader-webgl".to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{FingerprintConfig, Ident};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
        assert_eq!(base64(&[0xfb, 0xff, 0xfe]), "+//+");
    }

    #[test]
    fn persona_switches_move_to_the_environment_once_with_their_last_value() {
        let args = s(&[
            "--user-data-dir=/d",
            "--fingerprint=seed1",
            "--fingerprint-platform=windows",
            "--timezone=Europe/Paris",
            "--socks5-credentials=u:p",
            "--fingerprint=seed2",
            "--proxy-server=socks5://h:1",
            "about:blank",
        ]);
        let (kept, payload) = persona_to_env(&args).unwrap();
        assert_eq!(
            kept,
            s(&[
                "--user-data-dir=/d",
                "--timezone=Europe/Paris",
                "--proxy-server=socks5://h:1",
                "about:blank",
                "--persona-from-env"
            ])
        );
        // decode: name\0value\0 pairs, in order of each name's last occurrence
        let expect = b"fingerprint-platform\0windows\0socks5-credentials\0u:p\0fingerprint\0seed2\0";
        assert_eq!(payload, base64(expect));
        assert!(persona_to_env(&s(&["--timezone=x", "about:blank"])).is_none());
    }

    #[test]
    fn an_oversized_payload_stays_on_the_command_line() {
        let big = format!("--fingerprint-gpu-renderer={}", "x".repeat(MAX_PAYLOAD));
        assert!(persona_to_env(&[big]).is_none());
    }

    #[test]
    fn ui_locale_becomes_language() {
        let l = locale_env(&s(&["--lang=de", "--x", "--lang=en-GB"]));
        if cfg!(target_os = "linux") {
            assert_eq!(l, Some(("LANGUAGE".to_owned(), "en_GB".to_owned())));
            assert_eq!(locale_env(&s(&["--lang="])), None);
            assert_eq!(locale_env(&s(&["--x"])), None);
        } else {
            assert_eq!(l, None);
        }
    }

    fn resolved(os: &str, cfg: serde_json::Value) -> Resolved {
        let fp: FingerprintConfig = serde_json::from_value(cfg).unwrap();
        Resolved::resolve(&Ident { profile_id: "x", country: "fr", ip: "", persona_os: os }, &fp)
    }

    #[test]
    fn hlsl_only_for_a_windows_claim_showing_a_direct3d_gpu_on_a_non_windows_host() {
        if Os::host() == Os::Windows {
            return;
        }
        assert_eq!(shader_dialect(&resolved("windows", serde_json::json!({}))), Some("hlsl"));
        assert_eq!(shader_dialect(&resolved("linux", serde_json::json!({}))), None);
        assert_eq!(shader_dialect(&resolved("windows", serde_json::json!({"webgl": "0"}))), None);
        let d3d = serde_json::json!({"webgl": "2", "webgl_config": {"unmasked_vendor": "Google Inc. (Intel)", "unmasked_renderer": "ANGLE (Intel, Direct3D11)"}});
        let mesa = serde_json::json!({"webgl": "2", "webgl_config": {"unmasked_vendor": "Mesa", "unmasked_renderer": "llvmpipe"}});
        assert_eq!(shader_dialect(&resolved("windows", d3d)), Some("hlsl"));
        assert_eq!(shader_dialect(&resolved("windows", mesa)), None);
    }

    #[test]
    fn gpu_backend_follows_the_claim() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let none: Vec<String> = Vec::new();
        let blocklist = "--ignore-gpu-blocklist";
        assert_eq!(gpu_backend_args(Os::Windows, &none), s(&[blocklist, "--use-angle=swiftshader-webgl"]));
        assert_eq!(gpu_backend_args(Os::Linux, &none), s(&[blocklist]), "the engine default measured to work");
        assert_eq!(gpu_backend_args(Os::Mac, &none), s(&[blocklist]));
        // the caller's own backend is kept, and the blocklist override is never doubled
        let own = s(&["--use-angle=gl"]);
        assert_eq!(gpu_backend_args(Os::Windows, &own), s(&[blocklist]));
        let both = s(&["--use-gl=egl", blocklist]);
        assert!(gpu_backend_args(Os::Windows, &both).is_empty());
        assert!(gpu_backend_args(Os::Linux, &both).is_empty());
    }

    #[test]
    fn x_display_needs_a_socket_or_a_remote_host() {
        assert!(!x_display_available(None) && !x_display_available(Some("")) && !x_display_available(Some(":x")));
        assert!(!x_display_available(Some(":987654")));
        assert!(x_display_available(Some("somehost:0")));
    }

    #[test]
    fn fonts_conf_fills_in_the_bundle_and_cache_paths() {
        let root = std::env::temp_dir().join(format!("citro-fonts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let kdir = root.join("kernels").join("clearcote-151.0.1.2");
        fs::create_dir_all(kdir.join("fonts")).unwrap();
        fs::write(kdir.join("chrome"), b"").unwrap();
        fs::write(kdir.join("fonts/fonts.conf.template"), "<dir>@FONTS_DIR@</dir><cachedir>@CACHE_DIR@</cachedir>")
            .unwrap();
        let kernel = Kernel::from_path(kdir.join("chrome")).unwrap();
        let home = Home { root: root.join("home") };
        let path = fonts_conf(&home, &kernel).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("<dir>{}</dir>", fs::canonicalize(kdir.join("fonts")).unwrap().display())),
            "{text}"
        );
        assert!(text.contains("/cache/fontconfig</cachedir>") && !text.contains('@'));
        assert_eq!(fonts_conf(&home, &kernel), Some(path)); // idempotent
        fs::remove_file(kdir.join("fonts/fonts.conf.template")).unwrap();
        assert!(fonts_conf(&home, &kernel).is_none(), "no template: no bundle");
        let _ = fs::remove_dir_all(&root);
    }
}
