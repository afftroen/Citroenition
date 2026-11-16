//! Starting, tracking and stopping Clearcote kernels.
//!
//! The persona is not a pile of config files: the engine derives the whole machine from `--fingerprint=<seed>`
//! plus a few explicit switches (see [`crate::fingerprint`]). Opening a profile is therefore: compute the
//! command line and the environment ([`prepare`]), spawn the engine, and wait for its DevTools endpoint.
//!
//! A running kernel is recorded in `run/<profile_id>.json`. The record is trusted only while the pid is
//! alive *and* its command line still carries the profile's `--user-data-dir`, so a recycled pid is never
//! mistaken for a browser.

use crate::fingerprint::{Ident, Resolved};
use crate::kernel::Kernel;
use crate::kernel_env;
use crate::profile::Profile;
use crate::store::Home;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
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
    /// Keep Chromium's sandbox. Off by default, as in AdsPower: the sandbox needs user namespaces, which
    /// containers and many servers lack.
    pub sandbox: bool,
    /// Keep the persona and the proxy credentials on the command line even when the engine can take them
    /// from its environment (`ps` shows them to every local user; the environment is private).
    pub persona_on_argv: bool,
    /// Set **only** by the sync layer, after it took the team hub's lock for the profile. Without it a hub-bound
    /// profile is not launched ([`Error::HubBound`]): every way of opening one (CLI, Local API, app) has to go
    /// through the lock.
    pub hub_cleared: bool,
    pub startup_timeout: Duration,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            headless: false,
            launch_args: Vec::new(),
            last_opened_tabs: true,
            sandbox: false,
            persona_on_argv: false,
            hub_cleared: false,
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
    #[serde(default)]
    pub kernel_version: String,
    pub headless: bool,
}

/// Everything needed to spawn the kernel; produced by [`prepare`].
#[derive(Debug, Clone)]
pub struct Prepared {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub data_dir: PathBuf,
}

/// Flags every launch carries. Deliberately small: each one changes what a page can observe or how the
/// browser behaves, and a stock Chrome profile has none of them. No `--enable-automation` (it would set
/// `navigator.webdriver` and show the "controlled by automated test software" bar).
const BASE_FLAGS: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    "--password-store=basic",
    "--disable-sync",
    "--disable-breakpad",
    "--disable-dev-shm-usage",
    "--disable-background-networking",
    // many profiles share one machine: a window in the background must not run slower than the front one
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    // the Privacy Sandbox APIs a stock profile would expose and a persona should not
    "--disable-features=Translate,BrowsingTopics,BrowsingTopicsDocumentAPI,Fledge,InterestGroupStorage,PrivateAggregationApi,SharedStorageAPI,FencedFrames",
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

/// Compute the kernel's command line and environment for `p`. Creates the profile's data directory; spawns
/// nothing.
pub fn prepare(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<Prepared> {
    let id = p.profile_id.as_str();
    if id.is_empty() {
        return Err(Error::Invalid("profile has no id".into()));
    }
    let resolved = Resolved::resolve(
        &Ident { profile_id: id, country: &p.country, ip: &p.ip, persona_os: &p.persona_os },
        &p.fingerprint_config,
    );
    let data_dir = home.profile_dir(id).join("data");
    fs::create_dir_all(&data_dir)?;
    let data_dir = fs::canonicalize(&data_dir)?;
    let user_args: Vec<String> = if opts.launch_args.is_empty() { &p.launch_args } else { &opts.launch_args }
        .iter()
        .map(|a| a.trim().to_owned())
        .filter(|a| a.starts_with("--"))
        .collect();

    let mut args: Vec<String> =
        vec![format!("--user-data-dir={}", data_dir.display()), "--remote-debugging-port=0".into()];
    args.extend(BASE_FLAGS.iter().map(|s| (*s).to_owned()));
    args.extend(resolved.switches());

    // The window follows a pinned screen; a headless browser has no desktop to size it, so it gets a common one.
    if let Some((w, h)) = resolved.screen.or(opts.headless.then_some((1366, 768))) {
        args.push(format!("--window-size={w},{h}"));
    }
    match p.user_proxy_config.hop() {
        Some(h) => {
            args.push(format!("--proxy-server={}://{}:{}", h.scheme, host_for_url(&h.host), h.port));
            // QUIC is UDP; a TCP proxy cannot carry it, and a connection that falls back is a timing tell
            args.push("--disable-quic".into());
            if !h.account.is_empty() {
                match h.scheme.as_str() {
                    "socks5" => args.push(format!("--socks5-credentials={}:{}", h.account, h.password)),
                    "http" | "https" => args.push(format!("--proxy-auth={}:{}", h.account, h.password)),
                    _ => {}
                }
            }
        }
        None => args.push("--no-proxy-server".into()),
    }
    if opts.headless {
        args.extend(["--headless=new", "--disable-infobars", "--mute-audio"].map(str::to_owned));
    }
    if !opts.sandbox {
        args.push("--no-sandbox".into());
        // headed Chrome shows an "unsupported command-line flag" bar for --no-sandbox, which shrinks the first
        // tab by 56px; --test-type is the one switch that suppresses it
        if !opts.headless && !user_args.iter().any(|a| a == "--test-type") {
            args.push("--test-type".into());
        }
    }
    args.extend(kernel_env::gpu_backend_args(resolved.os, &user_args));
    args.extend(user_args);
    let mut args = merge_list_flags(args);

    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(v) = kernel_env::shader_dialect(&resolved) {
        env.push((kernel_env::SHADER_DIALECT_ENV.into(), v.into()));
    }
    env.extend(kernel_env::locale_env(&args));
    if let Some(conf) = kernel_env::fonts_conf(home, kernel) {
        env.push(("FONTCONFIG_FILE".into(), conf.display().to_string()));
    }
    if !opts.persona_on_argv
        && kernel.supports_switch(kernel_env::FROM_ENV_SWITCH)
        && let Some((kept, payload)) = kernel_env::persona_to_env(&args)
    {
        args = kept;
        env.push((kernel_env::PERSONA_ENV.into(), payload));
    }

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
/// Chromium may rewrite its own argv into ONE space-joined string, so whole NUL-separated arguments cannot
/// always be compared. Match on token boundaries instead: the flag must start the blob or follow a
/// separator, and end the blob or precede one (so `.../data` never matches `.../data2`).
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

/// A browser that exited since [`take_closed`] last looked.
#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    pub profile_id: String,
    /// Unix seconds. Exact for a browser we stopped or reaped, an estimate for one that vanished unseen.
    pub at: i64,
}

fn mtime_secs(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// When did a browser that vanished unseen close? Chromium rewrites `Preferences` (`exit_type`) and `Local
/// State` as it shuts down, so their newest mtime is the best evidence there is.
fn estimate_close_time(data_dir: &Path, started: Option<i64>) -> i64 {
    let now = now();
    let guess = ["Default/Preferences", "Local State"].iter().filter_map(|f| mtime_secs(&data_dir.join(f))).max();
    guess.unwrap_or(now).clamp(started.unwrap_or(0).min(now), now)
}

/// Replace the run record by a "closed at" note for [`take_closed`].
fn retire(home: &Home, id: &str, at: i64) {
    let _ = fs::create_dir_all(home.run_dir());
    let _ = write_private(&home.run_dir().join(format!("{id}.closed")), at.to_string().as_bytes());
    let _ = fs::remove_file(home.run_file(id));
}

/// The record of a live browser for this profile id, if any. A record whose browser is gone is retired
/// (see [`take_closed`]).
pub fn running(home: &Home, id: &str) -> Option<Running> {
    let bytes = fs::read(home.run_file(id)).ok()?;
    let record: Option<Running> = serde_json::from_slice(&bytes).ok();
    let data_dir = data_dir_of(home, id);
    match record {
        Some(r) if r.profile_id == id && pid_is_browser(r.pid, &data_dir) => Some(r),
        other => {
            retire(home, id, estimate_close_time(&data_dir, other.map(|r| r.started)));
            None
        }
    }
}

fn run_ids(home: &Home) -> Vec<String> {
    let Ok(entries) = fs::read_dir(home.run_dir()) else { return Vec::new() };
    let mut ids: Vec<String> =
        entries.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned)).collect();
    ids.sort();
    ids
}

/// Every live browser.
pub fn list_running(home: &Home) -> Vec<Running> {
    run_ids(home).iter().filter_map(|id| running(home, id)).collect()
}

/// The browsers that exited since the last call (each is reported once). Does not touch the store; see
/// [`sync_closed`].
pub fn take_closed(home: &Home) -> Vec<Closed> {
    let Ok(entries) = fs::read_dir(home.run_dir()) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().and_then(|n| n.strip_suffix(".closed")).map(str::to_owned) else {
            continue;
        };
        let at = fs::read_to_string(e.path()).ok().and_then(|t| t.trim().parse().ok()).unwrap_or_else(now);
        if fs::remove_file(e.path()).is_ok() {
            out.push(Closed { profile_id: id, at });
        }
    }
    out.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    out
}

/// Notice every browser that has exited and store its `last_close_time`. Returns how many were recorded.
/// Takes the store lock: never call it while holding that lock.
pub fn sync_closed(home: &Home) -> Result<usize> {
    for id in run_ids(home) {
        let _ = running(home, &id); // retires the ones whose process is gone
    }
    let closed = take_closed(home);
    if closed.is_empty() {
        return Ok(0);
    }
    crate::store::locked(home, |s| {
        for c in &closed {
            let _ = s.touch_closed(&c.profile_id, c.at); // a profile deleted in the meantime is fine
        }
        Ok(())
    })?;
    Ok(closed.len())
}

/// Open the profile. If it is already running its existing endpoints are returned.
pub fn start(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<Running> {
    if let Some(r) = running(home, &p.profile_id) {
        return Ok(r);
    }
    if home.hub_bound(&p.profile_id) && !opts.hub_cleared {
        return Err(Error::HubBound(p.profile_id.clone()));
    }
    let start_time = now();
    let prep = prepare(home, kernel, p, opts)?;
    let _ = fs::remove_file(prep.data_dir.join("DevToolsActivePort"));

    let log_path = kernel_log(home, &p.profile_id);
    let log = OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path)?;
    let mut cmd = Command::new(&kernel.binary);
    // our own configuration (hub tokens, test proxies, ...) is none of the browser's business
    for (k, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("CITRO_")) {
        cmd.env_remove(k);
    }
    let mut child = cmd
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
        kernel_version: kernel.version.clone(),
        headless: opts.headless,
    };
    fs::create_dir_all(home.run_dir())?;
    write_private(&home.run_file(&r.profile_id), &serde_json::to_vec_pretty(&r)?)?;

    // Reap the child when it exits (no zombies in a long-lived server) and drop our run record, unless a
    // newer launch has already replaced it.
    let (home, id) = (home.clone(), r.profile_id.clone());
    let _ = std::thread::Builder::new().name(format!("reap-{pid}")).spawn(move || {
        let _ = child.wait();
        let ours = fs::read(home.run_file(&id))
            .ok()
            .and_then(|b| serde_json::from_slice::<Running>(&b).ok())
            .is_some_and(|x| x.pid == pid);
        if ours {
            retire(&home, &id, now());
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
    retire(home, id, now());
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

    /// A stand-in kernel: a shell script installed the way `citro kernel install` lays kernels out. `tail` is
    /// appended verbatim after the script (a probe string for `supports_switch`).
    fn fake_kernel_with(home: &Home, body: &str, tail: &[u8]) -> Kernel {
        let dir = crate::kernel::kernels_dir(home).join("clearcote-151.0.7922.108");
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("chrome");
        fs::write(&binary, [format!("#!/bin/sh\n{body}\n").as_bytes(), tail].concat()).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        Kernel::from_path(binary).unwrap()
    }

    fn fake_kernel(home: &Home, body: &str) -> Kernel {
        fake_kernel_with(home, body, b"")
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

    fn env_of<'a>(p: &'a Prepared, key: &str) -> Option<&'a str> {
        p.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
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
        // one space-joined argument
        assert!(cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data --remote-debugging-port=0\0", d));
        assert!(cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data", d));
        // look-alikes: a longer directory, another profile, not at a token start, empty
        assert!(!cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k1/data2 --x", d));
        assert!(!cmdline_has_data_dir(b"/bin/k --user-data-dir=/h/profiles/k10/data --x", d));
        assert!(!cmdline_has_data_dir(b"/bin/k x--user-data-dir=/h/profiles/k1/data", d));
        assert!(!cmdline_has_data_dir(b"", d));
    }

    #[test]
    fn prepare_builds_a_clearcote_command_line_and_environment() {
        let home = tmp_home("prepare");
        let kernel = fake_kernel(&home, "exit 0");
        let p = profile(
            &home,
            Profile {
                name: "t".into(),
                country: "fr".into(),
                persona_os: "windows".into(),
                user_proxy_config: ProxyConfig::from_url("http://u:secret@10.0.0.1:8080").unwrap(),
                ..Default::default()
            },
        );
        let prep = prepare(&home, &kernel, &p, &LaunchOptions { headless: true, ..Default::default() }).unwrap();
        let a = &prep.args;
        assert_eq!(arg(a, "--user-data-dir="), Some(prep.data_dir.to_str().unwrap()));
        assert_eq!(arg(a, "--fingerprint="), Some(p.profile_id.as_str()), "the seed is the profile id");
        assert_eq!(arg(a, "--fingerprint-platform="), Some("windows"));
        assert_eq!(arg(a, "--accept-lang="), Some("fr-FR,fr,en-US,en"));
        assert_eq!((arg(a, "--lang="), arg(a, "--timezone=")), (Some("fr"), Some("Europe/Paris")));
        assert_eq!(arg(a, "--proxy-server="), Some("http://10.0.0.1:8080"));
        assert_eq!(arg(a, "--proxy-auth="), Some("u:secret"));
        assert!(!a.iter().any(|x| x.starts_with("--socks5-credentials=")));
        assert!(a.contains(&"--disable-quic".to_owned()));
        assert!(a.contains(&"--headless=new".to_owned()) && a.contains(&"--no-sandbox".to_owned()));
        assert!(a.contains(&"--disable-infobars".to_owned()) && !a.contains(&"--test-type".to_owned()));
        assert_eq!(arg(a, "--window-size="), Some("1366,768"));
        assert!(!a.iter().any(|x| x.contains("enable-automation")));
        let features: Vec<_> = a.iter().filter(|x| x.starts_with("--enable-features=")).collect();
        assert_eq!(features, ["--enable-features=WebBluetooth"]);
        let disabled = arg(a, "--disable-features=").unwrap();
        assert!(disabled.contains("Translate") && disabled.contains("FencedFrames"), "{disabled}");
        assert_eq!(a.iter().filter(|x| x.starts_with("--disable-features=")).count(), 1);
        assert_eq!(a.last().map(String::as_str), Some("about:blank"));
        if cfg!(target_os = "linux") {
            assert!(a.contains(&"--use-angle=swiftshader-webgl".to_owned()));
            assert_eq!(env_of(&prep, "LANGUAGE"), Some("fr"));
            assert_eq!(env_of(&prep, "CLEARCOTE_SHADER_DIALECT"), Some("hlsl"));
        }
        assert_eq!(env_of(&prep, "CLEARCOTE_PERSONA_ARGS"), None, "this engine has no env transport");
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn socks5_credentials_use_the_socks_switch_and_a_linux_persona_hides_web_bluetooth() {
        let home = tmp_home("socks");
        let kernel = fake_kernel(&home, "exit 0");
        let p = profile(
            &home,
            Profile {
                persona_os: "linux".into(),
                user_proxy_config: ProxyConfig::from_url("socks5://u:p%40ss@[::1]:1080").unwrap(),
                ..Default::default()
            },
        );
        let a = prepare(&home, &kernel, &p, &LaunchOptions::default()).unwrap().args;
        assert_eq!(arg(&a, "--proxy-server="), Some("socks5://[::1]:1080"));
        assert_eq!(arg(&a, "--socks5-credentials="), Some("u:p@ss"));
        assert!(!a.iter().any(|x| x.starts_with("--proxy-auth=") || x.starts_with("--headless")));
        assert!(a.contains(&"--no-sandbox".to_owned()) && a.contains(&"--test-type".to_owned()));
        assert!(arg(&a, "--disable-features=").unwrap().contains("WebBluetooth"));
        assert!(!a.iter().any(|x| x.starts_with("--window-size=")), "headed + no pinned screen: the desktop decides");
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn direct_profiles_get_no_proxy_args_and_api_args_win() {
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
        let prep = prepare(&home, &kernel, &p, &opts).unwrap();
        assert!(prep.args.contains(&"--no-proxy-server".to_owned()));
        assert!(!prep.args.iter().any(|x| x.starts_with("--proxy-server=") || x == "--disable-quic"));
        assert!(prep.args.contains(&"--from-api".to_owned()) && !prep.args.contains(&"--from-profile".to_owned()));
        assert!(!prep.args.contains(&"not-a-flag".to_owned()));
        assert_eq!(prep.args.last().map(String::as_str), Some("https://example.com/"));
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn an_engine_with_env_transport_keeps_the_seed_and_credentials_off_argv() {
        let home = tmp_home("envmode");
        let kernel = fake_kernel_with(&home, "exit 0", b"\0persona-from-env\0");
        let p = profile(
            &home,
            Profile {
                persona_os: "linux".into(),
                user_proxy_config: ProxyConfig::from_url("socks5://u:secret@10.0.0.1:1080").unwrap(),
                ..Default::default()
            },
        );
        let prep = prepare(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        assert!(prep.args.contains(&"--persona-from-env".to_owned()));
        for leaked in ["--fingerprint=", "--fingerprint-platform=", "--socks5-credentials="] {
            assert!(arg(&prep.args, leaked).is_none(), "{leaked} must not be on the command line");
        }
        assert!(prep.args.iter().all(|x| !x.contains("secret")));
        assert!(arg(&prep.args, "--proxy-server=").is_some(), "the proxy address itself stays on argv");
        assert!(env_of(&prep, "CLEARCOTE_PERSONA_ARGS").is_some_and(|v| !v.is_empty()));
        // ...unless the caller insists on the command line
        let opts = LaunchOptions { persona_on_argv: true, ..Default::default() };
        let on_argv = prepare(&home, &kernel, &p, &opts).unwrap();
        assert!(
            arg(&on_argv.args, "--socks5-credentials=").is_some()
                && env_of(&on_argv, "CLEARCOTE_PERSONA_ARGS").is_none()
        );
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
        assert_eq!((r.kernel_major, r.kernel_version.as_str()), (151, "151.0.7922.108"));
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
    fn closing_is_reported_once_and_reaches_the_profile() {
        let home = tmp_home("closed");
        let (kernel, _listener) = listening_kernel(&home);
        let p = profile(&home, Profile { name: "c".into(), ..Default::default() });
        let before = now();
        start(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        assert!(take_closed(&home).is_empty(), "a running browser is not closed");
        assert!(stop(&home, &p.profile_id).unwrap());

        assert_eq!(sync_closed(&home).unwrap(), 1);
        assert_eq!(sync_closed(&home).unwrap(), 0, "each close is recorded once");
        let stored = Store::open(&home).unwrap().get(&p.profile_id).unwrap().last_close_time;
        assert!(stored >= before && stored <= now(), "{stored}");
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_browser_that_vanishes_unseen_is_retired_with_an_estimate() {
        let home = tmp_home("vanish");
        let (kernel, _listener) = listening_kernel(&home);
        let p = profile(&home, Profile { name: "v".into(), ..Default::default() });
        let r = start(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        // the window is closed by the user (here: the whole group is killed behind our back)
        signal(-(r.pid as i32), libc::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Path::new(&format!("/proc/{}", r.pid)).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(running(&home, &p.profile_id).is_none());
        let closed = take_closed(&home);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].profile_id, p.profile_id);
        assert!(closed[0].at >= r.started && closed[0].at <= now(), "{closed:?}");
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
