//! `citro-install`: command-line front end of the `citro_install` library.

use citro_install::{
    InstallOptions, Installed, Progress, Release, install, latest_release, list_installed, pinned_key_fingerprint,
    verify_checksums,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::error::Error;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "citro-install", version, about = "Download, verify and unpack the Clearcote browser kernel")]
struct Cli {
    /// Directory holding the kernels, as `clearcote-<version>/` (default: $CITRO_HOME/kernels, i.e.
    /// ~/.local/share/citroenition/kernels)
    #[arg(long, global = true, value_name = "DIR")]
    kernels_dir: Option<PathBuf>,
    /// Print machine-readable JSON on stdout
    #[arg(long, global = true)]
    json: bool,
    /// Seconds to wait for a connection and for each read before giving up (0 = forever)
    #[arg(long, global = true, default_value_t = 30, value_name = "SECS")]
    timeout: u64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the latest release for this platform (one lookup; downloads nothing)
    Latest {
        /// Ask this mirror (`<URL>/SHA256SUMS.txt`, `.asc`, archives) instead of GitHub
        #[arg(long, value_name = "URL")]
        base_url: Option<String>,
    },
    /// Download, verify and install a kernel
    Install {
        /// Chromium version (151.0.7922.108) or, from GitHub, a release tag (v0.1.0-pre.25); default: the latest
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Reinstall even if that version is already installed
        #[arg(long)]
        force: bool,
        /// Download from this mirror instead of GitHub (the signature is still checked against the pinned key)
        #[arg(long, value_name = "URL")]
        base_url: Option<String>,
    },
    /// List the installed kernels, newest first
    #[command(visible_alias = "ls")]
    List,
    /// Check a SHA256SUMS file against its detached signature with the pinned release key
    VerifyChecksums {
        /// The checksum list (SHA256SUMS.txt)
        sums: PathBuf,
        /// Its detached OpenPGP signature (SHA256SUMS.txt.asc)
        sig: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            if cli.json {
                println!("{}", json!({ "ok": false, "error": e.to_string() }));
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let options = |version: Option<String>, base_url: Option<String>, force: bool| InstallOptions {
        version,
        base_url,
        force,
        timeout: Duration::from_secs(cli.timeout),
    };
    match &cli.cmd {
        Cmd::Latest { base_url } => {
            let release = latest_release(&options(None, base_url.clone(), false))?;
            if cli.json {
                println!("{}", serde_json::to_string(&release)?);
            } else {
                print_release(&release);
            }
        }
        Cmd::Install { version, force, base_url } => {
            let root = kernels_dir(cli);
            let opts = options(version.clone(), base_url.clone(), *force);
            let mut reporter = Reporter::new();
            let mut downloaded = false;
            let installed = install(&root, &opts, &mut |p| {
                downloaded |= matches!(p, Progress::Downloading { .. });
                reporter.report(p);
            });
            reporter.finish();
            let installed = installed?;
            if cli.json {
                let mut out = serde_json::to_value(&installed)?;
                out["downloaded"] = json!(downloaded);
                println!("{out}");
            } else if downloaded {
                println!("installed {} -> {}", installed.version, installed.binary.display());
            } else {
                println!(
                    "{} is already installed -> {} (use --force to reinstall)",
                    installed.version,
                    installed.binary.display()
                );
            }
        }
        Cmd::List => {
            let root = kernels_dir(cli);
            let found = list_installed(&root);
            if cli.json {
                println!("{}", serde_json::to_string(&found)?);
            } else if found.is_empty() {
                eprintln!("no kernel installed in {}", root.display());
            } else {
                for Installed { version, binary, .. } in &found {
                    println!("{version:<18}  {}", binary.display());
                }
            }
        }
        Cmd::VerifyChecksums { sums, sig } => {
            let read = |p: &PathBuf| std::fs::read(p).map_err(|e| format!("reading {}: {e}", p.display()));
            let list = verify_checksums(&read(sums)?, &read(sig)?)?;
            if cli.json {
                let files: Vec<_> =
                    list.entries().iter().map(|e| json!({ "name": e.name, "sha256": e.sha256 })).collect();
                println!("{}", json!({ "ok": true, "signed_by": list.signed_by(), "files": files }));
            } else {
                println!("OK: {} is signed by the pinned release key {}", sums.display(), pinned_key_fingerprint()?);
                for e in list.entries() {
                    println!("  {}  {}", e.sha256, e.name);
                }
            }
        }
    }
    Ok(())
}

/// `--kernels-dir`, else `$CITRO_HOME/kernels`, else `$XDG_DATA_HOME/citroenition/kernels`, else
/// `~/.local/share/citroenition/kernels` (the same state directory the `citro` CLI uses).
fn kernels_dir(cli: &Cli) -> PathBuf {
    if let Some(dir) = &cli.kernels_dir {
        return dir.clone();
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    env("CITRO_HOME")
        .or_else(|| env("XDG_DATA_HOME").map(|p| p.join("citroenition")))
        .unwrap_or_else(|| env("HOME").unwrap_or_default().join(".local/share/citroenition"))
        .join("kernels")
}

fn print_release(r: &Release) {
    let size = r.asset.size.map_or(String::new(), |s| format!("  ({s} bytes)"));
    if !r.tag.is_empty() {
        println!("release    {}", r.tag);
    }
    println!("kernel     {} ({})", r.version, r.platform);
    println!("archive    {}{size}", r.asset.name);
    println!("url        {}", r.asset.url);
    println!("checksums  {}", r.sums_url);
    println!("signature  {}", r.sig_url);
    if let Some(url) = &r.page_url {
        println!("page       {url}");
    }
}

/// Progress on stderr: one line that keeps updating on a terminal, plain lines (every 10 %) otherwise.
struct Reporter {
    tty: bool,
    line_open: bool,
    last_decile: Option<u64>,
}

impl Reporter {
    fn new() -> Self {
        Self { tty: std::io::stderr().is_terminal(), line_open: false, last_decile: None }
    }

    fn status(&mut self, text: &str) {
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r\x1b[K{text}");
            self.line_open = true;
        } else {
            let _ = writeln!(err, "{text}");
        }
        let _ = err.flush();
    }

    fn report(&mut self, p: Progress) {
        match p {
            Progress::Resolving => self.status("resolving the release and checking the signature of SHA256SUMS.txt..."),
            Progress::Downloading { done, total } => {
                let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
                if total == 0 {
                    return self.status(&format!("downloading {:.1} MiB", mib(done)));
                }
                let pct = done * 100 / total;
                let decile = pct / 10;
                if self.tty || self.last_decile != Some(decile) {
                    self.last_decile = Some(decile);
                    self.status(&format!("downloading {:.1} / {:.1} MiB ({pct}%)", mib(done), mib(total)));
                }
            }
            Progress::Verifying => self.status("verifying the archive against the signed checksum..."),
            Progress::Extracting => self.status("extracting..."),
            Progress::Done => {}
        }
    }

    /// Wipe the updating line so the next output starts on a clean one.
    fn finish(&mut self) {
        if self.line_open {
            let _ = write!(std::io::stderr(), "\r\x1b[K");
            self.line_open = false;
        }
    }
}
