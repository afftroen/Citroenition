//! `citro-install`: command-line front end of the `citro_install` library.

use citro_install::{
    InstallOptions, Installed, ProgressLine, Release, install, latest_release, list_installed, pinned_key_fingerprint,
    verify_checksums,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::error::Error;
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
            let mut line = ProgressLine::new();
            let installed = install(&root, &opts, &mut |p| line.update(p));
            line.finish();
            let (installed, downloaded) = (installed?, line.downloaded());
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
