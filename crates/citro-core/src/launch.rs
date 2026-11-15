//! Starting, tracking and stopping SunBrowser kernels.
//!
//! This is what AdsPower's desktop app does when you open a profile (see
//! `docs/REVERSE_ENGINEERING.md`): write the `StaticConfig` / `DynamicConfig` / `WebGLFP` files, spawn the
//! kernel with `--extended-parameters=<shuffled-base64 JSON>`, and wait for its DevTools endpoint.
//!
//! A running kernel is recorded in `run/<profile_id>.json`. The record is trusted only while the pid is
//! alive *and* its command line still carries the profile's `--user-data-dir`, so a recycled pid is never
//! mistaken for a browser.

use crate::codec;
use crate::fingerprint::{Ident, Resolved};
use crate::kernel::Kernel;
use crate::profile::Profile;
use crate::store::Home;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    /// Extra kernel flags. When non-empty they replace the profile's own `launch_args` (AdsPower: the
    /// API's flags win).
    pub launch_args: Vec<String>,
    /// Reopen the tabs of the previous session instead of the profile's start pages.
    pub last_opened_tabs: bool,
    /// Keep Chromium's sandbox. AdsPower's Linux build always passes `--no-sandbox`, and so do we by
    /// default: the sandbox needs user namespaces, which containers and many servers lack.
    pub sandbox: bool,
    pub startup_timeout: Duration,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            headless: false,
            launch_args: Vec::new(),
            last_opened_tabs: true,
            sandbox: false,
            startup_timeout: Duration::from_secs(30),
        }
    }
}

/// A running browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Running {
    pub profile_id: String,
    pub pid: u32,
    pub debug_port: u16,
    /// `ws://127.0.0.1:<port>/devtools/browser/<guid>`
    pub ws: String,
    pub started: i64,
    pub kernel: PathBuf,
    pub kernel_major: u32,
    pub headless: bool,
}

/// Everything needed to spawn the kernel; produced by [`prepare`].
#[derive(Debug, Clone)]
pub struct Prepared {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub data_dir: PathBuf,
}

/// Flags every launch carries: AdsPower's default set minus `--enable-automation`, which would set
/// `navigator.webdriver` and show the "controlled by automated test software" bar.
const BASE_FLAGS: &[&str] = &[
    "--protected-disable-safe-open",
    "--no-first-run",
    "--no-default-browser-check",
    "--password-store=basic",
    "--use-mock-keychain",
    "--disable-dev-shm-usage",
    "--disable-sync",
    "--disable-breakpad",
    "--disable-background-networking",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--disable-default-apps",
    "--disable-hang-monitor",
    "--disable-ipc-flooding-protection",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--force-color-profile=srgb",
    "--metrics-recording-only",
    "--export-tagged-pdf",
    "--disable-blink-features=AutomationControlled",
    "--enable-blink-features=IdleDetection,Fledge",
    "--enable-features=NetworkService,NetworkServiceInProcess,LoadCryptoTokenExtension",
    "--disable-features=Translate,BackForwardCache",
];

const LIST_FLAGS: [&str; 4] =
    ["--enable-features=", "--disable-features=", "--enable-blink-features=", "--disable-blink-features="];

/// Chromium honours only the *last* `--enable-features=` / `--disable-features=` (and `-blink-features`)
/// switch, so every occurrence is folded into the first one, duplicates removed.
fn merge_list_flags(args: Vec<String>) -> Vec<String> {
    let mut lists: Vec<(&str, Vec<String>, Option<usize>)> =
        LIST_FLAGS.iter().map(|k| (*k, Vec::new(), None)).collect();
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        match lists.iter_mut().find(|(k, _, _)| a.starts_with(*k)) {
            Some((key, list, slot)) => {
                if slot.is_none() {
                    *slot = Some(out.len());
                    out.push(String::new()); // filled in below
                }
                for v in a[key.len()..].split(',').filter(|v| !v.is_empty()) {
                    if !list.iter().any(|x| x == v) {
                        list.push(v.to_owned());
                    }
                }
            }
            None => out.push(a),
        }
    }
    for (key, list, slot) in lists {
        if let Some(i) = slot {
            out[i] = if list.is_empty() { String::new() } else { format!("{key}{}", list.join(",")) };
        }
    }
    out.retain(|a| !a.is_empty());
    out
}

/// The `N` of `Chrome/N.x.y.z` (AdsPower's `getChromeVersion`).
fn chrome_major(ua: &str) -> Option<u32> {
    ua.split("Chrome/").nth(1)?.split('.').next()?.parse().ok()
}

fn host_for_url(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_owned() }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

fn data_dir_of(home: &Home, id: &str) -> PathBuf {
    let dir = home.profile_dir(id).join("data");
    fs::canonicalize(&dir).unwrap_or(dir)
}

pub fn kernel_log(home: &Home, id: &str) -> PathBuf {
    home.profile_dir(id).join("kernel.log")
}

/// Write the config files for `p` and compute the kernel's command line. Spawns nothing.
pub fn prepare(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions, start_time: i64) -> Result<Prepared> {
    let id = p.profile_id.as_str();
    if id.is_empty() {
        return Err(Error::Invalid("profile has no id".into()));
    }
    let fp = &p.fingerprint_config;
    let resolved =
        Resolved::resolve(&Ident { profile_id: id, country: &p.country, ip: &p.ip, kernel_major: kernel.major }, fp);
    let mut params = resolved.kernel_params(start_time, kernel.major);
    params.inline.insert("UserId".into(), json!(p.serial_number.to_string()));

    let dir = home.profile_dir(id);
    let (data_dir, cfg_dir) = (dir.join("data"), dir.join("cfg"));
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&cfg_dir)?;
    let data_dir = fs::canonicalize(&data_dir)?;
    let cfg_dir = fs::canonicalize(&cfg_dir)?;

    // The static file: AdsPower's own mechanism for proxy credentials is `ProxyChain` (measured: the
    // kernel authenticates through it; credentials inside the `--proxy-server` URL are ignored).
    let hop = p.user_proxy_config.hop();
    let mut static_cfg = params.static_config();
    if let Some(h) = hop.as_ref().filter(|h| !h.account.is_empty()) {
        static_cfg.insert("ProxyChain".into(), json!([h]));
    }

    let mut inline = params.inline;
    let mut put = |name: &str, bytes: Vec<u8>, key: &str| -> Result<()> {
        let path = cfg_dir.join(name);
        write_private(&path, &bytes)?;
        inline.insert(key.into(), json!(path.display().to_string()));
        Ok(())
    };
    put("static.cfg", codec::encode_json(&Value::Object(static_cfg))?.into_bytes(), "StaticConfig")?;
    put("dynamic.cfg", codec::encode_json(&Value::Object(params.dynamic))?.into_bytes(), "DynamicConfig")?;
    if let Some(gl) = resolved.webgl_file() {
        put("webgl.json", serde_json::to_vec(&gl)?, "WebGLFP")?;
    }

    let mut args: Vec<String> = vec![
        format!("--user-data-dir={}", data_dir.display()),
        "--remote-debugging-port=0".into(),
        format!("--extended-parameters={}", codec::encode_json(&Value::Object(inline))?),
    ];
    args.extend(BASE_FLAGS.iter().map(|s| (*s).to_owned()));
    args.push(format!("--user-agent={}", resolved.user_agent));

    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(l) = &resolved.ui_language {
        args.push(format!("--lang={l}"));
        env.push(("LANGUAGE".into(), l.clone()));
    }
    if let Some(l) = resolved.languages.first() {
        env.push(("ISOLATE_DEFAULT_LOCALE".into(), l.clone()));
    }
    if let Some((w, h)) = resolved.screen {
        args.push(format!("--window-size={w},{h}"));
        args.push(format!("--ash-host-window-bounds={w}x{h}"));
    }
    // JA3: AdsPower toggles TLS extension permutation by the UA's Chrome version.
    match chrome_major(&resolved.user_agent) {
        Some(v) if v > 105 => args.push("--enable-features=PermuteTLSExtensions".into()),
        Some(_) => args.push("--disable-features=PermuteTLSExtensions".into()),
        None => {}
    }
    let gpu_off = fp.str("gpu").as_deref() == Some("2");
    if gpu_off {
        args.push("--disable-gpu".into());
    }
    match &hop {
        Some(h) => args.push(format!("--proxy-server={}://{}:{}", h.scheme, host_for_url(&h.host), h.port)),
        None => args.push("--no-proxy-server".into()),
    }
    if opts.headless {
        args.extend(["--headless=new", "--hide-scrollbars", "--mute-audio"].map(str::to_owned));
        if !gpu_off {
            // a software GL stack, so WebGL (and its fingerprint) still works without a GPU
            args.extend(
                ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"].map(str::to_owned),
            );
        }
    }
    if !opts.sandbox {
        args.push("--no-sandbox".into());
    }
    args.extend(params.args);
    let user_args = if opts.launch_args.is_empty() { &p.launch_args } else { &opts.launch_args };
    args.extend(user_args.iter().map(|a| a.trim().to_owned()).filter(|a| a.starts_with("--")));
    let mut args = merge_list_flags(args);

    if opts.last_opened_tabs && data_dir.join("Default/Sessions").is_dir() {
        args.push("--restore-last-session".into());
    } else {
        let mut urls: Vec<String> = p.tabs.iter().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
        if urls.is_empty() && !p.domain_name.trim().is_empty() {
            let d = p.domain_name.trim();
            urls.push(if d.contains("://") { d.to_owned() } else { format!("https://{d}") });
        }
        if urls.is_empty() {
            urls.push("about:blank".into());
        }
        args.extend(urls);
    }
    Ok(Prepared { args, env, data_dir })
}

fn read_devtools_port(data_dir: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(data_dir.join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    (port != 0 && path.starts_with('/')).then(|| (port, path.to_owned()))
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_millis(500)).is_ok()
}

/// The last `max` bytes of a log file, lossily decoded.
fn tail(path: &Path, max: u64) -> String {
    let Ok(mut f) = fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if f.seek(SeekFrom::Start(len.saturating_sub(max))).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).trim().to_owned()
}

fn signal(pid: i32, sig: i32) {
    // SAFETY: kill(2) takes plain integers; a stale pid at worst yields ESRCH.
    let _ = unsafe { libc::kill(pid, sig) };
}

/// Does a `/proc/<pid>/cmdline` blob carry `--user-data-dir=<data_dir>`?
///
/// SunBrowser rewrites its own argv into ONE space-joined string (measured: argc == 1), so whole
/// NUL-separated arguments cannot be compared. Match on token boundaries instead: the flag must start the
/// blob or follow a separator, and end the blob or precede one (so `.../data` never matches `.../data2`).
fn cmdline_has_data_dir(raw: &[u8], data_dir: &Path) -> bool {
    let needle = format!("--user-data-dir={}", data_dir.display());
    let n = needle.as_bytes();
    let hay: Vec<u8> = raw.iter().map(|b| if *b == 0 { b' ' } else { *b }).collect();
    hay.windows(n.len())
        .enumerate()
        .any(|(i, w)| w == n && (i == 0 || hay[i - 1] == b' ') && hay.get(i + n.len()).is_none_or(|c| *c == b' '))
}

fn pid_is_browser(pid: u32, data_dir: &Path) -> bool {
    fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|raw| cmdline_has_data_dir(&raw, data_dir))
}

/// The record of a live browser for this profile id, if any. Stale records are deleted.
pub fn running(home: &Home, id: &str) -> Option<Running> {
    let bytes = fs::read(home.run_file(id)).ok()?;
    let r: Running = serde_json::from_slice(&bytes).ok()?;
    if r.profile_id == id && pid_is_browser(r.pid, &data_dir_of(home, id)) {
        Some(r)
    } else {
        let _ = fs::remove_file(home.run_file(id));
        None
    }
}

/// Every live browser.
pub fn list_running(home: &Home) -> Vec<Running> {
    let Ok(entries) = fs::read_dir(home.run_dir()) else { return Vec::new() };
    let mut out: Vec<Running> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            running(home, name.strip_suffix(".json")?)
        })
        .collect();
    out.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    out
}

/// Open the profile. If it is already running its existing endpoints are returned.
pub fn start(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<Running> {
    if let Some(r) = running(home, &p.profile_id) {
        return Ok(r);
    }
    let start_time = now();
    let prep = prepare(home, kernel, p, opts, start_time)?;
    let _ = fs::remove_file(prep.data_dir.join("DevToolsActivePort"));

    let log_path = kernel_log(home, &p.profile_id);
    let log = OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path)?;
    let mut child = Command::new(&kernel.binary)
        .args(&prep.args)
        .envs(prep.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        // its own process group: closing the launching terminal does not take the browser down, and
        // `stop` can sweep every helper process with one signal
        .process_group(0)
        .spawn()?;
    let pid = child.id();

    let deadline = Instant::now() + opts.startup_timeout;
    let (port, path) = loop {
        if let Some((port, path)) = read_devtools_port(&prep.data_dir).filter(|(port, _)| listening(*port)) {
            break (port, path);
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::KernelExited { status: status.code(), stderr: tail(&log_path, 2000) });
        }
        if Instant::now() >= deadline {
            signal(-(pid as i32), libc::SIGKILL);
            let _ = child.wait();
            return Err(Error::LaunchTimeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let r = Running {
        profile_id: p.profile_id.clone(),
        pid,
        debug_port: port,
        ws: format!("ws://127.0.0.1:{port}{path}"),
        started: start_time,
        kernel: kernel.binary.clone(),
        kernel_major: kernel.major,
        headless: opts.headless,
    };
    fs::create_dir_all(home.run_dir())?;
    write_private(&home.run_file(&r.profile_id), &serde_json::to_vec_pretty(&r)?)?;

    // Reap the child when it exits (no zombies in a long-lived server) and drop our run record, unless a
    // newer launch has already replaced it.
    let run_file = home.run_file(&r.profile_id);
    let _ = std::thread::Builder::new().name(format!("reap-{pid}")).spawn(move || {
        let _ = child.wait();
        let ours = fs::read(&run_file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Running>(&b).ok())
            .is_some_and(|x| x.pid == pid);
        if ours {
            let _ = fs::remove_file(&run_file);
        }
    });
    Ok(r)
}

/// Close the profile: SIGTERM so Chromium shuts down cleanly (cookies and session are flushed), then
/// SIGKILL for the whole process group if it lingers. `Ok(false)` = it was not running.
pub fn stop(home: &Home, id: &str) -> Result<bool> {
    let Some(r) = running(home, id) else { return Ok(false) };
    let data_dir = data_dir_of(home, id);
    let pid = r.pid as i32;
    signal(pid, libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_is_browser(r.pid, &data_dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    // helper processes (zygote, GPU, renderers) normally die with the main process; sweep any straggler
    signal(-pid, libc::SIGKILL);
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_is_browser(r.pid, &data_dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_file(home.run_file(id));
    Ok(true)
}

/// Stop the browser (if running) and delete the profile's data directory.
pub fn purge(home: &Home, id: &str) -> Result<()> {
    stop(home, id)?;
    match fs::remove_dir_all(home.profile_dir(id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProxyConfig;
    use crate::store::Store;
    use std::os::unix::fs::PermissionsExt;

    fn tmp_home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("citro-launch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    fn fake_kernel(home: &Home, body: &str) -> Kernel {
        fs::create_dir_all(&home.root).unwrap();
        let binary = home.root.join("chrome_157").join("SunBrowser");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        Kernel::from_path(binary).unwrap()
    }

    /// A stand-in kernel: announces a real listening port the way Chromium does, then idles.
    fn listening_kernel(home: &Home) -> (Kernel, std::net::TcpListener) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let body = format!(
            "for a in \"$@\"; do case \"$a\" in --user-data-dir=*) ud=\"${{a#--user-data-dir=}}\";; esac; done\n\
             printf '%s\\n%s\\n' {port} /devtools/browser/test-guid > \"$ud/DevToolsActivePort\"\n\
             sleep 60 &\nwait"
        );
        (fake_kernel(home, &body), l)
    }

    fn profile(home: &Home, mut p: Profile) -> Profile {
        let _g = crate::store::lock(home).unwrap();
        Store::open(home).unwrap().create(std::mem::take(&mut p)).unwrap()
    }

    fn arg<'a>(a: &'a [String], prefix: &str) -> Option<&'a str> {
        a.iter().find_map(|x| x.strip_prefix(prefix))
    }

    #[test]
    fn list_flags_are_merged_into_one_switch_each() {
        let m = merge_list_flags(
            [
                "--a",
                "--enable-features=X,Y",
                "--b",
                "--enable-features=Y,Z",
                "--disable-features=Q",
                "--disable-features=",
            ]
            .map(str::to_owned)
            .to_vec(),
        );
        assert_eq!(m, ["--a", "--enable-features=X,Y,Z", "--b", "--disable-features=Q"]);
        assert_eq!(merge_list_flags(vec!["--enable-features=".into()]), Vec::<String>::new());
    }

    #[test]
    fn data_dir_is_found_in_plain_and_rewritten_argv() {
        let d = Path::new("/h/profiles/k1/data");
        // ordinary NUL-separated argv
        assert!(cmdline_has_data_dir(b"/bin/k\0--user-data-dir=/h/profiles/k1/data\0--x\0", d));
        // what SunBrowser really exposes: one space-joined argument
        assert!(cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data --remote-debugging-port=0\0", d));
        assert!(cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data", d));
        // look-alikes: a longer directory, another profile, not at a token start, empty
        assert!(!cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data2 --x", d));
        assert!(!cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k10/data --x", d));
        assert!(!cmdline_has_data_dir(b"/bin/k x--user-data-dir=/h/profiles/k1/data", d));
        assert!(!cmdline_has_data_dir(b"", d));
    }

    #[test]
    fn chrome_major_from_ua() {
        assert_eq!(chrome_major("Mozilla/5.0 (X11) AppleWebKit/537.36 Chrome/157.0.0.0 Safari/537.36"), Some(157));
        assert_eq!(chrome_major("Mozilla/5.0 Gecko/20100101 Firefox/120.0"), None);
    }

    #[test]
    fn prepare_writes_config_files_and_builds_the_command_line() {
        let home = tmp_home("prepare");
        let kernel = fake_kernel(&home, "exit 0");
        let p = profile(
            &home,
            Profile {
                name: "t".into(),
                country: "fr".into(),
                user_proxy_config: ProxyConfig::from_url("http://u:p@10.0.0.1:8080").unwrap(),
                ..Default::default()
            },
        );
        let prep = prepare(&home, &kernel, &p, &LaunchOptions { headless: true, ..Default::default() }, 1_700_000_000)
            .unwrap();
        let a = &prep.args;
        assert_eq!(arg(a, "--user-data-dir="), Some(prep.data_dir.to_str().unwrap()));
        assert_eq!(arg(a, "--proxy-server="), Some("http://10.0.0.1:8080"));
        assert!(a.contains(&"--headless=new".to_owned()) && a.contains(&"--no-sandbox".to_owned()));
        assert!(!a.iter().any(|x| x.contains("enable-automation")));
        assert_eq!(a.iter().filter(|x| x.starts_with("--enable-features=")).count(), 1);
        assert!(arg(a, "--enable-features=").unwrap().contains("PermuteTLSExtensions"));
        assert_eq!(a.last().map(String::as_str), Some("about:blank"));
        assert!(arg(a, "--user-agent=").unwrap().contains("Chrome/157"));

        // the inline blob points at three files; the static one carries the proxy chain
        let inline = codec::decode_json(arg(a, "--extended-parameters=").unwrap()).unwrap();
        assert_eq!(inline["UserId"], p.serial_number.to_string());
        let read = |key: &str| codec::decode_json(&fs::read_to_string(inline[key].as_str().unwrap()).unwrap()).unwrap();
        let stat = read("StaticConfig");
        assert_eq!(stat["ProxyChain"][0]["account"], "u");
        assert_eq!(stat["ProxyChain"][0]["host"], "10.0.0.1");
        assert_eq!(stat["Platform"], "Win32");
        assert!(stat.get("StaticConfig").is_none() && stat.get("DynamicConfig").is_none());
        assert_eq!(read("DynamicConfig")["TimeZone"], "Europe/Paris");
        let mode = fs::metadata(inline["StaticConfig"].as_str().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn direct_profiles_get_no_proxy_chain_and_api_args_win() {
        let home = tmp_home("direct");
        let kernel = fake_kernel(&home, "exit 0");
        let p = profile(
            &home,
            Profile {
                launch_args: vec!["--from-profile".into()],
                tabs: vec!["https://example.com/".into()],
                domain_name: "ignored.test".into(),
                ..Default::default()
            },
        );
        let opts = LaunchOptions { launch_args: vec!["--from-api".into(), "not-a-flag".into()], ..Default::default() };
        let prep = prepare(&home, &kernel, &p, &opts, 1).unwrap();
        assert!(prep.args.contains(&"--no-proxy-server".to_owned()));
        assert!(!prep.args.iter().any(|x| x.starts_with("--proxy-server=") || x == "--headless=new"));
        assert!(prep.args.contains(&"--from-api".to_owned()) && !prep.args.contains(&"--from-profile".to_owned()));
        assert!(!prep.args.contains(&"not-a-flag".to_owned()));
        assert_eq!(prep.args.last().map(String::as_str), Some("https://example.com/"));
        let inline = codec::decode_json(arg(&prep.args, "--extended-parameters=").unwrap()).unwrap();
        let stat = codec::decode_json(&fs::read_to_string(inline["StaticConfig"].as_str().unwrap()).unwrap()).unwrap();
        assert!(stat.get("ProxyChain").is_none());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn start_reports_endpoint_then_stop_kills_the_whole_group() {
        let home = tmp_home("life");
        let (kernel, _listener) = listening_kernel(&home);
        let p = profile(&home, Profile { name: "life".into(), ..Default::default() });
        assert!(running(&home, &p.profile_id).is_none());

        let r = start(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        assert_eq!(r.ws, format!("ws://127.0.0.1:{}/devtools/browser/test-guid", r.debug_port));
        assert_eq!(running(&home, &p.profile_id).as_ref(), Some(&r));
        assert_eq!(list_running(&home), vec![r.clone()]);
        // a second start is idempotent: same process, same endpoint
        assert_eq!(start(&home, &kernel, &p, &LaunchOptions::default()).unwrap().pid, r.pid);

        assert!(stop(&home, &p.profile_id).unwrap());
        assert!(running(&home, &p.profile_id).is_none() && list_running(&home).is_empty());
        assert!(!stop(&home, &p.profile_id).unwrap());
        // the `sleep 60` the stand-in kernel forked went down with its group
        assert!(!Path::new(&format!("/proc/{}", r.pid)).exists());
        purge(&home, &p.profile_id).unwrap();
        assert!(!home.profile_dir(&p.profile_id).exists());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_kernel_that_dies_early_surfaces_its_log() {
        let home = tmp_home("dies");
        let kernel = fake_kernel(&home, "echo 'boom: no display' >&2\nexit 3");
        let p = profile(&home, Profile::default());
        match start(&home, &kernel, &p, &LaunchOptions::default()) {
            Err(Error::KernelExited { status: Some(3), stderr }) => {
                assert!(stderr.contains("boom: no display"), "{stderr}")
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(running(&home, &p.profile_id).is_none());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_kernel_that_never_listens_times_out_and_is_killed() {
        let home = tmp_home("hang");
        let kernel = fake_kernel(&home, "sleep 60 &\nwait");
        let p = profile(&home, Profile::default());
        let opts = LaunchOptions { startup_timeout: Duration::from_millis(600), ..Default::default() };
        assert!(matches!(start(&home, &kernel, &p, &opts), Err(Error::LaunchTimeout)));
        assert!(running(&home, &p.profile_id).is_none());
        let _ = fs::remove_dir_all(&home.root);
    }
}
