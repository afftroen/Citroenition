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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    /// Extra kernel flags. When non-empty they replace the profile's own `launch_args` (the antidetect browser: the
    /// API's flags win).
    pub launch_args: Vec<String>,
    /// Reopen the tabs of the previous session instead of the profile's start pages.
    pub last_opened_tabs: bool,
    /// Keep Chromium's sandbox. Off by default, as in other antidetect browsers: the sandbox needs user namespaces, which
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
    /// The proxy needs a login this kernel cannot send: [`start`] runs a [`crate::relay`] and points the browser
    /// at it (`args` then has no `--proxy-server` yet).
    pub relay: Option<crate::relay::Upstream>,
    /// Every start page the profile wants (its group's, then its own). Opened on a fresh start; with a restored
    /// session only the ones not opened before ([`start`] remembers them in `start-urls.json`).
    pub start_urls: Vec<String>,
    /// Start pages [`start`] opens through DevTools once the browser is up: headless Chromium refuses more than one
    /// URL on its command line ("Multiple targets are not supported in headless mode").
    pub later_tabs: Vec<String>,
}

/// `example.com` -> `https://example.com`; `about:` and URLs with a scheme stay as they are.
fn start_url(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        None
    } else if t.contains("://") || t.starts_with("about:") || t.starts_with("chrome:") {
        Some(t.to_owned())
    } else {
        Some(format!("https://{t}"))
    }
}

fn opened_start_urls(home: &Home, id: &str) -> Vec<String> {
    fs::read(home.profile_dir(id).join("start-urls.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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

/// Write `bytes` so that a reader sees the old file or the new one, never half of it: write a private temp
/// file next to it and rename it over the target. The run record is read by other processes at any moment
/// (`citro list`, the Local API, the desktop app, the hub supervisor); with truncate-then-write one of them could
/// see an empty record, take it for "the browser is gone" and retire the record of a live browser.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp =
        PathBuf::from(format!("{}.tmp{}-{}", path.display(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let written = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(written?)
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
    // On macOS Chromium keeps the key that encrypts cookies and saved passwords in the login keychain and asks for
    // the user's password ("Chromium Safe Storage") on first use; `--password-store=basic` only means something on
    // Linux. The mock keychain uses a fixed key instead, as other antidetect browsers' launcher does.
    if cfg!(target_os = "macos") {
        args.push("--use-mock-keychain".into());
    }
    args.extend(resolved.switches());

    // The window follows a pinned screen; a headless browser has no desktop to size it, so it gets a common one.
    if let Some((w, h)) = resolved.screen.or(opts.headless.then_some((1366, 768))) {
        args.push(format!("--window-size={w},{h}"));
    }
    let mut relay = None;
    match p.user_proxy_config.hop() {
        // a login the kernel has no switch for (Clearcote's macOS build): the relay logs in instead
        Some(h)
            if !h.account.is_empty()
                && !kernel.supports_switch(if h.scheme == "socks5" { "socks5-credentials" } else { "proxy-auth" })
                && let Some(up) = crate::relay::Upstream::for_hop(&h) =>
        {
            relay = Some(up);
            args.push("--disable-quic".into());
        }
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
    // the extensions for every profile and for this profile's group
    let extensions = crate::extensions::folders_for(home, &p.group_id);
    if !extensions.is_empty() {
        let list: Vec<String> = extensions.iter().map(|d| d.display().to_string()).collect();
        args.push(format!("--load-extension={}", list.join(",")));
    }
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

    // the group's start pages, then the profile's own, each once
    let group_tabs = crate::store::Store::open(home)
        .ok()
        .and_then(|s| {
            s.groups().iter().find(|g| !p.group_id.is_empty() && g.group_id == p.group_id).map(|g| g.tabs.clone())
        })
        .unwrap_or_default();
    let mut wanted: Vec<String> = Vec::new();
    for u in group_tabs.iter().chain(&p.tabs).filter_map(|t| start_url(t)) {
        if !wanted.contains(&u) {
            wanted.push(u);
        }
    }
    if wanted.is_empty()
        && let Some(d) = start_url(&p.domain_name)
    {
        wanted.push(d);
    }
    let reopen: Vec<String> = fs::read(home.profile_dir(id).join(REOPEN_TABS))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut urls: Vec<String> = if opts.last_opened_tabs && !reopen.is_empty() {
        // the window was closed (not quit): its session is empty, so the tabs Citroenition saw come back instead
        let before = opened_start_urls(home, id);
        let mut u = reopen;
        for w in wanted.iter().filter(|u| !before.contains(u)) {
            if !u.contains(w) {
                u.push(w.clone());
            }
        }
        u
    } else if opts.last_opened_tabs && data_dir.join("Default/Sessions").is_dir() {
        // the last session comes back; a start page that is new or was changed since opens next to it
        args.push("--restore-last-session".into());
        let before = opened_start_urls(home, id);
        wanted.iter().filter(|u| !before.contains(u)).cloned().collect()
    } else if wanted.is_empty() {
        vec!["about:blank".into()]
    } else {
        wanted.clone()
    };
    let later_tabs = if opts.headless && urls.len() > 1 { urls.split_off(1) } else { Vec::new() };
    args.extend(urls);
    Ok(Prepared { args, env, data_dir, relay, start_urls: wanted, later_tabs })
}

pub(crate) fn read_devtools_port(data_dir: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(data_dir.join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    (port != 0 && path.starts_with('/')).then(|| (port, path.to_owned()))
}

pub(crate) fn listening(port: u16) -> bool {
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

pub(crate) fn signal(pid: i32, sig: i32) {
    // SAFETY: kill(2) takes plain integers; a stale pid at worst yields ESRCH.
    let _ = unsafe { libc::kill(pid, sig) };
}

/// Process `pid`'s command line as NUL-terminated arguments, the shape of `/proc/<pid>/cmdline`; `None` when the
/// process is gone (or is not ours to look at).
#[cfg(not(target_os = "macos"))]
fn cmdline_of(pid: u32) -> Option<Vec<u8>> {
    fs::read(format!("/proc/{pid}/cmdline")).ok()
}

/// macOS has no `/proc`: `sysctl(CTL_KERN, KERN_PROCARGS2, pid)` hands out the arguments the process was started
/// with, to the user that owns it. They cannot be rewritten by the process, unlike Linux's `cmdline`.
#[cfg(target_os = "macos")]
fn cmdline_of(pid: u32) -> Option<Vec<u8>> {
    let pid = libc::c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a null output buffer only asks how much room the answer needs.
    let sized = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if sized != 0 {
        return None;
    }
    // The process may grow its argument block between the two calls: a few rounds, with more room each time.
    for round in 1..=3 {
        let mut buf = vec![0u8; size * round + 4096];
        let mut len: libc::size_t = buf.len();
        // SAFETY: `buf` is writable for `len` bytes and the kernel sets `len` to what it wrote.
        let read =
            unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
        if read == 0 {
            buf.truncate(len);
            return procargs_cmdline(&buf);
        }
    }
    None
}

/// The arguments in a `KERN_PROCARGS2` answer: `[argc: i32, native endian][executable path\0][NUL padding]
/// [argv[0]\0 ... argv[argc-1]\0][environment...]`. Returned as NUL-terminated arguments, without the environment.
#[cfg(any(target_os = "macos", test))]
fn procargs_cmdline(buf: &[u8]) -> Option<Vec<u8>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?).max(0);
    let rest = &buf[4..];
    let mut args = &rest[rest.iter().position(|b| *b == 0)?..];
    while let [0, tail @ ..] = args {
        args = tail;
    }
    let mut out = Vec::new();
    for _ in 0..argc {
        let end = args.iter().position(|b| *b == 0).unwrap_or(args.len());
        out.extend_from_slice(&args[..end]);
        out.push(0);
        args = args.get(end + 1..).unwrap_or(&[]);
    }
    Some(out)
}

/// Does a process's command line (NUL-separated arguments, see [`cmdline_of`]) carry `--user-data-dir=<data_dir>`?
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
    cmdline_of(pid).is_some_and(|raw| cmdline_has_data_dir(&raw, data_dir))
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
    let path = home.run_file(id);
    let data_dir = data_dir_of(home, id);
    // A few rounds: the record may be replaced by a new launch between our read and our verdict.
    for _ in 0..3 {
        let bytes = fs::read(&path).ok()?;
        let record: Option<Running> = serde_json::from_slice(&bytes).ok();
        match record {
            Some(r) if r.profile_id == id && pid_is_browser(r.pid, &data_dir) => return Some(r),
            other => {
                // Retire the record we judged, not one a newer launch has put there since.
                if fs::read(&path).is_ok_and(|now_bytes| now_bytes == bytes) {
                    retire(home, id, estimate_close_time(&data_dir, other.map(|r| r.started)));
                    return None;
                }
            }
        }
    }
    None
}

/// What [`clear_data`] can delete, with the Chromium files of each (relative to the profile's `Default/`).
pub const DATA_KINDS: &[(&str, &[&str])] = &[
    ("cookie", &["Cookies", "Cookies-journal", "Network/Cookies", "Network/Cookies-journal"]),
    ("local_storage", &["Local Storage", "Session Storage"]),
    ("indexeddb", &["IndexedDB"]),
    (
        "extension_cache",
        &[
            "Local Extension Settings",
            "Sync Extension Settings",
            "Extension State",
            "Extension Rules",
            "Extension Scripts",
        ],
    ),
    ("history", &["History", "History-journal", "Visited Links", "Top Sites", "Top Sites-journal"]),
    (
        "image_file",
        &["Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache", "Service Worker/CacheStorage"],
    ),
];

/// Delete kinds of a closed profile's browser data (the `type`s of the Local API's delete-cache: `cookie`,
/// `local_storage`, `indexeddb`, `extension_cache`, `history`, `image_file`). Returns how many files and folders went.
pub fn clear_data(home: &Home, id: &str, kinds: &[String]) -> Result<usize> {
    if running(home, id).is_some() {
        return Err(Error::Invalid("close the browser first: its data is in use".into()));
    }
    let base = data_dir_of(home, id).join("Default");
    let mut n = 0;
    for k in kinds {
        let k = k.trim().to_ascii_lowercase();
        let Some((_, files)) = DATA_KINDS.iter().find(|(name, _)| *name == k) else {
            return Err(Error::Invalid(format!("unknown data type `{k}`")));
        };
        for f in *files {
            let path = base.join(f);
            let gone = if path.is_dir() { fs::remove_dir_all(&path).is_ok() } else { fs::remove_file(&path).is_ok() };
            n += usize::from(gone);
        }
    }
    Ok(n)
}

/// `true` while any browser of this data folder runs.
pub fn any_running(home: &Home) -> bool {
    run_ids(home).iter().any(|id| running(home, id).is_some())
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
    // macOS kernel: DCHECKs that kill tabs ("Aw, Snap! Error code: 6") become log lines; once per kernel
    let patch_note = match crate::kernel_patch::ensure(kernel) {
        Ok(crate::kernel_patch::Outcome::Patched(n)) => Some(format!("made {n} DCHECK functions non-fatal")),
        Ok(_) => None,
        Err(e) => Some(format!("could not make DCHECKs non-fatal: {e}")),
    };
    let mut prep = prepare(home, kernel, p, opts)?;
    let _ = fs::remove_file(prep.data_dir.join("DevToolsActivePort"));

    let log_path = kernel_log(home, &p.profile_id);
    let log = OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path)?;
    let pdir = home.profile_dir(&p.profile_id);
    {
        use std::io::Write;
        let note = |m: String| {
            let _ = writeln!(&log, "citroenition: {m}");
        };
        if let Some(m) = patch_note {
            note(m);
        }
        // the search engine Citroenition was told to use (first launch, or after the choice changed)
        match crate::browser_prefs::apply_search_choice(&prep.data_dir, &pdir.join("search-engine"), &p.search_engine) {
            Ok(true) => note(format!(
                "default search engine set ({})",
                if p.search_engine.trim().is_empty() { "google" } else { p.search_engine.trim() }
            )),
            Ok(false) => {}
            Err(e) => note(format!("could not set the default search engine: {e}")),
        }
        // the bookmarks bar: the group's bookmarks, then the profile's
        let group_marks = crate::store::Store::open(home)
            .ok()
            .and_then(|s| {
                s.groups()
                    .iter()
                    .find(|g| !p.group_id.is_empty() && g.group_id == p.group_id)
                    .map(|g| g.bookmarks.clone())
            })
            .unwrap_or_default();
        let mut marks: Vec<crate::bookmarks::Bookmark> = Vec::new();
        for b in group_marks.into_iter().chain(p.bookmarks.iter().cloned()) {
            if !marks.iter().any(|m| m.url == b.url) {
                marks.push(b);
            }
        }
        match crate::bookmarks::apply(&prep.data_dir, &pdir.join("bookmarks-applied.json"), &marks) {
            Ok(true) => note(format!("bookmarks bar updated ({} bookmarks)", marks.len())),
            Ok(false) => {}
            Err(e) => note(format!("could not write the bookmarks: {e}")),
        }
        // macOS: a page asking for a downloadable font must not pop up "needs to download the font"
        if cfg!(target_os = "macos")
            && let Some(app) = kernel.binary.ancestors().find(|a| a.extension().is_some_and(|e| e == "app"))
        {
            crate::browser_prefs::disable_font_downloads(&crate::browser_prefs::kernel_bundle_ids(app));
        }
    }
    let relay = match &prep.relay {
        Some(up) => {
            let r = crate::relay::Relay::start(up, log.try_clone()?)
                .map_err(|e| Error::Invalid(format!("could not start the local proxy relay: {e}")))?;
            prep.args.insert(1, format!("--proxy-server=socks5://127.0.0.1:{}", r.port));
            Some(r)
        }
        None => None,
    };
    let mut cmd = Command::new(&kernel.binary);
    // our own configuration (hub tokens, test proxies, ...) is none of the browser's business
    for (k, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("CITRO_")) {
        cmd.env_remove(k);
    }
    let child = cmd
        .args(&prep.args)
        .envs(prep.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        // its own process group: closing the launching terminal does not take the browser down, and
        // `stop` can sweep every helper process with one signal
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            if let Some(r) = relay {
                r.abort();
            }
            return Err(e.into());
        }
    };
    let pid = child.id();

    let deadline = Instant::now() + opts.startup_timeout;
    let (port, path) = loop {
        if let Some((port, path)) = read_devtools_port(&prep.data_dir).filter(|(port, _)| listening(*port)) {
            break (port, path);
        }
        if let Some(status) = child.try_wait()? {
            if let Some(r) = relay {
                r.abort();
            }
            return Err(Error::KernelExited { status: status.code(), stderr: tail(&log_path, 2000) });
        }
        if Instant::now() >= deadline {
            signal(-(pid as i32), libc::SIGKILL);
            let _ = child.wait();
            if let Some(r) = relay {
                r.abort();
            }
            return Err(Error::LaunchTimeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    if let Some(relay) = relay {
        relay.follow(pid);
    }
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
    let _ = fs::write(pdir.join("start-urls.json"), serde_json::to_vec(&prep.start_urls)?);
    let _ = fs::remove_file(pdir.join(REOPEN_TABS));
    if !prep.later_tabs.is_empty()
        && let Ok(mut cdp) = crate::cdp::Cdp::connect(&r.ws)
    {
        for url in &prep.later_tabs {
            let _ = cdp.call("Target.createTarget", serde_json::json!({ "url": url }));
        }
    }
    import_cookies(&pdir, &log_path, p, &prep.start_urls, &r.ws);

    // macOS keeps an app running after its last window closes; a profile browser must not linger like that
    // (still "open", cookies and session not written out). When every window is gone, save the cookies and quit.
    if !opts.headless {
        watch_windows(home.clone(), r.profile_id.clone(), r.ws.clone(), pid);
    }

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

/// The tabs that were open when the user closed the last window; the next launch opens them.
const REOPEN_TABS: &str = "reopen-tabs.json";

/// Quit the browser `pid` once it has no window left. On macOS the window server is asked (the red button closes
/// the window but leaves the app running in the Dock, and pages such as an extension's can outlive the window);
/// elsewhere the browser's page targets are counted. Before quitting, the cookies and the open tabs are kept, then
/// the browser is closed the way Chrome's own Quit does (`Browser.close`), which writes everything to disk.
fn watch_windows(home: Home, id: String, ws: String, pid: u32) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static WATCHED: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
    if !WATCHED.lock().map(|mut w| w.get_or_insert_with(HashSet::new).insert(pid)).unwrap_or(false) {
        return; // already watched
    }
    let _ = std::thread::Builder::new().name(format!("windows-{pid}")).spawn(move || {
        let (mut seen, mut empty, mut polls) = (false, 0, 0u32);
        // without a page the browser no longer hands out its cookies, so a recent copy is kept while it has one
        let mut last = String::new();
        // and the tabs: a window closed with its close button leaves an empty session behind
        let mut tabs: Vec<String> = Vec::new();
        let mut cdp: Option<crate::cdp::Cdp> = None;
        loop {
            polls += 1;
            std::thread::sleep(Duration::from_millis(700));
            if !process_alive(pid) {
                break;
            }
            if cdp.is_none() {
                cdp = crate::cdp::Cdp::connect(&ws).ok();
            }
            let Some(c) = cdp.as_mut() else { continue };
            let targets = match c.call("Target.getTargets", serde_json::json!({})) {
                Ok(r) => r["targetInfos"].as_array().cloned().unwrap_or_default(),
                Err(_) => {
                    cdp = None; // a slow or dropped answer: connect again next time
                    continue;
                }
            };
            let pages: Vec<&serde_json::Value> = targets.iter().filter(|t| t["type"] == "page").collect();
            let open = match crate::windows::has_window(pid) {
                Some(w) => w,
                None => !pages.is_empty(),
            };
            if open {
                (seen, empty) = (true, 0);
                let now: Vec<String> = pages
                    .iter()
                    .filter_map(|t| t["url"].as_str())
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                    .map(str::to_owned)
                    .collect();
                if !now.is_empty() {
                    tabs = now;
                }
                if polls % 3 == 1
                    && let Ok(t) = crate::cookies::export_live(&ws)
                {
                    last = t;
                }
                continue;
            }
            empty += 1;
            if seen && empty >= 2 {
                if let Ok(t) = crate::cookies::export_live(&ws) {
                    last = t; // still possible while a background page is alive
                }
                keep_cookies(&home, &id, &last);
                if !tabs.is_empty() {
                    let _ = fs::write(
                        home.profile_dir(&id).join(REOPEN_TABS),
                        serde_json::to_vec(&tabs).unwrap_or_default(),
                    );
                }
                let _ = c.call("Browser.close", serde_json::json!({}));
                break;
            }
        }
        if let Ok(mut w) = WATCHED.lock()
            && let Some(set) = w.as_mut()
        {
            set.remove(&pid);
        }
    });
}

/// Watch the windows of every browser that is open now (ones opened before this program started, or by the
/// command line), so closing their last window quits them too. Cheap to call often.
pub fn watch_open_browsers(home: &Home) {
    for r in list_running(home) {
        if !r.headless {
            watch_windows(home.clone(), r.profile_id.clone(), r.ws.clone(), r.pid);
        }
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Put the profile's `cookie` field into the browser, once per change of the field (a digest in `cookies-imported`
/// remembers what went in), so cookies that sites renew later are not overwritten on every launch. Problems are
/// written to the kernel log and never stop the launch.
fn import_cookies(pdir: &Path, log_path: &Path, p: &Profile, start_urls: &[String], ws: &str) {
    use sha2::Digest;
    use std::io::Write;
    let text = p.cookie.trim();
    let marker = pdir.join("cookies-imported");
    if text.is_empty() {
        let _ = fs::remove_file(&marker);
        return;
    }
    let digest = format!("{:x}", sha2::Sha256::digest(text.as_bytes()));
    if fs::read_to_string(&marker).is_ok_and(|m| m.trim() == digest) {
        return;
    }
    let domain = crate::cookies::default_domain(&p.domain_name, start_urls);
    let outcome = crate::cookies::parse(text, &domain).and_then(|c| crate::cookies::import(ws, &c));
    let line = match &outcome {
        Ok(n) => format!("imported {n} cookies from the profile"),
        Err(e) => format!("could not import the profile's cookies: {e}"),
    };
    if let Ok(mut f) = OpenOptions::new().append(true).open(log_path) {
        let _ = writeln!(f, "citroenition: {line}");
    }
    if outcome.is_ok() {
        let _ = fs::write(&marker, digest);
    }
}

/// Close the profile: SIGTERM so Chromium shuts down cleanly (cookies and session are flushed), then
/// SIGKILL for the whole process group if it lingers. `Ok(false)` = it was not running.
pub fn stop(home: &Home, id: &str) -> Result<bool> {
    if let Some(r) = running(home, id) {
        save_cookies(home, id, &r.ws);
    }
    stop_quietly(home, id)
}

/// Keep what the browser holds now in the profile's `cookie` field, so the editor shows the real cookies and an
/// export carries them. The import marker is set to the same text: the browser already has these cookies, so the
/// next launch does not put them back over newer ones. Team profiles (the hub keeps their data) and a store
/// someone else is writing are left alone.
pub fn save_cookies(home: &Home, id: &str, ws: &str) {
    if home.hub_bound(id) {
        return;
    }
    if let Ok(text) = crate::cookies::export_live(ws) {
        keep_cookies(home, id, &text);
    }
}

/// Store cookie text read from the browser (see [`save_cookies`]).
fn keep_cookies(home: &Home, id: &str, text: &str) {
    use sha2::Digest;
    if text.is_empty() || home.hub_bound(id) {
        return;
    }
    let saved = crate::store::try_locked(home, |s| {
        let same = s.get(id).is_ok_and(|p| crate::cookies::same(&p.cookie, text));
        if !same {
            s.update(id, |p| p.cookie = text.to_owned())?;
        }
        Ok(())
    });
    if matches!(saved, Some(Ok(()))) {
        let digest = format!("{:x}", sha2::Sha256::digest(text.trim().as_bytes()));
        let _ = fs::write(home.profile_dir(id).join("cookies-imported"), digest);
    }
}

fn stop_quietly(home: &Home, id: &str) -> Result<bool> {
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
    stop_quietly(home, id)?;
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

    /// Is there no process `pid` any more? Waits up to 5 s: the reaper thread collects a killed browser a moment
    /// after the signal. (No `/proc` on macOS; signal 0 only asks whether the process exists.)
    fn gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: `kill` with signal 0 sends nothing.
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if !alive {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// What `KERN_PROCARGS2` returns for `argv`: argc, the executable path, NUL padding, the arguments, the
    /// environment.
    fn procargs(argc: i32, exe: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        b.extend_from_slice(exe.as_bytes());
        b.extend_from_slice(&[0, 0, 0]);
        for a in argv.iter().chain(env) {
            b.extend_from_slice(a.as_bytes());
            b.push(0);
        }
        b
    }

    #[test]
    fn the_macos_process_arguments_blob_yields_the_command_line_without_the_environment() {
        let d = Path::new("/h/profiles/k1/data");
        let blob = procargs(
            3,
            "/k/Chromium.app/Contents/MacOS/Chromium",
            &["/k/Chromium.app/Contents/MacOS/Chromium", "--user-data-dir=/h/profiles/k1/data", "--x"],
            &["HOME=/Users/a", "--user-data-dir=/elsewhere"],
        );
        let cmdline = procargs_cmdline(&blob).unwrap();
        assert_eq!(cmdline, b"/k/Chromium.app/Contents/MacOS/Chromium\0--user-data-dir=/h/profiles/k1/data\0--x\0");
        assert!(cmdline_has_data_dir(&cmdline, d));
        // an environment entry that looks like the flag is not an argument
        let other = procargs(1, "/bin/k", &["/bin/k"], &["--user-data-dir=/h/profiles/k1/data"]);
        assert!(!cmdline_has_data_dir(&procargs_cmdline(&other).unwrap(), d));
        // a short or empty answer, and an argc larger than what is there, are survivable
        assert_eq!(procargs_cmdline(b""), None);
        assert_eq!(procargs_cmdline(&[1, 0, 0, 0]), None);
        assert_eq!(procargs_cmdline(&procargs(5, "/bin/k", &["/bin/k", "--a"], &[])).unwrap(), b"/bin/k\0--a\0\0\0\0");
        assert_eq!(procargs_cmdline(&procargs(0, "/bin/k", &[], &[])).unwrap(), b"");
        assert_eq!(procargs_cmdline(&procargs(-1, "/bin/k", &["/bin/k"], &[])).unwrap(), b"");
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
        let kernel = fake_kernel_with(&home, "exit 0", b"\0proxy-auth\0socks5-credentials\0");
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
        assert_eq!(a.contains(&"--use-mock-keychain".to_owned()), cfg!(target_os = "macos"));
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
        let kernel = fake_kernel_with(&home, "exit 0", b"\0proxy-auth\0socks5-credentials\0");
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
    fn a_login_the_kernel_cannot_send_goes_through_the_relay() {
        let home = tmp_home("relay");
        // Clearcote's macOS build: no --socks5-credentials, no --proxy-auth
        let kernel = fake_kernel(&home, "exit 0");
        let p = profile(
            &home,
            Profile {
                persona_os: "macos".into(),
                user_proxy_config: ProxyConfig::from_url("socks5://u:secret@10.0.0.1:1080").unwrap(),
                ..Default::default()
            },
        );
        let prep = prepare(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        let up = prep.relay.clone().expect("a relay");
        assert_eq!(
            (up.scheme.as_str(), up.host.as_str(), up.port, up.user.as_str()),
            ("socks5", "10.0.0.1", 1080, "u")
        );
        assert!(arg(&prep.args, "--proxy-server=").is_none(), "start() points the browser at the relay");
        assert!(prep.args.iter().all(|x| !x.contains("secret")));
        assert!(prep.args.contains(&"--disable-quic".to_owned()));
        // no login: the browser connects by itself
        let open = profile(
            &home,
            Profile {
                user_proxy_config: ProxyConfig::from_url("socks5://10.0.0.1:1080").unwrap(),
                ..Default::default()
            },
        );
        let prep = prepare(&home, &kernel, &open, &LaunchOptions::default()).unwrap();
        assert!(prep.relay.is_none());
        assert_eq!(arg(&prep.args, "--proxy-server="), Some("socks5://10.0.0.1:1080"));
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
        let kernel = fake_kernel_with(&home, "exit 0", b"\0persona-from-env\0socks5-credentials\0");
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
        assert!(gone(r.pid), "process {} is still there", r.pid);
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
        assert!(gone(r.pid), "process {} is still there", r.pid);
        assert!(running(&home, &p.profile_id).is_none());
        let closed = take_closed(&home);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].profile_id, p.profile_id);
        assert!(closed[0].at >= r.started && closed[0].at <= now(), "{closed:?}");
        let _ = fs::remove_dir_all(&home.root);
    }

    /// A browser's run record is read by other processes while it is being written. A reader that saw a
    /// half-written record used to retire it: the hub supervisor then took a live browser for closed, uploaded
    /// its data and released the team lock under it.
    #[test]
    fn a_live_browsers_record_survives_readers_that_poll_while_it_is_rewritten() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let home = tmp_home("poll");
        let (kernel, _listener) = listening_kernel(&home);
        let p = profile(&home, Profile { name: "poll".into(), ..Default::default() });
        let r = start(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        let id = p.profile_id.clone();
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (home, id, done) = (home.clone(), id.clone(), done.clone());
                std::thread::spawn(move || {
                    let mut gone = 0u32;
                    while !done.load(Ordering::Relaxed) {
                        if running(&home, &id).is_none() {
                            gone += 1;
                        }
                    }
                    gone
                })
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&r).unwrap();
        for _ in 0..4000 {
            write_private(&home.run_file(&id), &bytes).unwrap();
        }
        done.store(true, Ordering::Relaxed);
        let gone: u32 = readers.into_iter().map(|t| t.join().unwrap()).sum();
        assert_eq!(gone, 0, "a reader took the live browser for closed");
        assert!(running(&home, &id).is_some());
        assert!(take_closed(&home).is_empty(), "nothing was retired");
        assert!(stop(&home, &id).unwrap());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_record_of_a_process_that_is_not_the_browser_is_still_retired() {
        let home = tmp_home("replaced");
        let (kernel, _listener) = listening_kernel(&home);
        let p = profile(&home, Profile { name: "r".into(), ..Default::default() });
        let r = start(&home, &kernel, &p, &LaunchOptions::default()).unwrap();
        // a record for a pid that is not ours: the reader may retire it, but only while it is unchanged
        let stale = Running { pid: 1, ..r.clone() };
        write_private(&home.run_file(&p.profile_id), &serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
        assert!(running(&home, &p.profile_id).is_none(), "pid 1 is not this profile's browser");
        assert!(!home.run_file(&p.profile_id).exists(), "the stale record is retired");
        let _ = fs::remove_dir_all(&home.root);
        signal(-(r.pid as i32), libc::SIGKILL);
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
