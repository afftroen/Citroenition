//! The `citro-hub` command line: `serve` runs the hub, `admin ...` manages users, teams and tokens offline.
//!
//! Secrets: `admin create-user` and `admin rotate-token` print the new token to **stdout** and nothing else there
//! (so `TOKEN=$(citro-hub admin create-user ...)` works); every explanation goes to stderr. A token is never
//! printed twice, never logged and never stored (the data directory only keeps its SHA-256).

use crate::model::{TeamRole, UserRole};
use crate::server::admin::{self, AdminError};
use crate::server::run::run_blocking;
use crate::server::state::State;
use crate::server::store::Config;
use crate::server::util::iso_utc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_LISTEN: &str = "127.0.0.1:8470";
const DEFAULT_DATA_DIR: &str = "citro-hub-data";

#[derive(Debug, Parser)]
#[command(
    name = "citro-hub",
    version,
    about = "Citroenition team hub: teams, cross-device profile locks, profile-data hosting, logs and CSV export",
    long_about = "The self-hosted Citroenition team server. `serve` speaks plain HTTP and belongs behind a TLS reverse \
                  proxy (Caddy / nginx, see docs/HUB.md); `admin` creates users, teams and tokens directly on the data \
                  directory, with or without a running server."
)]
pub struct Cli {
    /// The hub's data directory (state, hosted profile data, logs). Created by the first `admin create-user`.
    #[arg(long, global = true, env = "CITRO_HUB_DATA", value_name = "DIR", default_value = DEFAULT_DATA_DIR)]
    pub data_dir: PathBuf,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve the HTTP API until Ctrl-C / SIGTERM.
    Serve(ServeArgs),
    /// Manage users, teams and tokens on the data directory (offline; users also while the server runs).
    Admin {
        #[command(subcommand)]
        action: AdminCmd,
    },
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on (plain HTTP; put a TLS reverse proxy in front for anything but localhost).
    #[arg(long, env = "CITRO_HUB_LISTEN", value_name = "ADDR", default_value = DEFAULT_LISTEN)]
    pub listen: SocketAddr,
    /// How long a lock lives without a heartbeat, in seconds.
    #[arg(long, env = "CITRO_HUB_LEASE_TTL", value_name = "SECS", default_value_t = 90)]
    pub lease_ttl: u64,
    /// The longest lease a client may ask for, in seconds.
    #[arg(long, env = "CITRO_HUB_MAX_LEASE_TTL", value_name = "SECS", default_value_t = 600)]
    pub max_lease_ttl: u64,
    /// Largest hosted profile-data archive, in MiB.
    #[arg(long, env = "CITRO_HUB_MAX_DATA_MB", value_name = "MIB", default_value_t = 512)]
    pub max_data_mb: u64,
    /// How many versions of a profile's data are kept.
    #[arg(long, env = "CITRO_HUB_KEEP_VERSIONS", value_name = "N", default_value_t = 5)]
    pub keep_versions: usize,
    /// Requests per minute and (address, user); 0 turns the limit off.
    #[arg(long, env = "CITRO_HUB_RATE_LIMIT", value_name = "PER_MIN", default_value_t = 600)]
    pub rate_limit: u32,
    /// Behind a reverse proxy: the header that carries the client address (`X-Forwarded-For`, `CF-Connecting-IP`).
    /// Only set it when the proxy overwrites that header; otherwise clients could forge their address.
    #[arg(long, env = "CITRO_HUB_REAL_IP_HEADER", value_name = "HEADER")]
    pub real_ip_header: Option<String>,
    /// Rotate a log file when it reaches this size, in MiB.
    #[arg(long, env = "CITRO_HUB_LOG_MAX_MB", value_name = "MIB", default_value_t = 20)]
    pub log_max_mb: u64,
    /// How many log files are kept per team (the current one plus rotated ones).
    #[arg(long, env = "CITRO_HUB_LOG_FILES", value_name = "N", default_value_t = 3)]
    pub log_files: usize,
}

impl ServeArgs {
    pub fn config(&self) -> Config {
        let d = Config::default();
        Config {
            bind: self.listen,
            lease_ttl: Duration::from_secs(self.lease_ttl.max(1)),
            max_lease_ttl: Duration::from_secs(self.max_lease_ttl.max(self.lease_ttl).max(1)),
            max_blob_bytes: self.max_data_mb.max(1).saturating_mul(1024 * 1024),
            keep_versions: self.keep_versions.max(1),
            rate_limit_per_min: self.rate_limit,
            real_ip_header: self.real_ip_header.clone().filter(|h| !h.trim().is_empty()),
            log_max_bytes: self.log_max_mb.max(1).saturating_mul(1024 * 1024),
            log_files: self.log_files.max(1),
            ..d
        }
    }
}

/// The four roles a person can be given. For a *user* `owner` and `admin` both mean "hub operator" (server admin:
/// creates teams, is an owner everywhere); `member` and `viewer` are ordinary accounts whose power comes from their
/// role in each team they are added to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RoleArg {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl RoleArg {
    fn user_role(self) -> UserRole {
        match self {
            RoleArg::Owner | RoleArg::Admin => UserRole::Admin,
            RoleArg::Member | RoleArg::Viewer => UserRole::Member,
        }
    }

    fn team_role(self) -> TeamRole {
        match self {
            RoleArg::Owner => TeamRole::Owner,
            RoleArg::Admin => TeamRole::Admin,
            RoleArg::Member => TeamRole::Member,
            RoleArg::Viewer => TeamRole::Viewer,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum AdminCmd {
    /// Add a user and print its token ONCE (stdout). Creates the hub when the data directory is new.
    CreateUser {
        /// Letters, digits and `. _ - @`, at most 64 characters; case-insensitively unique.
        #[arg(long)]
        name: String,
        /// owner / admin = hub operator; member / viewer = ordinary account (give it a team with `--team`).
        #[arg(long, value_enum, default_value = "member")]
        role: RoleArg,
        /// Also add the user to this team (id or name) with the role given above.
        #[arg(long, value_name = "TEAM")]
        team: Option<String>,
    },
    /// Create a team (needs the server stopped; with a server running use `POST /v1/teams`).
    CreateTeam {
        #[arg(long)]
        name: String,
        /// The team's first owner (default: the first admin).
        #[arg(long, value_name = "USER")]
        owner: Option<String>,
    },
    /// Put a user into a team or change its role there (needs the server stopped).
    AddMember {
        /// Team id or exact name.
        #[arg(long)]
        team: String,
        /// User name.
        #[arg(long)]
        user: String,
        #[arg(long, value_enum, default_value = "member")]
        role: RoleArg,
    },
    /// List users with their roles and teams (never tokens).
    ListUsers,
    /// List teams with their members.
    ListTeams,
    /// Cut a user off for good: its token stops working, its locks are released. Final: the name stays taken.
    RevokeToken {
        #[arg(long)]
        name: String,
    },
    /// Give a user a new token (the old one stops working at once); printed ONCE (stdout).
    RotateToken {
        #[arg(long)]
        name: String,
    },
}

fn out(text: &str) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(text.as_bytes());
    let _ = o.flush();
}

fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> =
        (0..cols).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0)).collect();
    let mut s = String::new();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = if i + 1 == r.len() { 0 } else { widths[i] }))
            .collect();
        s.push_str(line.join("  ").trim_end());
        s.push('\n');
    }
    s
}

fn when(secs: i64) -> String {
    iso_utc(secs)
}

fn users_table(st: &State) -> String {
    let mut rows = vec![["NAME", "ROLE", "STATUS", "CREATED", "TEAMS"].map(String::from).to_vec()];
    for u in st.users() {
        let teams: Vec<String> = st
            .members
            .iter()
            .filter(|m| m.user_id == u.id)
            .map(|m| format!("{}:{}", st.team_name(&m.team_id), m.role.as_str()))
            .collect();
        let teams = match (u.role, teams.is_empty()) {
            (UserRole::Admin, true) => "(every team, as owner)".to_owned(),
            (_, true) => "-".to_owned(),
            _ => teams.join(", "),
        };
        let status = if u.disabled { "revoked" } else { "active" };
        rows.push(vec![u.name.clone(), u.role.as_str().to_owned(), status.to_owned(), when(u.created_at), teams]);
    }
    table(&rows)
}

fn teams_table(st: &State) -> String {
    let mut rows = vec![["ID", "NAME", "PROFILES", "MEMBERS"].map(String::from).to_vec()];
    let mut teams: Vec<_> = st.teams.values().map(|t| &t.team).collect();
    teams.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    for t in teams {
        let profiles = st.profiles.values().filter(|p| p.team_id == t.id).count();
        let mut members: Vec<_> = st.members.iter().filter(|m| m.team_id == t.id).collect();
        members.sort_by(|a, b| b.role.cmp(&a.role).then_with(|| a.user_name.cmp(&b.user_name)));
        let members: Vec<String> = members.iter().map(|m| format!("{}:{}", m.user_name, m.role.as_str())).collect();
        rows.push(vec![t.id.clone(), t.name.clone(), profiles.to_string(), members.join(", ")]);
    }
    table(&rows)
}

/// Print a freshly issued token: the bare token on stdout, the explanation on stderr.
fn show_token(name: &str, label: &str, token: &str, what: &str) {
    eprintln!("{what} `{name}` ({label}). The token below is shown ONCE and cannot be recovered: save it now.");
    out(&format!("{token}\n"));
}

fn run_admin(dir: &std::path::Path, action: AdminCmd) -> Result<(), AdminError> {
    match action {
        AdminCmd::CreateUser { name, role, team } => {
            let made =
                admin::create_user(dir, &name, role.user_role(), team.as_deref().map(|t| (t, role.team_role())))?;
            if made.initialised {
                eprintln!("initialised the hub in {}", dir.display());
            }
            if let Some(m) = &made.member {
                eprintln!("added to team {} as {}", m.team_id, m.role.as_str());
            }
            let label = role.to_possible_value().map(|v| v.get_name().to_owned()).unwrap_or_default();
            show_token(&made.user.name, &label, &made.token, "created user");
            if made.initialised {
                eprintln!(
                    "next: citro-hub admin create-team --name TEAM, then citro-hub serve --data-dir {}",
                    dir.display()
                );
            }
        }
        AdminCmd::CreateTeam { name, owner } => {
            let (team, owner) = admin::create_team(dir, &name, owner.as_deref())?;
            eprintln!("team `{}` created, owned by `{}`", team.name, owner.name);
            out(&format!("{}\n", team.id));
        }
        AdminCmd::AddMember { team, user, role } => {
            let (team, m) = admin::add_member(dir, &team, &user, role.team_role())?;
            eprintln!("`{}` is now {} of team `{}`", m.user_name, m.role.as_str(), team.name);
        }
        AdminCmd::ListUsers => out(&users_table(&admin::snapshot(dir)?)),
        AdminCmd::ListTeams => out(&teams_table(&admin::snapshot(dir)?)),
        AdminCmd::RevokeToken { name } => {
            let u = admin::revoke_user(dir, &name)?;
            eprintln!("`{}` is revoked: the token no longer works and the user's locks are released", u.name);
        }
        AdminCmd::RotateToken { name } => {
            let (user, token) = admin::rotate_token(dir, &name)?;
            show_token(&user.name, user.role.as_str(), &token, "new token for");
        }
    }
    Ok(())
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

/// Parse the command line and do what it says. Returns the process exit code (clap exits by itself on usage errors).
pub fn run() -> ExitCode {
    run_with(Cli::parse())
}

pub fn run_with(cli: Cli) -> ExitCode {
    let result = match cli.command {
        Command::Serve(args) => {
            init_logging();
            let cfg = args.config();
            if !cfg.bind.ip().is_loopback() {
                tracing::warn!(
                    "listening on {} without TLS: anybody on the network path can read tokens; put a TLS reverse proxy in front (docs/HUB.md)",
                    cfg.bind
                );
            }
            run_blocking(&cli.data_dir, cfg).map_err(|e| e.to_string())
        }
        Command::Admin { action } => run_admin(&cli.data_dir, action).map_err(|e| e.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("citro-hub: error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("citro-hub").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn serve_defaults_and_flags() {
        let cli = parse(&["serve", "--data-dir", "/tmp/x", "--listen", "127.0.0.1:9000", "--lease-ttl", "30"]);
        assert_eq!(cli.data_dir, PathBuf::from("/tmp/x"));
        let Command::Serve(a) = cli.command else { panic!("serve expected") };
        let cfg = a.config();
        assert_eq!(cfg.bind, "127.0.0.1:9000".parse().unwrap());
        assert_eq!((cfg.lease_ttl.as_secs(), cfg.max_lease_ttl.as_secs()), (30, 600));
        assert_eq!(cfg.max_blob_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn the_data_dir_is_global_and_a_silly_lease_cap_follows_the_default_ttl() {
        let cli = parse(&["admin", "list-users", "--data-dir", "d"]);
        assert_eq!(cli.data_dir, PathBuf::from("d"));
        let Command::Serve(a) = parse(&["serve", "--lease-ttl", "900", "--max-lease-ttl", "60"]).command else {
            panic!("serve expected")
        };
        assert_eq!(a.config().max_lease_ttl.as_secs(), 900, "the cap is never below the default");
    }

    #[test]
    fn roles_map_to_server_and_team_roles() {
        assert_eq!(RoleArg::Owner.user_role(), UserRole::Admin);
        assert_eq!(RoleArg::Admin.user_role(), UserRole::Admin);
        assert_eq!(RoleArg::Member.user_role(), UserRole::Member);
        assert_eq!(RoleArg::Viewer.user_role(), UserRole::Member);
        assert_eq!(RoleArg::Viewer.team_role(), TeamRole::Viewer);
        assert_eq!(RoleArg::Owner.team_role(), TeamRole::Owner);
        let Command::Admin { action: AdminCmd::CreateUser { role, .. } } =
            parse(&["admin", "create-user", "--name", "a", "--role", "viewer"]).command
        else {
            panic!("create-user expected")
        };
        assert_eq!(role, RoleArg::Viewer);
        assert!(Cli::try_parse_from(["citro-hub", "admin", "create-user", "--name", "a", "--role", "root"]).is_err());
    }

    #[test]
    fn tables_align_columns_and_never_pad_the_last_one() {
        let t = table(&[vec!["a".into(), "bbb".into(), "c".into()], vec!["dddd".into(), "e".into(), "f".into()]]);
        assert_eq!(t, "a     bbb  c\ndddd  e    f\n");
    }
}
