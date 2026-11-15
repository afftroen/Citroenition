//! `citro`: manage anti-detect browser profiles from the shell, or serve the AdsPower-compatible Local API.

mod api;

use anyhow::{Result, anyhow, bail};
use citro_core::kernel::Kernel;
use citro_core::launch::{self, LaunchOptions};
use citro_core::profile::{Profile, ProxyConfig};
use citro_core::redact;
use citro_core::store::{self, Home, Store};
use citro_core::{Error, Result as CoreResult};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
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
    },
    /// Open a profile in its own browser
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
    /// The open-source browser kernel (Clearcote): which one is used, what is installed
    Kernel {
        #[command(subcommand)]
        cmd: Option<KernelCmd>,
    },
    /// Serve the AdsPower Local API
    Serve(ServeArgs),
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
                            "running": live.is_some(), "debug_port": live.map(|r| r.debug_port),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if rows.is_empty() {
                eprintln!("no profiles yet: citro new <name> --proxy socks5://user:pass@host:port");
            } else {
                let body: Vec<Vec<String>> = rows
                    .iter()
                    .map(|p| {
                        let status = launch::running(&home, &p.profile_id)
                            .map_or("-".to_owned(), |r| format!("running :{}", r.debug_port));
                        vec![
                            p.serial_number.to_string(),
                            p.profile_id.clone(),
                            p.name.clone(),
                            s.group_name(&p.group_id),
                            proxy_label(p),
                            status,
                        ]
                    })
                    .collect();
                table(&["NO", "ID", "NAME", "GROUP", "PROXY", "STATUS"], &body);
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
            v["running"] = launch::running(&home, &p.profile_id).map_or(Value::Null, |r| api::endpoints(&r));
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Edit { profile, name, set } => {
            let p = store::locked(&home, |s| {
                // validate on a copy: a bad flag must leave the stored profile untouched
                let mut staged = s.get(&profile)?.clone();
                if let Some(n) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    staged.name = n.to_owned();
                }
                set.apply(s, &mut staged)?;
                s.update(&profile, |p| *p = staged)
            })?;
            let note = if launch::running(&home, &p.profile_id).is_some() {
                " (running: applies the next time it is opened)"
            } else {
                ""
            };
            eprintln!("updated profile {} ({}){note}", p.serial_number, p.name);
        }
        Cmd::Delete { profiles } => {
            let gone = store::locked(&home, |s| s.delete(&profiles))?;
            if gone.is_empty() {
                bail!("no such profile: {}", profiles.join(", "));
            }
            for p in &gone {
                launch::purge(&home, &p.profile_id)?;
                println!("deleted {} ({})", p.serial_number, p.name);
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
            if a.dry_run {
                return dry_run(&home, &kernel, &p, &opts);
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
            for key in &profiles {
                match s.find(key) {
                    Some(p) if launch::stop(&home, &p.profile_id)? => {
                        println!("closed {} ({})", p.serial_number, p.name)
                    }
                    Some(p) => eprintln!("{} ({}) was not running", p.serial_number, p.name),
                    None => missing.push(key.as_str()),
                }
            }
            let _ = launch::sync_closed(&home);
            if !missing.is_empty() {
                bail!("no such profile: {}", missing.join(", "));
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
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow!("cannot start the async runtime: {e}"))?
                .block_on(api::serve(state, a.bind))?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
