//! Offline administration: `citro-hub admin ...` operates directly on the data directory.
//!
//! Users (and their tokens) can be changed while a server is running: the commands take `state.lock` for the
//! read-modify-write, exactly like the running server does when it persists, and the server adopts the users on its
//! next request (see `store.rs`). Teams and members are owned by the server while it runs, so changing them offline
//! needs the server to be stopped (they are held under `serve.lock` for the duration of the command); with a server
//! up, use the HTTP API instead. Tokens are shown once and only their SHA-256 is stored; nothing here ever logs one.

use super::fsio::{
    FileLock, create_private_dir, read_state, serve_lock_path, state_lock_path, state_path, write_state,
};
use super::logs::{LogSink, SERVER_LOG};
use super::state::{Cx, Event, State, UserRec};
use super::util::{new_token, now_ms, now_secs, random_id, token_hash, valid_user_name};
use crate::model::{LogKind, LogLevel, LogRecord, Member, Team, TeamRole, User, UserRole};
use std::io;
use std::path::Path;

/// The name offline changes are attributed to in the logs.
pub const CLI_ACTOR: &str = "admin-cli";

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("{0}")]
    Invalid(String),
    #[error(
        "{0} is not an initialised hub data directory; create its first user with \
         `citro-hub admin create-user --data-dir {0} --name NAME --role owner`"
    )]
    NotInitialised(String),
    #[error("{0} is already initialised")]
    AlreadyInitialised(String),
    #[error(
        "a citro-hub is serving {0}: teams and members belong to the running server while it is up. Stop it first, \
         or use the HTTP API (POST /v1/teams, POST /v1/teams/ID/members; see docs/HUB.md)"
    )]
    ServerRunning(String),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

pub type AdminResult<T> = Result<T, AdminError>;

fn check_name(name: &str) -> AdminResult<String> {
    let name = name.trim();
    if valid_user_name(name) {
        Ok(name.to_owned())
    } else {
        Err(AdminError::Invalid("a user name has 1-64 characters out of letters, digits and . _ - @".to_owned()))
    }
}

/// Run `f` on the state read from disk under `state.lock`; write the result back when `f` succeeds.
fn modify<T>(dir: &Path, f: impl FnOnce(&mut State) -> AdminResult<T>) -> AdminResult<T> {
    if !state_path(dir).exists() {
        return Err(AdminError::NotInitialised(dir.display().to_string()));
    }
    let _lock = FileLock::exclusive(&state_lock_path(dir))?;
    let mut st = read_state(dir)?;
    let out = f(&mut st)?;
    write_state(dir, &st)?;
    Ok(out)
}

/// Create the data directory with its first admin. Returns the admin's token — the only time it is ever shown.
pub fn init(dir: &Path, admin_name: &str) -> AdminResult<String> {
    init_with(dir, admin_name).map(|(_, token)| token)
}

fn init_with(dir: &Path, admin_name: &str) -> AdminResult<(User, String)> {
    let name = check_name(admin_name)?;
    create_private_dir(dir)?;
    create_private_dir(&dir.join("blobs"))?;
    create_private_dir(&dir.join("logs"))?;
    let _lock = FileLock::exclusive(&state_lock_path(dir))?;
    if state_path(dir).exists() {
        return Err(AdminError::AlreadyInitialised(dir.display().to_string()));
    }
    let token = new_token();
    let now = now_secs();
    let mut st = State { created_at: now, ..State::default() };
    let user = User { id: random_id("u", 8), name, role: UserRole::Admin, disabled: false, created_at: now };
    st.users.push(UserRec { user: user.clone(), token_hash: token_hash(&token) });
    write_state(dir, &st)?;
    Ok((user, token))
}

/// Add a user; returns it with its token (shown once).
pub fn add_user(dir: &Path, name: &str, role: UserRole) -> AdminResult<(User, String)> {
    let name = check_name(name)?;
    modify(dir, |st| {
        if st.user_by_name(&name).is_some() {
            return Err(AdminError::Invalid(format!(
                "user `{name}` already exists (use `user token {name}` to rotate its token)"
            )));
        }
        let token = new_token();
        let user = User { id: random_id("u", 8), name, role, disabled: false, created_at: now_secs() };
        st.users.push(UserRec { user: user.clone(), token_hash: token_hash(&token) });
        Ok((user, token))
    })
}

pub fn list_users(dir: &Path) -> AdminResult<Vec<User>> {
    if !state_path(dir).exists() {
        return Err(AdminError::NotInitialised(dir.display().to_string()));
    }
    let _lock = FileLock::exclusive(&state_lock_path(dir))?;
    Ok(read_state(dir)?.users())
}

/// Revoke a user: the token stops working on the server's next request and its locks are released.
/// Revocation is final; the name stays taken.
pub fn revoke_user(dir: &Path, name: &str) -> AdminResult<User> {
    modify(dir, |st| {
        let idx = st
            .users
            .iter()
            .position(|u| u.user.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| AdminError::Invalid(format!("no such user `{}`", name.trim())))?;
        let active_admins = st.users.iter().filter(|u| u.user.role == UserRole::Admin && !u.user.disabled).count();
        let target = &st.users[idx];
        if target.user.role == UserRole::Admin && !target.user.disabled && active_admins <= 1 {
            return Err(AdminError::Invalid("this is the last active admin; add another admin first".to_owned()));
        }
        let rec = &mut st.users[idx];
        rec.user.disabled = true;
        rec.token_hash.clear();
        Ok(rec.user.clone())
    })
}

/// Issue a new token for a user (the old one stops working). Returns the new token — shown once.
pub fn rotate_token(dir: &Path, name: &str) -> AdminResult<(User, String)> {
    modify(dir, |st| {
        let rec = st
            .users
            .iter_mut()
            .find(|u| u.user.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| AdminError::Invalid(format!("no such user `{}`", name.trim())))?;
        if rec.user.disabled {
            return Err(AdminError::Invalid(format!("user `{}` is revoked", rec.user.name)));
        }
        let token = new_token();
        rec.token_hash = token_hash(&token);
        Ok((rec.user.clone(), token))
    })
}

// ---- creating users, teams and memberships -------------------------------------------------------------------------

/// What `create_user` made: the user, its token (shown once) and whether the data directory was new.
#[derive(Debug, Clone)]
pub struct Created {
    pub user: User,
    pub token: String,
    pub initialised: bool,
    /// The membership requested with `team`, when there was one.
    pub member: Option<Member>,
}

/// Find a team by id or by exact (case-insensitive) name.
fn find_team<'a>(st: &'a State, key: &str) -> AdminResult<&'a Team> {
    let key = key.trim();
    st.teams
        .values()
        .map(|t| &t.team)
        .find(|t| t.id == key)
        .or_else(|| st.teams.values().map(|t| &t.team).find(|t| t.name.eq_ignore_ascii_case(key)))
        .ok_or_else(|| AdminError::Invalid(format!("no such team `{key}` (see `citro-hub admin list-teams`)")))
}

/// The pseudo user offline changes run as: a server admin that exists in no token table.
fn operator() -> User {
    User {
        id: "u-offline".to_owned(),
        name: CLI_ACTOR.to_owned(),
        role: UserRole::Admin,
        disabled: false,
        created_at: 0,
    }
}

fn cli_cx() -> Cx {
    Cx::new(operator(), "offline", now_ms(), 90_000, 0)
}

/// File what the offline command did under the teams' logs (and `note` in the server log). Best effort: a log that
/// cannot be written never fails an administration command.
fn audit(dir: &Path, events: Vec<Event>, note: &str) {
    let sink = LogSink::new(dir.join("logs"), 20 * 1024 * 1024, 3);
    let rec = |team: &str, level: LogLevel, profile_id: Option<String>, message: String| LogRecord {
        ts: now_secs(),
        kind: LogKind::Citro,
        level,
        team_id: team.to_owned(),
        user: CLI_ACTOR.to_owned(),
        device_id: "offline".to_owned(),
        profile_id,
        message,
    };
    let _ = sink.append(SERVER_LOG, &[rec(SERVER_LOG, LogLevel::Info, None, note.to_owned())]);
    for e in events {
        let _ = sink.append(&e.team_id.clone(), &[rec(&e.team_id, e.level, e.profile_id, e.message)]);
    }
}

/// Run `f` on the state while holding `state.lock` *and* `serve.lock`: nobody else is serving this directory, so
/// nothing but this command changes teams and members. Refused with [`AdminError::ServerRunning`] otherwise.
fn modify_stopped<T>(dir: &Path, f: impl FnOnce(&mut State) -> AdminResult<T>) -> AdminResult<T> {
    if !state_path(dir).exists() {
        return Err(AdminError::NotInitialised(dir.display().to_string()));
    }
    let _serve = FileLock::try_exclusive(&serve_lock_path(dir)).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => AdminError::ServerRunning(dir.display().to_string()),
        _ => AdminError::Io(e),
    })?;
    modify(dir, f)
}

/// `admin create-user`: add a user and give it a token (shown once). On a directory that does not exist yet, this
/// creates the hub: its first user must be an admin (`owner`/`admin`), it is the one who creates teams.
/// `team` (a team id or name and the role in it) adds the membership in the same step.
pub fn create_user(dir: &Path, name: &str, role: UserRole, team: Option<(&str, TeamRole)>) -> AdminResult<Created> {
    if !state_path(dir).exists() {
        if role != UserRole::Admin {
            return Err(AdminError::Invalid(
                "the first user of a new hub must be an owner or admin (`--role owner`): it creates the teams"
                    .to_owned(),
            ));
        }
        if team.is_some() {
            return Err(AdminError::Invalid("a new hub has no teams yet; create the user, then the team".to_owned()));
        }
        let (user, token) = init_with(dir, name)?;
        audit(dir, Vec::new(), &format!("hub initialised; first user `{}` created", user.name));
        return Ok(Created { user, token, initialised: true, member: None });
    }
    let name = check_name(name)?;
    let team = team.map(|(key, role)| (key.to_owned(), role));
    let made = modify(dir, |st| {
        if st.user_by_name(&name).is_some() {
            return Err(AdminError::Invalid(format!(
                "user `{name}` already exists (use `admin rotate-token --name {name}` for a new token)"
            )));
        }
        let team_id = team.as_ref().map(|(key, _)| find_team(st, key).map(|t| t.id.clone())).transpose()?;
        let token = new_token();
        let user = User { id: random_id("u", 8), name: name.clone(), role, disabled: false, created_at: now_secs() };
        st.users.push(UserRec { user: user.clone(), token_hash: token_hash(&token) });
        let member = match (team_id, team) {
            (Some(team_id), Some((_, team_role))) => {
                let m = Member { team_id, user_id: user.id.clone(), user_name: user.name.clone(), role: team_role };
                st.members.push(m.clone());
                Some(m)
            }
            _ => None,
        };
        Ok(Created { user, token, initialised: false, member })
    })?;
    let note = match &made.member {
        Some(m) => format!(
            "user `{}` created ({}); member of team {} as {}",
            made.user.name,
            made.user.role.as_str(),
            m.team_id,
            m.role.as_str()
        ),
        None => format!("user `{}` created ({})", made.user.name, made.user.role.as_str()),
    };
    audit(dir, Vec::new(), &note);
    Ok(made)
}

/// `admin create-team`: a new team owned by `owner` (default: the first active admin). Needs the server stopped.
pub fn create_team(dir: &Path, name: &str, owner: Option<&str>) -> AdminResult<(Team, User)> {
    let (team, owner, events) = modify_stopped(dir, |st| {
        let owner = match owner {
            Some(n) => st
                .user_by_name(n)
                .map(|u| u.user.clone())
                .ok_or_else(|| AdminError::Invalid(format!("no such user `{}`", n.trim())))?,
            None => st
                .users
                .iter()
                .find(|u| u.user.role == UserRole::Admin && !u.user.disabled)
                .map(|u| u.user.clone())
                .ok_or_else(|| AdminError::Invalid("the hub has no active admin to own the team".to_owned()))?,
        };
        if owner.disabled {
            return Err(AdminError::Invalid(format!("user `{}` is revoked", owner.name)));
        }
        let mut cx = cli_cx();
        let entry = st.insert_team(&mut cx, name, &owner).map_err(|e| AdminError::Invalid(e.message))?;
        Ok((entry.team, owner, std::mem::take(&mut cx.events)))
    })?;
    audit(dir, events, &format!("team `{}` ({}) created; owner `{}`", team.name, team.id, owner.name));
    Ok((team, owner))
}

/// `admin add-member`: put a user into a team (or change its role there). Needs the server stopped.
pub fn add_member(dir: &Path, team: &str, user: &str, role: TeamRole) -> AdminResult<(Team, Member)> {
    let (team, member, events) = modify_stopped(dir, |st| {
        let team = find_team(st, team)?.clone();
        let mut cx = cli_cx();
        let member = st.add_member(&mut cx, &team.id, user, role).map_err(|e| AdminError::Invalid(e.message))?;
        Ok((team, member, std::mem::take(&mut cx.events)))
    })?;
    audit(dir, events, &format!("`{}` is {} of team `{}`", member.user_name, member.role.as_str(), team.name));
    Ok((team, member))
}

/// A copy of the state for the listing commands (taken under `state.lock`, so it is never half written).
pub fn snapshot(dir: &Path) -> AdminResult<State> {
    if !state_path(dir).exists() {
        return Err(AdminError::NotInitialised(dir.display().to_string()));
    }
    let _lock = FileLock::exclusive(&state_lock_path(dir))?;
    Ok(read_state(dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citro-hub-admin-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn init_creates_the_admin_and_refuses_a_second_time() {
        let d = tmp("init");
        let token = init(&d, "root").unwrap();
        assert!(token.starts_with("citro_") && token.len() == 70);
        let st = read_state(&d).unwrap();
        assert_eq!(st.users.len(), 1);
        assert_eq!(st.users[0].user.role, UserRole::Admin);
        assert_eq!(st.users[0].token_hash, token_hash(&token));
        let raw = fs::read_to_string(state_path(&d)).unwrap();
        assert!(!raw.contains(&token), "only the hash is stored");
        assert!(matches!(init(&d, "other"), Err(AdminError::AlreadyInitialised(_))));
        assert!(matches!(init(&tmp("bad"), "no spaces"), Err(AdminError::Invalid(_))));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn user_lifecycle() {
        let d = tmp("users");
        let root_token = init(&d, "root").unwrap();
        let (ann, t1) = add_user(&d, "ann", UserRole::Member).unwrap();
        assert_eq!(ann.role, UserRole::Member);
        assert!(matches!(add_user(&d, "ANN", UserRole::Member), Err(AdminError::Invalid(_))));
        let st = read_state(&d).unwrap();
        assert_eq!(st.user_by_token(&t1).unwrap().user.name, "ann");
        // rotating replaces the token
        let (_, t2) = rotate_token(&d, "ann").unwrap();
        let st = read_state(&d).unwrap();
        assert!(st.user_by_token(&t1).is_none() && st.user_by_token(&t2).is_some());
        // revoking kills the token for good
        let revoked = revoke_user(&d, "ann").unwrap();
        assert!(revoked.disabled);
        let st = read_state(&d).unwrap();
        assert!(st.user_by_token(&t2).is_none());
        assert!(matches!(rotate_token(&d, "ann"), Err(AdminError::Invalid(_))));
        assert!(matches!(add_user(&d, "ann", UserRole::Member), Err(AdminError::Invalid(_))));
        // the last admin stays
        assert!(matches!(revoke_user(&d, "root"), Err(AdminError::Invalid(m)) if m.contains("last active admin")));
        assert!(read_state(&d).unwrap().user_by_token(&root_token).is_some());
        let (_, _) = add_user(&d, "root2", UserRole::Admin).unwrap();
        revoke_user(&d, "root").unwrap();
        assert_eq!(list_users(&d).unwrap().len(), 3);
        assert!(matches!(revoke_user(&d, "ghost"), Err(AdminError::Invalid(_))));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn uninitialised_directories_are_reported() {
        let d = tmp("none");
        assert!(matches!(list_users(&d), Err(AdminError::NotInitialised(_))));
        assert!(matches!(add_user(&d, "x", UserRole::Member), Err(AdminError::NotInitialised(_))));
    }
}
