//! Offline administration: `citro-hub init` and `citro-hub user ...` operate directly on the data directory.
//!
//! They take `state.lock` for the read-modify-write, exactly like the running server does when it persists, so both
//! can be used at the same time; a running server notices the new `state.json` on its next request and adopts the
//! users (see `store.rs`). Tokens are shown once and only their SHA-256 is stored.

use super::fsio::{FileLock, create_private_dir, read_state, state_lock_path, state_path, write_state};
use super::state::{State, UserRec};
use super::util::{new_token, now_secs, random_id, token_hash, valid_user_name};
use crate::model::{User, UserRole};
use std::io;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0} is not an initialised hub data directory; run `citro-hub init --data DIR --admin NAME` first")]
    NotInitialised(String),
    #[error("{0} is already initialised")]
    AlreadyInitialised(String),
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
    st.users.push(UserRec {
        user: User { id: random_id("u", 8), name, role: UserRole::Admin, disabled: false, created_at: now },
        token_hash: token_hash(&token),
    });
    write_state(dir, &st)?;
    Ok(token)
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
