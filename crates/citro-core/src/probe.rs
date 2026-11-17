//! What a profile's browser really shows to sites. Most of the persona is derived inside the kernel from the seed
//! ("based on seed"), so the only honest way to show the values is to ask the browser: start the kernel headless
//! with the profile's persona in a throwaway folder, read the values a fingerprinting page reads, stop it.
//!
//! The result depends only on the persona switches, the user's launch arguments and the kernel, so it is cached
//! under `cache/probe/<digest>.json` and a second look is instant.

use crate::fingerprint::{Ident, Resolved};
use crate::kernel::Kernel;
use crate::launch::{self, LaunchOptions};
use crate::profile::{Profile, ProxyConfig};
use crate::store::Home;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bump when the script below changes, so old cached answers are not reused.
const VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub user_agent: String,
    pub platform: String,
    pub cores: u32,
    /// GB, as `navigator.deviceMemory` reports it.
    pub memory: f64,
    pub screen: (u32, u32),
    pub avail_screen: (u32, u32),
    pub pixel_ratio: f64,
    pub color_depth: u32,
    pub languages: Vec<String>,
    pub timezone: String,
    pub gpu_vendor: String,
    pub gpu_renderer: String,
    /// Short hashes of what the canvas, WebGL, audio and ClientRects tests produce: the noise a site measures.
    pub canvas_hash: String,
    pub webgl_hash: String,
    pub audio_hash: String,
    pub rects_hash: String,
    pub voices: u32,
    /// `audioinput`, `audiooutput`, `videoinput` counts.
    pub media_devices: (u32, u32, u32),
}

const SCRIPT: &str = r#"(async () => {
  const h = s => { let x = 0x811c9dc5; for (let i = 0; i < s.length; i++) { x ^= s.charCodeAt(i); x = Math.imul(x, 0x01000193) >>> 0; } return x.toString(16).toUpperCase().padStart(8, '0'); };
  const o = {};
  o.user_agent = navigator.userAgent; o.platform = navigator.platform;
  o.cores = navigator.hardwareConcurrency || 0; o.memory = navigator.deviceMemory || 0;
  o.screen = [screen.width, screen.height]; o.avail_screen = [screen.availWidth, screen.availHeight];
  o.pixel_ratio = devicePixelRatio; o.color_depth = screen.colorDepth;
  o.languages = [...navigator.languages]; o.timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || '';
  o.gpu_vendor = ''; o.gpu_renderer = ''; o.webgl_hash = '';
  try {
    const c = document.createElement('canvas'); c.width = 64; c.height = 64;
    const g = c.getContext('webgl', { preserveDrawingBuffer: true });
    const e = g.getExtension('WEBGL_debug_renderer_info');
    o.gpu_vendor = String(g.getParameter(e ? e.UNMASKED_VENDOR_WEBGL : g.VENDOR));
    o.gpu_renderer = String(g.getParameter(e ? e.UNMASKED_RENDERER_WEBGL : g.RENDERER));
    g.clearColor(0.25, 0.5, 0.75, 1); g.clear(g.COLOR_BUFFER_BIT);
    o.webgl_hash = h(c.toDataURL());
  } catch (_) {}
  try {
    const c = document.createElement('canvas'); c.width = 240; c.height = 60;
    const x = c.getContext('2d');
    x.textBaseline = 'top'; x.font = '16px Arial'; x.fillStyle = '#f60'; x.fillRect(120, 1, 60, 20);
    x.fillStyle = '#069'; x.fillText('Citroenition \u{1F34B} 1.0', 2, 15);
    x.fillStyle = 'rgba(102, 204, 0, 0.7)'; x.fillText('Citroenition \u{1F34B} 1.0', 4, 17);
    o.canvas_hash = h(c.toDataURL());
  } catch (_) { o.canvas_hash = ''; }
  try {
    const a = new OfflineAudioContext(1, 5000, 44100);
    const osc = a.createOscillator(); osc.type = 'triangle'; osc.frequency.value = 10000;
    const k = a.createDynamicsCompressor();
    osc.connect(k); k.connect(a.destination); osc.start(0);
    const buf = await a.startRendering();
    const d = buf.getChannelData(0); let s = 0; for (let i = 4500; i < 5000; i++) s += Math.abs(d[i]);
    o.audio_hash = h(s.toString());
  } catch (_) { o.audio_hash = ''; }
  try {
    const sp = document.createElement('span'); sp.style.cssText = 'font: 15px serif; position: absolute';
    sp.textContent = 'The quick brown fox jumps'; document.body.appendChild(sp);
    const r = sp.getBoundingClientRect(); o.rects_hash = h([r.x, r.y, r.width, r.height].join(',')); sp.remove();
  } catch (_) { o.rects_hash = ''; }
  o.voices = await new Promise(res => {
    const n = () => speechSynthesis.getVoices().length;
    if (n()) return res(n());
    speechSynthesis.onvoiceschanged = () => res(n());
    setTimeout(() => res(n()), 800);
  }).catch(() => 0);
  try {
    const ds = await navigator.mediaDevices.enumerateDevices();
    const c = k => ds.filter(d => d.kind === k).length;
    o.media_devices = [c('audioinput'), c('audiooutput'), c('videoinput')];
  } catch (_) { o.media_devices = [0, 0, 0]; }
  return JSON.stringify(o);
})()"#;

/// The cache key: everything that can change what the browser shows.
pub fn key(kernel: &Kernel, p: &Profile) -> String {
    use sha2::Digest;
    let r = resolved(p);
    let mut text = format!("v{VERSION}\n{}\n{}\n", kernel.version, kernel.binary.display());
    for s in r.switches() {
        text.push_str(&s);
        text.push('\n');
    }
    for a in &p.launch_args {
        text.push_str(a.trim());
        text.push('\n');
    }
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))[..24].to_owned()
}

fn resolved(p: &Profile) -> Resolved {
    Resolved::resolve(
        &Ident { profile_id: &p.profile_id, country: &p.country, ip: &p.ip, persona_os: &p.persona_os },
        &p.fingerprint_config,
    )
}

/// The cached answer for this persona, if there is one.
pub fn cached(home: &Home, kernel: &Kernel, p: &Profile) -> Option<Probe> {
    let path = home.root.join("cache/probe").join(format!("{}.json", key(kernel, p)));
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Ask the browser. `p` may be unsaved (a form being edited); only its persona is used: no proxy, no cookies, no
/// extensions, no start pages, and the profile's own data folder is never touched.
pub fn probe(home: &Home, kernel: &Kernel, p: &Profile) -> Result<Probe> {
    if let Some(c) = cached(home, kernel, p) {
        return Ok(c);
    }
    let key = key(kernel, p);
    let seed = resolved(p).seed;
    let mut q = p.clone();
    q.profile_id = "probe".into();
    q.fingerprint_config.0.insert("seed".into(), Value::String(seed));
    q.user_proxy_config = ProxyConfig::default();
    q.tabs = Vec::new();
    q.domain_name = String::new();
    q.cookie = String::new();
    q.group_id = String::new();

    let scratch = home.root.join("cache").join(format!("probe-run-{}-{key}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let tmp = Home { root: scratch.clone() };
    let outcome = run(&tmp, kernel, &q);
    let _ = fs::remove_dir_all(&scratch);
    let probe = outcome?;
    let dir = home.root.join("cache/probe");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{key}.json")), serde_json::to_vec_pretty(&probe)?)?;
    Ok(probe)
}

fn run(tmp: &Home, kernel: &Kernel, q: &Profile) -> Result<Probe> {
    let _ = crate::kernel_patch::ensure(kernel);
    let opts = LaunchOptions { headless: true, last_opened_tabs: false, ..LaunchOptions::default() };
    let prep = launch::prepare(tmp, kernel, q, &opts)?;
    let mut cmd = Command::new(&kernel.binary);
    for (k, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("CITRO_")) {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .args(&prep.args)
        .envs(prep.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let pid = child.id() as i32;
    let result = (|| -> Result<Probe> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let (port, path) = loop {
            if let Some(found) = launch::read_devtools_port(&prep.data_dir).filter(|(p, _)| launch::listening(*p)) {
                break found;
            }
            if let Some(st) = child.try_wait()? {
                return Err(Error::KernelExited { status: st.code(), stderr: String::new() });
            }
            if Instant::now() >= deadline {
                return Err(Error::LaunchTimeout);
            }
            std::thread::sleep(Duration::from_millis(80));
        };
        // a fresh page, then its own DevTools socket
        let mut browser = crate::cdp::Cdp::connect(&format!("ws://127.0.0.1:{port}{path}"))?;
        let t =
            browser.call("Target.createTarget", json!({ "url": format!("http://127.0.0.1:{port}/json/version") }))?;
        let target = t["targetId"].as_str().ok_or_else(|| Error::Invalid("no page target".into()))?;
        let ws = format!("ws://127.0.0.1:{port}/devtools/page/{target}");
        let mut cdp = crate::cdp::Cdp::connect(&ws)?;
        // wait until the local page has loaded (a secure context: deviceMemory and friends exist only there)
        loop {
            let r = cdp.call(
                "Runtime.evaluate",
                json!({ "expression": "location.protocol + document.readyState", "returnByValue": true }),
            );
            if r.ok().is_some_and(|v| v["result"]["value"] == "http:complete") {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::LaunchTimeout);
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        let r = cdp.call(
            "Runtime.evaluate",
            json!({ "expression": SCRIPT, "awaitPromise": true, "returnByValue": true, "timeout": 10000 }),
        )?;
        let text = r["result"]["value"]
            .as_str()
            .ok_or_else(|| Error::Invalid(format!("the browser did not answer: {}", r["exceptionDetails"])))?;
        Ok(serde_json::from_str(text)?)
    })();
    launch::signal(-pid, libc::SIGKILL);
    let _ = child.wait();
    result
}
