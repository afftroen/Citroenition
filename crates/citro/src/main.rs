//! `citro`: manage anti-detect browser profiles from the shell, or serve the AdsPower-compatible Local API.

mod api;

use anyhow::{Result, anyhow, bail};
use citro_core::csv::{self, CsvRow};
use citro_core::kernel::Kernel;
use citro_core::launch::{self, LaunchOptions};
use citro_core::profile::{Profile, ProxyConfig};
use citro_core::redact;
use citro_core::store::{self, Home, Store};
use citro_core::{Error, Result as CoreResult};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "citro", version, about = "Open-source anti-detect browser profile manager", propagate_version = true)]
struct Cli {
    /// State directory (env CITRO_HOME; default ~/.local/share/citroenition)
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Clearcote kernel binary (env CITRO_KERNEL; default: the newest one installed by `citro kernel install`)
    #[arg(long, global = true, value_name = "PATH")]
    kernel: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a profile and print its id
    New {
        /// Profile name
        name: String,
        #[command(flatten)]
        set: ProfileArgs,
    },
    /// List profiles
    #[command(visible_alias = "ls")]
    List {
        /// Only this group (id or name)
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a profile (secrets hidden)
    Show { profile: String },
    /// Export profiles as a CSV file every spreadsheet opens (never contains passwords, cookies or proxy passwords)
    Export {
        /// Write to FILE instead of standard output
        #[arg(long, short, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Only this group (id or name)
        #[arg(long)]
        group: Option<String>,
        /// Leave out the UTF-8 byte order mark (Excel then guesses a legacy code page for non-ASCII text)
        #[arg(long)]
        no_bom: bool,
    },
    /// Change a profile (applies the next time it is opened)
    Edit {
        profile: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        set: ProfileArgs,
    },
    /// Delete profiles and their browser data
    #[command(visible_alias = "rm")]
    Delete {
        #[arg(required = true)]
        profiles: Vec<String>,
        /// A profile of the team hub belongs to the whole team: this deletes it on the hub, for everybody
        #[arg(long)]
        everywhere: bool,
    },
    /// Open a profile in its own browser (a profile of the team hub is locked for this device first)
    Open(OpenArgs),
    /// Close running browsers
    Close {
        #[arg(required = true)]
        profiles: Vec<String>,
    },
    /// List running browsers
    Ps {
        #[arg(long)]
        json: bool,
    },
    /// Manage groups
    Group {
        #[command(subcommand)]
        cmd: GroupCmd,
    },
    /// Import profiles from AdsPower (its Local API) or from a file (CSV, Excel, JSON, a list of proxies)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    /// Chrome extensions loaded into profiles (all of them, or the profiles of chosen groups)
    #[command(visible_alias = "ext")]
    Extension {
        #[command(subcommand)]
        cmd: ExtCmd,
    },
    /// Share profiles with a team through a Citroenition hub: one device at a time, the data follows the profile
    Hub {
        #[command(subcommand)]
        cmd: HubCmd,
    },
    /// The open-source browser kernel (Clearcote): which one is used, what is installed
    Kernel {
        #[command(subcommand)]
        cmd: Option<KernelCmd>,
    },
    /// Serve the AdsPower Local API
    Serve(ServeArgs),
}

#[derive(Subcommand)]
enum HubCmd {
    /// Log in to a team hub with an access token (a hub admin makes one for you)
    Login {
        /// Address of the hub, e.g. https://hub.example.org
        #[arg(long, env = "CITRO_HUB_URL", value_name = "URL")]
        url: String,
        /// Access token (prefer the environment variable: arguments show up in `ps`)
        #[arg(long, env = "CITRO_HUB_TOKEN", hide_env_values = true, value_name = "TOKEN")]
        token: String,
        /// How this device is called in lock messages and logs (default: its host name)
        #[arg(long, value_name = "NAME")]
        device_name: Option<String>,
        /// The team to use, when the account is in several
        #[arg(long)]
        team: Option<String>,
    },
    /// Forget the login (the hub's profiles stay here, locked to the hub, until you log in again or unbind them)
    Logout,
    /// Who you are logged in as, whether the hub answers, and what is waiting to be uploaded
    Status,
    /// Bring the team's profiles here (settings only: cookies and data come down when a profile is opened)
    Pull,
    /// Put a local profile on the hub: its cookies and data go up and the team can open it from then on
    Push { profile: String },
    /// Who has which profile open right now
    Locks {
        #[arg(long)]
        json: bool,
    },
    /// Break the lock somebody holds (team owners and admins): their browser stops at its next check
    Unlock { profile: String },
    /// Upload what a session left behind (the browser died, or the hub was away when it was closed)
    Recover { profile: String },
    /// Detach a profile from the hub: it stays here, with its data, as an ordinary local profile
    Unbind { profile: String },
}

#[derive(Subcommand)]
enum KernelCmd {
    /// Show the kernel that `citro open` would use (the default)
    Status,
    /// List the installed kernels, newest first
    #[command(visible_alias = "ls")]
    List,
    /// Download the Clearcote kernel, verify it (pinned OpenPGP key + SHA-256) and install it
    #[command(disable_version_flag = true)] // `--version` here selects the kernel, not the citro version
    Install {
        /// Chromium version (151.0.7922.108) or release tag (default: the latest release)
        #[arg(long)]
        version: Option<String>,
        /// Reinstall even if that version is already installed
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum GroupCmd {
    /// Create a group and print its id
    New {
        name: String,
        #[arg(long, default_value = "")]
        remark: String,
    },
    /// List groups
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand)]
enum ImportCmd {
    /// Every profile of a running AdsPower, with groups, proxies and cookies (the key: --key or $ADSPOWER_API_KEY)
    Adspower {
        /// AdsPower's Local API address (API & MCP in AdsPower)
        #[arg(long, default_value = citro_core::adspower::DEFAULT_URL)]
        url: String,
        /// The API key (better: set ADSPOWER_API_KEY, so it does not show in `ps`)
        #[arg(long, env = "ADSPOWER_API_KEY", hide_env_values = true, default_value = "")]
        key: String,
        /// Show what would be imported, change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// A CSV / TSV / TXT / JSON / XLSX file; columns are matched by name, almost-right values become warnings
    File {
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a sample CSV with every column the importer understands
    Sample,
}

#[derive(Subcommand)]
enum ExtCmd {
    /// Download an extension from the Chrome Web Store (a store link or the 32-letter id) and load it into profiles
    Add {
        /// https://chromewebstore.google.com/detail/<name>/<id>, or the id
        extension: String,
        /// Only for the profiles of these groups (name or id; repeat). Default: every profile
        #[arg(long = "group")]
        groups: Vec<String>,
    },
    /// List installed extensions
    #[command(visible_alias = "ls")]
    List,
    /// Remove an extension
    #[command(visible_alias = "rm")]
    Remove { id: String },
}

/// Settings shared by `new` and `edit`. Anything not given keeps its current value.
#[derive(Args, Default)]
struct ProfileArgs {
    /// Group id or name
    #[arg(long, value_name = "GROUP")]
    group: Option<String>,
    /// Platform site, opened on start when there is no --tab
    #[arg(long, value_name = "URL")]
    platform: Option<String>,
    #[arg(long)]
    remark: Option<String>,
    /// Start page; repeat for more (replaces the current list)
    #[arg(long = "tab", value_name = "URL")]
    tabs: Vec<String>,
    /// [scheme://][user:pass@]host:port (http, https, socks4, socks5), or `none`
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
    /// ISO country of the proxy exit: picks the default language and time zone
    #[arg(long, value_name = "CC")]
    country: Option<String>,
    /// Operating system the profile claims: windows, macos or linux (default: this machine's)
    #[arg(long, value_name = "OS")]
    os: Option<String>,
    /// User agent (default: Chrome of the kernel's version)
    #[arg(long)]
    ua: Option<String>,
    /// Language, most preferred first; repeat for more: --lang fr-FR --lang fr
    #[arg(long = "lang", value_name = "TAG")]
    langs: Vec<String>,
    /// IANA time zone, e.g. Europe/Paris
    #[arg(long, value_name = "TZ")]
    timezone: Option<String>,
    /// Screen / window size, e.g. 1920x1080
    #[arg(long, value_name = "WxH")]
    screen: Option<String>,
    /// Any fingerprint_config key (a JSON value when it parses): --fp webrtc=proxy --fp hardware_concurrency=8
    #[arg(long = "fp", value_name = "KEY=VALUE")]
    fp: Vec<String>,
    /// Extra kernel flag; repeat for more: --arg=--disable-gpu
    #[arg(long = "arg", value_name = "FLAG", allow_hyphen_values = true)]
    launch_args: Vec<String>,
}

#[derive(Args)]
struct OpenArgs {
    /// Profile id or serial number
    profile: String,
    /// No window (headless=new; WebGL through SwiftShader)
    #[arg(long)]
    headless: bool,
    /// Start on the profile's start pages instead of restoring the last session
    #[arg(long)]
    fresh: bool,
    /// Keep Chromium's sandbox (needs unprivileged user namespaces)
    #[arg(long)]
    sandbox: bool,
    /// Kernel flags for this launch only (they replace the profile's own)
    #[arg(long = "arg", value_name = "FLAG", allow_hyphen_values = true)]
    launch_args: Vec<String>,
    /// Seconds to wait for the DevTools endpoint
    #[arg(long, default_value_t = 30, value_name = "SECS")]
    timeout: u64,
    /// Block until the browser exits
    #[arg(long)]
    wait: bool,
    /// Print the kernel command line instead of starting it (still writes the profile's config files)
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ServeArgs {
    /// Address to listen on (AdsPower's own default is 127.0.0.1:50325)
    #[arg(long, default_value = "127.0.0.1:50325", value_name = "ADDR")]
    bind: SocketAddr,
    /// Require `Authorization: Bearer <KEY>` on every route but /status (env CITRO_API_KEY). Mandatory off loopback.
    #[arg(long, env = "CITRO_API_KEY", hide_env_values = true, value_name = "KEY")]
    api_key: Option<String>,
}

fn invalid<T>(msg: impl Into<String>) -> CoreResult<T> {
    Err(Error::Invalid(msg.into()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A duration in seconds as `45s`, `12m05s`, `3h20m` or `2d04h`.
fn age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d{:02}h", s / 86_400, s % 86_400 / 3600),
    }
}

fn who_has_it(l: &citro_sync::LockedBy) -> String {
    format!("{} on {} (for {})", l.user, l.device, age(now_secs() - l.since))
}

/// A group id, or a group name (case-insensitive), as an id.
fn resolve_group(s: &Store, g: &str) -> CoreResult<String> {
    let g = g.trim();
    if g == "0" || g.eq_ignore_ascii_case("ungrouped") {
        return Ok("0".into());
    }
    let by_id = s.groups().iter().find(|x| x.group_id == g);
    let by_name = || s.groups().iter().find(|x| x.group_name.eq_ignore_ascii_case(g));
    match by_id.or_else(by_name) {
        Some(x) => Ok(x.group_id.clone()),
        None => invalid(format!("no group `{g}` (create it with: citro group new {g})")),
    }
}

fn parse_wh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '_', ','])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 200 && h >= 200).then_some((w, h))
}

impl ProfileArgs {
    fn apply(&self, s: &Store, p: &mut Profile) -> CoreResult<()> {
        if let Some(g) = &self.group {
            p.group_id = resolve_group(s, g)?;
        }
        if let Some(v) = &self.platform {
            p.domain_name = v.clone();
        }
        if let Some(v) = &self.remark {
            p.remark = v.clone();
        }
        if !self.tabs.is_empty() {
            p.tabs = self.tabs.clone();
        }
        if let Some(v) = &self.proxy {
            p.user_proxy_config = if v.trim().eq_ignore_ascii_case("none") {
                ProxyConfig { proxy_soft: Some("no_proxy".into()), ..Default::default() }
            } else if let Some(c) = ProxyConfig::from_url(v) {
                c
            } else {
                // deliberately not echoing the value: it may carry credentials
                return invalid("--proxy: expected [scheme://][user:pass@]host:port, or `none`");
            };
        }
        if let Some(v) = &self.country {
            p.country = v.trim().to_ascii_lowercase();
        }
        if let Some(v) = &self.os {
            let os = v.trim().to_ascii_lowercase();
            if !matches!(os.as_str(), "windows" | "macos" | "linux" | "") {
                return invalid(format!("--os: expected windows, macos or linux, got `{v}`"));
            }
            p.persona_os = os;
        }
        let fp = &mut p.fingerprint_config.0;
        if let Some(v) = &self.ua {
            fp.insert("ua".into(), json!(v.trim()));
        }
        if !self.langs.is_empty() {
            fp.insert("language".into(), json!(self.langs));
            fp.insert("language_switch".into(), json!("0"));
        }
        if let Some(v) = &self.timezone {
            fp.insert("automatic_timezone".into(), json!("0"));
            fp.insert("timezone".into(), json!(v.trim()));
        }
        if let Some(v) = &self.screen {
            let Some((w, h)) = parse_wh(v) else {
                return invalid(format!("--screen: expected WxH (at least 200x200), got `{v}`"));
            };
            fp.insert("screen_resolution".into(), json!(format!("{w}_{h}")));
        }
        for kv in &self.fp {
            let Some((k, v)) = kv.split_once('=').filter(|(k, _)| !k.trim().is_empty()) else {
                return invalid(format!("--fp: expected KEY=VALUE, got `{kv}`"));
            };
            let value = serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_owned()));
            fp.insert(k.trim().to_owned(), value);
        }
        if !self.launch_args.is_empty() {
            p.launch_args = self.launch_args.clone();
        }
        Ok(())
    }
}

fn proxy_label(p: &Profile) -> String {
    match p.user_proxy_config.hop() {
        Some(h) => format!("{}://{}:{}", h.scheme, h.host, h.port),
        None => "direct".into(),
    }
}

fn table(head: &[&str], rows: &[Vec<String>]) {
    let mut width: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            width[i] = width[i].max(c.chars().count());
        }
    }
    let line = |cells: &[&str]| {
        let cols: Vec<String> = cells.iter().enumerate().map(|(i, c)| format!("{:<w$}", c, w = width[i])).collect();
        cols.join("  ").trim_end().to_owned()
    };
    println!("{}", line(head));
    for r in rows {
        println!("{}", line(&r.iter().map(String::as_str).collect::<Vec<_>>()));
    }
}

fn print_running(p: &Profile, r: &launch::Running, as_json: bool) -> Result<()> {
    if as_json {
        let mut d = api::endpoints(r);
        d["profile_id"] = json!(r.profile_id);
        d["profile_no"] = json!(p.serial_number.to_string());
        d["pid"] = json!(r.pid);
        println!("{}", serde_json::to_string_pretty(&d)?);
    } else {
        println!("profile {} ({}) is running", p.serial_number, p.profile_id);
        println!("  pid        {}", r.pid);
        println!("  selenium   127.0.0.1:{}", r.debug_port);
        println!("  puppeteer  {}", r.ws);
    }
    Ok(())
}

fn dry_run(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions) -> Result<()> {
    let prep = launch::prepare(home, kernel, p, opts)?;
    // credentials never reach the terminal: the proxy login is hidden, and the persona payload the engine
    // reads from its environment is shown decoded
    let (args, env) = redact::command_line(&prep);
    let env: serde_json::Map<String, Value> = env.into_iter().map(|(k, v)| (k, json!(v))).collect();
    let out = json!({"kernel": kernel.binary, "version": kernel.version, "env": env, "args": args});
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// A profile of the team hub: the hub's lock is taken first and the newest data comes down. This process then
/// keeps the lock alive until the browser is closed, uploads what the session changed and hands the profile back,
/// so it stays in the foreground (Ctrl-C closes the browser properly). For scripts, `citro serve` does the same.
fn open_on_hub(home: &Home, kernel: &Kernel, p: &Profile, opts: &LaunchOptions, as_json: bool) -> Result<()> {
    citro_sync::catch_signals();
    let r = match citro_sync::open(home, kernel, p, opts) {
        Ok(r) => r,
        Err(e) => match e.locked_by() {
            Some(l) => bail!(
                "{} ({}) is open on another device: {}; it opens here as soon as it is closed there",
                p.serial_number,
                p.name,
                who_has_it(&l)
            ),
            None => return Err(e.into()),
        },
    };
    print_running(p, &r, as_json)?;
    eprintln!(
        "{} belongs to the team hub: keep this window open. Closing the browser (or Ctrl-C here) uploads its data \
         and hands the profile back.",
        p.profile_id
    );
    let id = &p.profile_id;
    let out = loop {
        if let Some(o) = citro_sync::wait(home, id, Duration::from_millis(500)) {
            break Some(o);
        }
        if !citro_sync::is_syncing(home, id) {
            break citro_sync::wait(home, id, Duration::ZERO);
        }
    };
    let Some(out) = out else { return Ok(()) };
    if !out.is_clean() {
        bail!("{} ({}) closed, but: {}", p.serial_number, p.name, out.summary());
    }
    eprintln!("closed {} ({}): {}", p.serial_number, p.name, out.summary());
    Ok(())
}

fn hub_connect(home: &Home) -> Result<citro_sync::Hub> {
    Ok(citro_sync::Hub::connect(home)?.ok_or(citro_sync::Error::NotLoggedIn)?)
}

fn hub_cmd(home: &Home, cmd: HubCmd) -> Result<()> {
    match cmd {
        HubCmd::Login { url, token, device_name, team } => {
            let hub = citro_sync::Hub::log_in(home, &url, &token, device_name.as_deref(), team.as_deref())?;
            let l = hub.login();
            println!("logged in to {} as {} in team {}", l.url, l.user, l.team_name);
            println!("this device is `{}`; bring the team's profiles here with: citro hub pull", hub.device_name());
        }
        HubCmd::Logout => {
            if citro_sync::log_out(home)? {
                println!("logged out; the hub's profiles stay here but cannot be opened until you log in again");
            } else {
                eprintln!("this device was not logged in");
            }
        }
        HubCmd::Status => {
            let Some(hub) = citro_sync::Hub::connect(home)? else {
                println!("not logged in to a team hub: citro hub login --url <hub> --token <token>");
                return Ok(());
            };
            let l = hub.login();
            println!("hub        {}", l.url);
            println!("team       {} ({})", l.team_name, l.team_id);
            println!("user       {}", l.user);
            println!("device     {} ({})", hub.device_name(), hub.client().device().id);
            match hub.client().me() {
                Ok(_) => println!("reachable  yes"),
                Err(e) => println!("reachable  NO: {e}"),
            }
            let bound = citro_sync::bound_ids(home);
            let open = bound.iter().filter(|id| launch::running(home, id).is_some()).count();
            println!("profiles   {} from the hub, {open} open on this device", bound.len());
            for id in &bound {
                let waiting = citro_sync::Bound::load(home, id).is_some_and(|b| b.dirty);
                if waiting && launch::running(home, id).is_none() {
                    println!("waiting    {id} has newer data than the hub: citro hub recover {id}");
                }
            }
        }
        HubCmd::Pull => {
            let hub = hub_connect(home)?;
            let r = hub.pull()?;
            println!(
                "pulled {} profile(s) from {}: {} new, {} already here",
                r.added.len() + r.updated.len(),
                hub.login().team_name,
                r.added.len(),
                r.updated.len()
            );
            for e in &r.added {
                println!("  + {}  {}", e.id, e.name);
            }
            for e in &r.gone {
                eprintln!(
                    "  ! {}  {}: not on the hub any more. It stays here; `citro hub unbind {}` makes it a local profile",
                    e.id, e.name, e.id
                );
            }
        }
        HubCmd::Push { profile } => {
            let hub = hub_connect(home)?;
            let r = hub.push(&profile)?;
            println!("{}", r.profile.profile_id);
            eprintln!("{} ({}) is on the hub now: {}", r.profile.serial_number, r.profile.name, r.outcome.summary());
            if !r.outcome.is_clean() {
                bail!("the profile is on the hub, but its data is not clean there: {}", r.outcome.summary());
            }
        }
        HubCmd::Locks { json } => {
            let rows = hub_connect(home)?.locks()?;
            if json {
                let list: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "profile_id": r.profile_id, "name": r.name, "user": r.user, "device": r.device,
                            "since": r.since, "expires_at": r.expires_at, "this_device": r.mine,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if rows.is_empty() {
                eprintln!("nothing is open on the hub");
            } else {
                let now = now_secs();
                let body: Vec<Vec<String>> = rows
                    .iter()
                    .map(|r| {
                        let device = if r.mine { format!("{} (this device)", r.device) } else { r.device.clone() };
                        vec![
                            r.profile_id.clone(),
                            r.name.clone(),
                            r.user.clone(),
                            device,
                            age(now - r.since),
                            age(r.expires_at - now),
                        ]
                    })
                    .collect();
                table(&["ID", "NAME", "USER", "DEVICE", "OPEN FOR", "FREES IN"], &body);
            }
        }
        HubCmd::Unlock { profile } => {
            if hub_connect(home)?.force_unlock(&profile)? {
                println!(
                    "unlocked {profile}: its browser stops at its next check and its unsaved changes are kept aside"
                );
            } else {
                eprintln!("{profile} was not open");
            }
        }
        HubCmd::Recover { profile } => {
            let out = citro_sync::recover(home, &profile)?;
            if !out.is_clean() {
                bail!("{}: {}", out.profile_id, out.summary());
            }
            println!("{}: {}", out.profile_id, out.summary());
        }
        HubCmd::Unbind { profile } => {
            let p = citro_sync::unbind(home, &profile)?;
            println!("{} ({}) is a local profile now; nothing changed on the hub", p.serial_number, p.name);
        }
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let home = Home::discover(cli.home.as_deref());
    match cli.cmd {
        Cmd::New { name, set } => {
            if name.trim().is_empty() {
                bail!("the profile name must not be empty");
            }
            let made = store::locked(&home, |s| {
                let mut p = Profile { name: name.trim().to_owned(), ..Default::default() };
                set.apply(s, &mut p)?;
                s.create(p)
            })?;
            println!("{}", made.profile_id);
            eprintln!(
                "created profile {} ({}); open it with: citro open {}",
                made.serial_number, made.name, made.profile_id
            );
        }
        Cmd::List { group, json } => {
            let _ = launch::sync_closed(&home);
            let s = Store::open(&home)?;
            let gid = group.as_deref().map(|g| resolve_group(&s, g)).transpose()?;
            let rows: Vec<&Profile> =
                s.profiles().iter().filter(|p| gid.as_ref().is_none_or(|g| &p.group_id == g)).collect();
            if json {
                let list: Vec<Value> = rows
                    .iter()
                    .map(|p| {
                        let live = launch::running(&home, &p.profile_id);
                        json!({
                            "profile_no": p.serial_number, "profile_id": p.profile_id, "name": p.name,
                            "group": s.group_name(&p.group_id), "proxy": proxy_label(p),
                            "hub": home.hub_bound(&p.profile_id),
                            "running": live.is_some(), "debug_port": live.map(|r| r.debug_port),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if rows.is_empty() {
                eprintln!("no profiles yet: citro new <name> --proxy socks5://user:pass@host:port");
            } else {
                let any_hub = rows.iter().any(|p| home.hub_bound(&p.profile_id));
                let body: Vec<Vec<String>> = rows
                    .iter()
                    .map(|p| {
                        let status = launch::running(&home, &p.profile_id)
                            .map_or("-".to_owned(), |r| format!("running :{}", r.debug_port));
                        let mut row = vec![
                            p.serial_number.to_string(),
                            p.profile_id.clone(),
                            p.name.clone(),
                            s.group_name(&p.group_id),
                            proxy_label(p),
                            status,
                        ];
                        if any_hub {
                            row.push(if home.hub_bound(&p.profile_id) { "hub" } else { "" }.to_owned());
                        }
                        row
                    })
                    .collect();
                if any_hub {
                    table(&["NO", "ID", "NAME", "GROUP", "PROXY", "STATUS", "HUB"], &body);
                } else {
                    table(&["NO", "ID", "NAME", "GROUP", "PROXY", "STATUS"], &body);
                }
            }
        }
        Cmd::Export { out, group, no_bom } => {
            let _ = launch::sync_closed(&home);
            let s = Store::open(&home)?;
            let gid = group.as_deref().map(|g| resolve_group(&s, g)).transpose()?;
            let rows: Vec<CsvRow> = s
                .profiles()
                .iter()
                .filter(|p| gid.as_ref().is_none_or(|g| &p.group_id == g))
                .map(|p| {
                    let ungrouped = p.group_id.is_empty() || p.group_id == "0";
                    let group_name = if ungrouped { String::new() } else { s.group_name(&p.group_id) };
                    CsvRow::from_profile(p, &group_name, launch::running(&home, &p.profile_id).is_some())
                })
                .collect();
            let bytes = csv::write(&rows, !no_bom);
            match out {
                Some(path) => {
                    std::fs::write(&path, &bytes).map_err(|e| anyhow!("cannot write {}: {e}", path.display()))?;
                    eprintln!("exported {} profile(s) to {}", rows.len(), path.display());
                }
                None => std::io::stdout().lock().write_all(&bytes)?,
            }
        }
        Cmd::Show { profile } => {
            let s = Store::open(&home)?;
            let p = s.get(&profile)?;
            let mut v = serde_json::to_value(p)?;
            v["user_proxy_config"] = serde_json::to_value(p.user_proxy_config.redacted())?;
            for secret in ["password", "fakey"] {
                if !v[secret].as_str().unwrap_or("").is_empty() {
                    v[secret] = json!("********");
                }
            }
            if !p.cookie.is_empty() {
                v["cookie"] = json!(format!("<{} bytes>", p.cookie.len()));
            }
            v["group_name"] = json!(s.group_name(&p.group_id));
            v["hub"] = json!(home.hub_bound(&p.profile_id));
            v["running"] = launch::running(&home, &p.profile_id).map_or(Value::Null, |r| api::endpoints(&r));
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Edit { profile, name, set } => {
            // validated on a copy (a bad flag leaves the stored profile untouched); a profile of the team hub is
            // changed on the hub first, so everybody gets the same one
            let p = citro_sync::edit(&home, &profile, |s, staged| {
                if let Some(n) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    staged.name = n.to_owned();
                }
                set.apply(s, staged)
            })?;
            let note = if launch::running(&home, &p.profile_id).is_some() {
                " (running: applies the next time it is opened)"
            } else {
                ""
            };
            eprintln!("updated profile {} ({}){note}", p.serial_number, p.name);
        }
        Cmd::Delete { profiles, everywhere } => {
            let mut failed = Vec::new();
            for key in &profiles {
                match citro_sync::delete(&home, key, everywhere) {
                    Ok(p) => println!("deleted {} ({})", p.serial_number, p.name),
                    Err(e) => {
                        eprintln!("{key}: {e}");
                        failed.push(key.as_str());
                    }
                }
            }
            if !failed.is_empty() {
                bail!("not deleted: {}", failed.join(", "));
            }
        }
        Cmd::Open(a) => {
            let _ = launch::sync_closed(&home);
            let p = Store::open(&home)?.get(&a.profile)?.clone();
            let kernel = Kernel::discover(&home, cli.kernel.as_deref())?;
            let opts = LaunchOptions {
                headless: a.headless,
                launch_args: a.launch_args.clone(),
                last_opened_tabs: !a.fresh,
                sandbox: a.sandbox,
                startup_timeout: Duration::from_secs(a.timeout),
                ..Default::default()
            };
            let on_hub = home.hub_bound(&p.profile_id);
            if a.dry_run {
                if on_hub {
                    bail!(
                        "{} belongs to the team hub and gets its real settings with its lock: there is no dry run, open it",
                        p.profile_id
                    );
                }
                return dry_run(&home, &kernel, &p, &opts);
            }
            if on_hub {
                return open_on_hub(&home, &kernel, &p, &opts, a.json);
            }
            let r = launch::start(&home, &kernel, &p, &opts)?;
            let _ = store::locked(&home, |s| s.touch_opened(&p.profile_id));
            print_running(&p, &r, a.json)?;
            if a.wait {
                while launch::running(&home, &p.profile_id).is_some_and(|x| x.pid == r.pid) {
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        Cmd::Close { profiles } => {
            let s = Store::open(&home)?;
            let mut missing = Vec::new();
            let mut unclean = Vec::new();
            for key in &profiles {
                let Some(p) = s.find(key) else {
                    missing.push(key.as_str());
                    continue;
                };
                // a profile of the team hub is closed, its data uploaded and its lock handed back before this returns
                let report = citro_sync::close(&home, &p.profile_id, Duration::from_secs(180))?;
                if report.stopped {
                    println!("closed {} ({})", p.serial_number, p.name);
                } else {
                    eprintln!("{} ({}) was not running", p.serial_number, p.name);
                }
                if let Some(out) = &report.outcome {
                    println!("  {}", out.summary());
                    if !out.is_clean() {
                        unclean.push(p.profile_id.as_str());
                    }
                }
            }
            if !missing.is_empty() {
                bail!("no such profile: {}", missing.join(", "));
            }
            if !unclean.is_empty() {
                bail!("closed, but the hub side needs a look: {}", unclean.join(", "));
            }
        }
        Cmd::Ps { json } => {
            let _ = launch::sync_closed(&home);
            let s = Store::open(&home)?;
            let live = launch::list_running(&home);
            if json {
                let list: Vec<Value> = live
                    .iter()
                    .map(|r| {
                        let mut d = api::endpoints(r);
                        d["profile_id"] = json!(r.profile_id);
                        d["pid"] = json!(r.pid);
                        d
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if live.is_empty() {
                eprintln!("nothing is running");
            } else {
                let body: Vec<Vec<String>> = live
                    .iter()
                    .map(|r| {
                        let (no, name) = s
                            .find(&r.profile_id)
                            .map_or((String::new(), String::new()), |p| (p.serial_number.to_string(), p.name.clone()));
                        let up = (now_secs() - r.started).max(0);
                        vec![
                            no,
                            r.profile_id.clone(),
                            name,
                            r.pid.to_string(),
                            r.debug_port.to_string(),
                            format!("{}m{:02}s", up / 60, up % 60),
                        ]
                    })
                    .collect();
                table(&["NO", "ID", "NAME", "PID", "PORT", "UP"], &body);
            }
        }
        Cmd::Group { cmd: GroupCmd::New { name, remark } } => {
            let name = name.trim();
            if name.is_empty() {
                bail!("the group name must not be empty");
            }
            let g = store::locked(&home, |s| s.create_group(name, &remark))?;
            println!("{}", g.group_id);
        }
        Cmd::Extension { cmd: ExtCmd::Add { extension, groups } } => {
            let id = citro_core::extensions::id_from_input(&extension)
                .ok_or_else(|| anyhow!("`{extension}` is not a Chrome Web Store link or extension id"))?;
            let major = Kernel::discover(&home, cli.kernel.as_deref()).map(|k| k.major).unwrap_or(150);
            let group_ids = groups
                .iter()
                .map(|g| store::locked(&home, |s| s.group_id_for(g)))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let work = citro_core::extensions::root(&home).join(".download");
            let unpacked =
                citro_install::chrome_store::download_extension(&id, major, &work, std::time::Duration::from_secs(60))?;
            let e = citro_core::extensions::install(&home, &id, &unpacked, now_secs())?;
            let _ = std::fs::remove_dir_all(&work);
            if !group_ids.is_empty() {
                citro_core::extensions::assign(&home, &id, false, group_ids)?;
            }
            println!("installed {} {} ({})", e.name, e.version, e.id);
        }
        Cmd::Import { cmd: ImportCmd::Sample } => print!("{}", citro_core::import::SAMPLE_CSV),
        Cmd::Import { cmd } => {
            let (parsed, dry) = match cmd {
                ImportCmd::Adspower { url, key, dry_run } => {
                    eprintln!("reading AdsPower at {url} (about one request per second)...");
                    let p = citro_core::adspower::fetch(&url, &key, &mut |d, t| {
                        if t > 0 {
                            eprint!("\r  cookies {d}/{t}   ");
                        }
                    })
                    .map_err(|e| anyhow!(e))?;
                    eprintln!();
                    (p, dry_run)
                }
                ImportCmd::File { path, dry_run } => {
                    let bytes = std::fs::read(&path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let mut p = citro_core::import::parse_file(&bytes, name).map_err(|e| anyhow!(e))?;
                    // an AdsPower export leaves big cookie lists out: fetch them from AdsPower when its key is set
                    if p.profiles.iter().any(|i| i.cookies_elsewhere) {
                        match std::env::var("ADSPOWER_API_KEY") {
                            Ok(key) if !key.trim().is_empty() => {
                                eprintln!("fetching the cookies the file left out from AdsPower...");
                                let n = citro_core::adspower::fill_cookies(
                                    citro_core::adspower::DEFAULT_URL,
                                    &key,
                                    &mut p,
                                    &mut |_, _| {},
                                )
                                .map_err(|e| anyhow!(e))?;
                                eprintln!("  cookies for {n} profile(s)");
                            }
                            _ => eprintln!(
                                "note: the file left cookies out; set ADSPOWER_API_KEY (AdsPower open) to fetch them"
                            ),
                        }
                    }
                    (p, dry_run)
                }
                ImportCmd::Sample => unreachable!(),
            };
            for n in &parsed.notes {
                eprintln!("note: {n}");
            }
            let body: Vec<Vec<String>> = parsed
                .profiles
                .iter()
                .map(|i| {
                    let proxy = i
                        .profile
                        .user_proxy_config
                        .hop()
                        .map_or("direct".to_owned(), |h| format!("{} {}:{}", h.scheme, h.host, h.port));
                    let cookies = citro_core::cookies::parse(&i.profile.cookie, &i.profile.domain_name)
                        .map(|c| c.len())
                        .unwrap_or(0);
                    vec![i.profile.name.clone(), i.group.clone(), proxy, cookies.to_string(), i.warnings.join("; ")]
                })
                .collect();
            table(&["NAME", "GROUP", "PROXY", "COOKIES", "WARNINGS"], &body);
            if dry {
                println!("dry run: nothing imported ({} profiles found)", parsed.profiles.len());
            } else {
                let r = citro_core::import::apply(&home, &parsed)?;
                println!(
                    "imported {} profiles, filled in {} imported before ({} unchanged), {} new groups",
                    r.created, r.updated, r.skipped, r.groups_created
                );
            }
        }
        Cmd::Extension { cmd: ExtCmd::List } => {
            let s = Store::open(&home)?;
            let body: Vec<Vec<String>> = citro_core::extensions::list(&home)
                .iter()
                .map(|e| {
                    let to = if e.all_profiles {
                        "all profiles".to_owned()
                    } else {
                        e.groups.iter().map(|g| s.group_name(g)).collect::<Vec<_>>().join(", ")
                    };
                    vec![e.id.clone(), e.name.clone(), e.version.clone(), to]
                })
                .collect();
            table(&["ID", "NAME", "VERSION", "LOADED INTO"], &body);
        }
        Cmd::Extension { cmd: ExtCmd::Remove { id } } => {
            citro_core::extensions::remove(&home, id.trim())?;
            println!("removed {id}");
        }
        Cmd::Group { cmd: GroupCmd::List } => {
            let s = Store::open(&home)?;
            let body: Vec<Vec<String>> = s
                .groups()
                .iter()
                .map(|g| {
                    let n = s.profiles().iter().filter(|p| p.group_id == g.group_id).count();
                    vec![g.group_id.clone(), g.group_name.clone(), n.to_string(), g.remark.clone()]
                })
                .collect();
            table(&["ID", "NAME", "PROFILES", "REMARK"], &body);
        }
        Cmd::Hub { cmd } => hub_cmd(&home, cmd)?,
        Cmd::Kernel { cmd: None | Some(KernelCmd::Status) } => {
            let k = Kernel::discover(&home, cli.kernel.as_deref())?;
            println!("binary        {}", k.binary.display());
            println!("version       {}", if k.version.is_empty() { "unknown" } else { &k.version });
            println!("chrome major  {}", k.major);
            println!("chromedriver  {}", k.chromedriver().map_or("-".to_owned(), |p| p.display().to_string()));
        }
        Cmd::Kernel { cmd: Some(KernelCmd::List) } => {
            let found = Kernel::installed(&home);
            if found.is_empty() {
                eprintln!("no kernel installed in {}", citro_core::kernel::kernels_dir(&home).display());
            }
            for k in found {
                println!("{:<18}  {}", k.version, k.binary.display());
            }
        }
        Cmd::Kernel { cmd: Some(KernelCmd::Install { version, force }) } => {
            let opts = citro_install::InstallOptions { version, force, ..Default::default() };
            let mut line = citro_install::ProgressLine::new();
            let result =
                citro_install::install(&citro_core::kernel::kernels_dir(&home), &opts, &mut |p| line.update(p));
            line.finish();
            let k = result?;
            let what = if line.downloaded() { "installed" } else { "already installed (--force reinstalls)" };
            println!("{what} {}: {}", k.version, k.binary.display());
        }
        Cmd::Serve(a) => {
            if !a.bind.ip().is_loopback() && a.api_key.as_deref().is_none_or(str::is_empty) {
                bail!(
                    "refusing to listen on {} without an API key: pass --api-key (or set CITRO_API_KEY), or bind to 127.0.0.1",
                    a.bind
                );
            }
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
                .init();
            let state = api::AppState::new(home, cli.kernel, a.api_key);
            let served = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow!("cannot start the async runtime: {e}"))?
                .block_on(api::serve(state, a.bind));
            // the browsers of hub profiles this server opened are closed, their data uploaded and their locks
            // handed back: nothing else would keep those locks alive
            for out in citro_sync::shutdown(Duration::from_secs(180)) {
                tracing::info!(profile = %out.profile_id, "closed on shutdown: {}", out.summary());
            }
            served?;
        }
    }
    Ok(())
}

fn main() {
    // a browser whose proxy needs a login this kernel cannot send runs us again as its local relay
    citro_core::relay::run_if_requested();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
