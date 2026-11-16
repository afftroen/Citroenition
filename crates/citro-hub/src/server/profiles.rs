//! Profiles, locks (leases) and the data-blob metadata: all the rules that decide who may see or change what.
//!
//! Like the rest of the state layer these are synchronous methods on `&mut State`; validate first, mutate last.

use super::error::{ApiError, ApiResult};
use super::redact::{merge_patch, redact, restore_masked};
use super::state::{Cx, LockRec, State};
use super::util::{clean_text, new_lease, random_id, token_hash};
use crate::model::{DataMeta, HubProfile, Lease, LogLevel, ProfilePage, TeamRole, UNGROUPED, User};
use citro_core::profile::Profile;
use serde_json::Value;

/// Keys of a profile the server owns: a PATCH or POST can never set them.
const SERVER_OWNED: [&str; 5] = ["profile_id", "serial_number", "created_time", "last_open_time", "last_close_time"];

pub const MAX_COOKIE_BYTES: usize = 256 * 1024;
pub const DEFAULT_LIMIT: usize = 1000;
pub const MAX_LIMIT: usize = 10_000;

/// What `bulk_delete` did: the ids deleted and `(id, reason)` for the ones that were skipped.
pub type BulkDeleted = (Vec<String>, Vec<(String, String)>);

/// Server-side form of the profile list filters.
#[derive(Debug, Clone, Default)]
pub struct ProfileQuery {
    pub group: Option<String>,
    pub q: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

fn too_long(field: &str, max: usize) -> ApiError {
    ApiError::invalid(format!("`{field}` is longer than {max} characters"))
}

/// Size sanity checks (the 1 MiB body limit bounds everything; these give readable errors).
pub fn validate_profile(p: &Profile) -> ApiResult<()> {
    let checks: [(&str, &str, usize); 9] = [
        ("name", &p.name, 200),
        ("remark", &p.remark, 5000),
        ("domain_name", &p.domain_name, 2000),
        ("username", &p.username, 500),
        ("password", &p.password, 1000),
        ("fakey", &p.fakey, 500),
        ("country", &p.country, 16),
        ("persona_os", &p.persona_os, 32),
        ("category_id", &p.category_id, 64),
    ];
    for (field, value, max) in checks {
        if value.chars().count() > max {
            return Err(too_long(field, max));
        }
    }
    if p.cookie.len() > MAX_COOKIE_BYTES {
        return Err(ApiError::invalid(format!("`cookie` is larger than {MAX_COOKIE_BYTES} bytes")));
    }
    if p.tabs.len() > 200 || p.tabs.iter().any(|t| t.len() > 4000) {
        return Err(ApiError::invalid("`tabs` holds more than 200 entries or an entry longer than 4000 bytes"));
    }
    if p.launch_args.len() > 100 || p.launch_args.iter().any(|t| t.len() > 2000) {
        return Err(ApiError::invalid("`launch_args` holds more than 100 entries or an entry longer than 2000 bytes"));
    }
    Ok(())
}

fn changed_keys(old: &Profile, new: &Profile) -> Vec<String> {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) = (serde_json::to_value(old), serde_json::to_value(new)) else {
        return Vec::new();
    };
    b.iter().filter(|(k, v)| a.get(*k) != Some(v)).map(|(k, _)| k.clone()).collect()
}

fn matches_query(p: &Profile, q: &str) -> bool {
    let hay = [
        p.name.as_str(),
        p.remark.as_str(),
        p.profile_id.as_str(),
        p.domain_name.as_str(),
        p.username.as_str(),
        p.country.as_str(),
        p.ip.as_str(),
        p.user_proxy_config.proxy_host.as_deref().unwrap_or(""),
    ];
    hay.iter().any(|h| h.to_lowercase().contains(q))
        || p.serial_number.to_string() == q
        || p.tabs.iter().any(|t| t.to_lowercase().contains(q))
}

impl State {
    // ---- views -------------------------------------------------------------------------------------------------

    /// A profile as served: the lock filled in from the live lock table, secrets masked unless `reveal`.
    pub fn view(&self, hp: &HubProfile, reveal: bool, now_ms: i64) -> HubProfile {
        let mut out = hp.clone();
        out.lock = self.effective_lock(&hp.profile.profile_id, now_ms).map(LockRec::info);
        if !reveal {
            redact(&mut out.profile);
        }
        out
    }

    pub fn list_profiles(&self, user: &User, team_id: &str, q: &ProfileQuery, now_ms: i64) -> ApiResult<ProfilePage> {
        self.require_role(user, team_id, TeamRole::Viewer)?;
        let group = q
            .group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(|g| self.resolve_group(team_id, g).unwrap_or_else(|| "\u{0}".to_owned()));
        let needle = q.q.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let mut all: Vec<&HubProfile> = self
            .profiles
            .values()
            .filter(|p| p.team_id == team_id)
            .filter(|p| group.as_deref().is_none_or(|g| p.profile.group_id == g))
            .filter(|p| needle.as_deref().is_none_or(|n| matches_query(&p.profile, n)))
            .collect();
        all.sort_by_key(|p| p.profile.serial_number);
        let total = all.len();
        let offset = q.offset.unwrap_or(0);
        let limit = q.limit.filter(|l| *l > 0).unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        let profiles = all.into_iter().skip(offset).take(limit).map(|p| self.view(p, false, now_ms)).collect();
        Ok(ProfilePage { profiles, total })
    }

    /// May this caller see the secrets of the profile? Holders (with a valid lease) and team owners/admins may.
    /// A lease that is present but not the live one is an error, never a silent downgrade.
    fn may_reveal(&self, user: &User, role: TeamRole, pid: &str, lease: Option<&str>, now_ms: i64) -> ApiResult<bool> {
        if lease.is_some_and(|l| !l.is_empty()) {
            self.check_lease(user, pid, lease, now_ms)?;
            return Ok(true);
        }
        Ok(role >= TeamRole::Admin)
    }

    pub fn profile_detail(&self, user: &User, pid: &str, lease: Option<&str>, now_ms: i64) -> ApiResult<HubProfile> {
        let (hp, role) = self.profile_role(user, pid)?;
        let reveal = self.may_reveal(user, role, pid, lease, now_ms)?;
        Ok(self.view(hp, reveal, now_ms))
    }

    // ---- create / edit / delete ----------------------------------------------------------------------------------

    fn resolve_group_for_write(&self, team_id: &str, key: &str) -> ApiResult<String> {
        self.resolve_group(team_id, key).ok_or_else(|| ApiError::invalid(format!("unknown group `{}`", key.trim())))
    }

    pub fn create_profile(&mut self, cx: &mut Cx, team_id: &str, mut p: Profile) -> ApiResult<HubProfile> {
        let role = self.require_role(&cx.user, team_id, TeamRole::Member)?;
        validate_profile(&p)?;
        p.group_id = self.resolve_group_for_write(team_id, &p.group_id)?;
        let now = cx.now();
        let pid = loop {
            let id = random_id("k", 6);
            if !self.profiles.contains_key(&id) {
                break id;
            }
        };
        let serial = {
            let team = self.teams.get_mut(team_id).ok_or_else(|| ApiError::not_found("no such team"))?;
            let s = team.next_serial;
            team.next_serial += 1;
            s
        };
        p.profile_id = pid.clone();
        p.serial_number = serial;
        p.created_time = now;
        p.last_open_time = 0;
        p.last_close_time = 0;
        let name = p.name.clone();
        let hp = HubProfile {
            team_id: team_id.to_owned(),
            created_by: cx.user.name.clone(),
            updated_at: now,
            last_open_by: String::new(),
            last_close_by: String::new(),
            data: DataMeta::default(),
            lock: None,
            profile: p,
        };
        let out = self.view(&hp, role >= TeamRole::Admin, cx.now_ms);
        self.profiles.insert(pid.clone(), hp);
        cx.log(team_id, LogLevel::Info, Some(&pid), format!("profile `{name}` (#{serial}) created"));
        Ok(out)
    }

    /// The lock gate of every edit: the live lock blocks everyone but its holder (who proves it with the lease).
    fn edit_gate(&self, cx: &mut Cx, pid: &str, lease: Option<&str>, team_id: &str, what: &str) -> ApiResult<()> {
        if lease.is_some_and(|l| !l.is_empty()) {
            self.check_lease(&cx.user, pid, lease, cx.now_ms)?;
            return Ok(());
        }
        if let Some(l) = self.effective_lock(pid, cx.now_ms) {
            let holder = l.holder();
            cx.log(team_id, LogLevel::Warn, Some(pid), format!("{what} refused: profile is open ({holder})"));
            return Err(ApiError::locked(holder));
        }
        Ok(())
    }

    pub fn patch_profile(
        &mut self,
        cx: &mut Cx,
        pid: &str,
        patch: &Value,
        lease: Option<&str>,
    ) -> ApiResult<HubProfile> {
        let (hp, role) = self.profile_role_min(&cx.user, pid, TeamRole::Member)?;
        let team_id = hp.team_id.clone();
        let old = hp.profile.clone();
        let Value::Object(fields) = patch else {
            return Err(ApiError::invalid("the patch must be a JSON object"));
        };
        self.edit_gate(cx, pid, lease, &team_id, "edit")?;

        let mut merged = serde_json::to_value(&old).map_err(|e| ApiError::internal(e.to_string()))?;
        let mut cleaned = fields.clone();
        for k in SERVER_OWNED {
            cleaned.remove(k);
        }
        merge_patch(&mut merged, &Value::Object(cleaned));
        let mut new: Profile =
            serde_json::from_value(merged).map_err(|e| ApiError::invalid(format!("invalid profile field: {e}")))?;
        restore_masked(&old, &mut new);
        new.profile_id = old.profile_id.clone();
        new.serial_number = old.serial_number;
        new.created_time = old.created_time;
        new.last_open_time = old.last_open_time;
        new.last_close_time = old.last_close_time;
        validate_profile(&new)?;
        new.group_id = self.resolve_group_for_write(&team_id, &new.group_id)?;

        let reveal = role >= TeamRole::Admin || lease.is_some_and(|l| !l.is_empty());
        if new == old {
            let hp = &self.profiles[pid];
            return Ok(self.view(hp, reveal, cx.now_ms));
        }
        let keys = changed_keys(&old, &new);
        let name = new.name.clone();
        let now = cx.now();
        let hp = self.profiles.get_mut(pid).expect("looked up above");
        hp.profile = new;
        hp.updated_at = now;
        let out = self.view(&self.profiles[pid], reveal, cx.now_ms);
        cx.log(&team_id, LogLevel::Info, Some(pid), format!("profile `{name}` edited ({})", keys.join(", ")));
        Ok(out)
    }

    fn check_deletable(&self, cx: &mut Cx, pid: &str) -> ApiResult<String> {
        let (hp, role) = self.profile_role(&cx.user, pid)?;
        if role < TeamRole::Admin {
            return Err(ApiError::forbidden("this needs a team owner or admin"));
        }
        let team_id = hp.team_id.clone();
        if let Some(l) = self.effective_lock(pid, cx.now_ms) {
            let holder = l.holder();
            cx.log(&team_id, LogLevel::Warn, Some(pid), format!("delete refused: profile is open ({holder})"));
            return Err(ApiError::locked(holder));
        }
        Ok(team_id)
    }

    /// Delete a profile (refused while it is open). The caller removes the blob directory afterwards.
    pub fn delete_profile(&mut self, cx: &mut Cx, pid: &str) -> ApiResult<()> {
        let team_id = self.check_deletable(cx, pid)?;
        let hp = self.profiles.remove(pid).expect("checked above");
        self.locks.remove(pid);
        cx.log(
            &team_id,
            LogLevel::Info,
            Some(pid),
            format!("profile `{}` (#{}) deleted", hp.profile.name, hp.profile.serial_number),
        );
        Ok(())
    }

    // ---- locks and leases --------------------------------------------------------------------------------------

    /// The caller holds the live lock of this profile: same user, the lease token matches, not expired (the token
    /// comparison is constant-time and only the token's SHA-256 is stored). Otherwise:
    ///
    /// | live lock | lease sent | answer |
    /// |---|---|---|
    /// | held by somebody | none | `409 locked` + who/which device (the caller never locked it) |
    /// | held by somebody | does not match | `403 lease_invalid` + who holds it now (voided, taken over, someone else's) |
    /// | nobody | any | `403 lease_invalid` (expired, released or force-unlocked) |
    /// | nobody | none | `403 lease_required` (lock the profile first) |
    pub fn check_lease(&self, user: &User, pid: &str, lease: Option<&str>, now_ms: i64) -> ApiResult<&LockRec> {
        let given = lease.filter(|l| !l.is_empty());
        match (self.effective_lock(pid, now_ms), given) {
            (Some(live), Some(g)) if live.user_id == user.id && live.lease_matches(g) => Ok(live),
            (Some(live), None) => Err(ApiError::locked(live.holder())),
            (Some(live), Some(_)) => Err(ApiError::lease_invalid().with_holder(live.holder())),
            (None, Some(_)) => Err(ApiError::lease_invalid()),
            (None, None) => Err(ApiError::lease_required()),
        }
    }

    /// Take the lock of a profile for a device (see the lock protocol in `docs/HUB.md`).
    ///
    /// The lock is exclusive: while it is live every other caller, even the same user on another device, gets
    /// `409 locked`. The same device asking again (a restarted client) is given a fresh lease and the old one dies.
    /// `cx.ttl_ms` is the lease length the caller asked for (already clamped by the HTTP layer).
    pub fn lock_profile(&mut self, cx: &mut Cx, pid: &str, device_id: &str, device_name: &str) -> ApiResult<Lease> {
        let (hp, _) = self.profile_role_min(&cx.user, pid, TeamRole::Member)?;
        let (team_id, pname) = (hp.team_id.clone(), hp.profile.name.clone());
        let device_id =
            clean_text(device_id, 128).ok_or_else(|| ApiError::invalid("device_id must be 1-128 characters"))?;
        let device_name = clean_text(device_name, 100).unwrap_or_else(|| device_id.chars().take(100).collect());
        cx.device_id = device_id.clone();
        let now_ms = cx.now_ms;

        let mut takeover = None;
        if let Some(l) = self.locks.get_mut(pid) {
            if !l.expired(now_ms) {
                if l.user_id == cx.user.id && l.device_id == device_id {
                    let lease = new_lease();
                    l.lease_hash = token_hash(&lease);
                    l.expires_ms = now_ms + cx.ttl_ms;
                    l.ttl_ms = cx.ttl_ms;
                    l.device_name = device_name.clone();
                    let l = l.clone();
                    let msg = format!(
                        "lock of {} on {device_name} renewed for `{pname}` (the previous lease is void)",
                        l.user
                    );
                    let out = cx.lease_of(pid, &l, &lease);
                    cx.log(&team_id, LogLevel::Info, Some(pid), msg);
                    return Ok(out);
                }
                let holder = l.holder();
                cx.log(
                    &team_id,
                    LogLevel::Warn,
                    Some(pid),
                    format!("lock denied for {} on {device_name}: `{pname}` is open ({holder})", cx.user.name),
                );
                return Err(ApiError::locked(holder));
            }
            takeover = Some(format!("{} on {}", l.user, l.device_name));
        }

        let lease = new_lease();
        let rec = LockRec {
            user_id: cx.user.id.clone(),
            user: cx.user.name.clone(),
            device_id,
            device_name: device_name.clone(),
            lease_hash: token_hash(&lease),
            since: cx.now(),
            expires_ms: now_ms + cx.ttl_ms,
            ttl_ms: cx.ttl_ms,
        };
        let out = cx.lease_of(pid, &rec, &lease);
        self.locks.insert(pid.to_owned(), rec);
        let hp = self.profiles.get_mut(pid).expect("looked up above");
        hp.profile.last_open_time = cx.now();
        hp.last_open_by = cx.user.name.clone();
        let msg = match takeover {
            Some(prev) => format!(
                "lock granted to {} on {device_name} for `{pname}` (took over the expired lock of {prev})",
                cx.user.name
            ),
            None => format!("lock granted to {} on {device_name} for `{pname}`", cx.user.name),
        };
        cx.log(&team_id, LogLevel::Info, Some(pid), msg);
        Ok(out)
    }

    /// Extend the lease by the lock's TTL. Only the holder can; see [`State::check_lease`] for the refusals.
    pub fn heartbeat(&mut self, cx: &mut Cx, pid: &str, lease: Option<&str>) -> ApiResult<Lease> {
        self.profile_role_min(&cx.user, pid, TeamRole::Member)?;
        self.check_lease(&cx.user, pid, lease, cx.now_ms)?;
        let given = lease.unwrap_or_default().to_owned();
        let l = self.locks.get_mut(pid).expect("checked above");
        l.expires_ms = cx.now_ms + l.ttl(cx.ttl_ms);
        cx.device_id = l.device_id.clone();
        cx.lazy = true;
        let l = l.clone();
        Ok(cx.lease_of(pid, &l, &given))
    }

    /// Release the lock; `closed` stamps `last_close_time` / `last_close_by`.
    pub fn unlock(&mut self, cx: &mut Cx, pid: &str, lease: Option<&str>, closed: bool) -> ApiResult<()> {
        let (hp, _) = self.profile_role(&cx.user, pid)?;
        let (team_id, pname) = (hp.team_id.clone(), hp.profile.name.clone());
        let l = self.check_lease(&cx.user, pid, lease, cx.now_ms)?.clone();
        cx.device_id = l.device_id.clone();
        self.locks.remove(pid);
        let now = cx.now();
        if closed {
            let hp = self.profiles.get_mut(pid).expect("looked up above");
            hp.profile.last_close_time = now;
            hp.last_close_by = cx.user.name.clone();
        }
        let how = if closed { "closed" } else { "released without closing" };
        cx.log(
            &team_id,
            LogLevel::Info,
            Some(pid),
            format!("unlock by {} on {}: `{pname}` {how}", l.user, l.device_name),
        );
        Ok(())
    }

    /// Break a lock (team owners/admins only). Returns whether there was a live lock to break.
    pub fn force_unlock(&mut self, cx: &mut Cx, pid: &str) -> ApiResult<bool> {
        let (hp, role) = self.profile_role(&cx.user, pid)?;
        if role < TeamRole::Admin {
            return Err(ApiError::forbidden("this needs a team owner or admin"));
        }
        let (team_id, pname) = (hp.team_id.clone(), hp.profile.name.clone());
        let live = self.effective_lock(pid, cx.now_ms).map(|l| format!("{} on {}", l.user, l.device_name));
        self.locks.remove(pid);
        match &live {
            Some(who) => cx.log(
                &team_id,
                LogLevel::Warn,
                Some(pid),
                format!("force-unlock by {}: lock of {who} on `{pname}` broken", cx.user.name),
            ),
            None => cx.log(
                &team_id,
                LogLevel::Info,
                Some(pid),
                format!("force-unlock by {}: `{pname}` was not locked", cx.user.name),
            ),
        }
        Ok(live.is_some())
    }

    /// Everything a data transfer needs to know before touching the blob: the caller is a team member (not a
    /// viewer), holds the live lock, and what is stored right now. Reading is as exclusive as writing: while
    /// another device holds the profile its cookies are not served to anybody else.
    pub fn data_access(&self, user: &User, pid: &str, lease: Option<&str>, now_ms: i64) -> ApiResult<DataMeta> {
        let (hp, _) = self.profile_role_min(user, pid, TeamRole::Member)?;
        self.check_lease(user, pid, lease, now_ms)?;
        Ok(hp.data.clone())
    }

    /// Final check of a data push, under the same mutex as the rename: lease still live, `base_version` current.
    /// Returns the version that is stored now.
    pub fn data_commit_check(&self, cx: &Cx, pid: &str, lease: Option<&str>, base: Option<u64>) -> ApiResult<u64> {
        let current = self.data_access(&cx.user, pid, lease, cx.now_ms)?.version;
        match base {
            Some(b) if b != current => Err(ApiError::stale(current)),
            _ => Ok(current),
        }
    }

    /// Record a pushed blob (the file is already in place): bump the version.
    pub fn data_commit_apply(&mut self, cx: &mut Cx, pid: &str, size: u64, sha256: String) -> DataMeta {
        let now = cx.now();
        let hp = self.profiles.get_mut(pid).expect("data_commit_check succeeded");
        hp.data = DataMeta { version: hp.data.version + 1, size, sha256, updated_at: now };
        let (team_id, meta, pname) = (hp.team_id.clone(), hp.data.clone(), hp.profile.name.clone());
        cx.log(
            &team_id,
            LogLevel::Info,
            Some(pid),
            format!("data pushed for `{pname}`: v{} ({size} bytes)", meta.version),
        );
        meta
    }

    /// Delete several profiles; the ones that cannot be deleted are reported with their reason, not fatal.
    pub fn bulk_delete(&mut self, cx: &mut Cx, team_id: &str, ids: &[String]) -> ApiResult<BulkDeleted> {
        let role = self.require_role(&cx.user, team_id, TeamRole::Member)?;
        if role < TeamRole::Admin {
            return Err(ApiError::forbidden("this needs a team owner or admin"));
        }
        let (mut deleted, mut skipped) = (Vec::new(), Vec::new());
        for pid in ids {
            if self.profiles.get(pid).is_none_or(|p| p.team_id != team_id) {
                skipped.push((pid.clone(), "not found".to_owned()));
                continue;
            }
            match self.delete_profile(cx, pid) {
                Ok(()) => deleted.push(pid.clone()),
                Err(e) => skipped.push((pid.clone(), e.code.to_owned())),
            }
        }
        Ok((deleted, skipped))
    }

    /// Where profiles are not in any group any more (used after group deletes) — kept here for the unit tests.
    pub fn ungrouped(&self, team_id: &str) -> usize {
        self.profiles.values().filter(|p| p.team_id == team_id && p.profile.group_id == UNGROUPED).count()
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::testkit::{cx, world};
    use super::*;
    use serde_json::json;

    fn prof(name: &str) -> Profile {
        serde_json::from_value(json!({
            "name": name, "password": "pw-secret", "cookie": "c00kie",
            "user_proxy_config": {"proxy_host": "h", "proxy_port": "1", "proxy_password": "px-secret"}
        }))
        .unwrap()
    }

    #[test]
    fn serials_count_per_team_and_are_never_reused() {
        let (mut st, admin, ann, _bob, tid) = world();
        let a = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap();
        let b = st.create_profile(&mut cx(&ann), &tid, prof("b")).unwrap();
        assert_eq!((a.profile.serial_number, b.profile.serial_number), (1, 2));
        assert!(a.profile.profile_id.starts_with('k') && a.profile.profile_id.len() == 7);
        st.delete_profile(&mut cx(&admin), &b.profile.profile_id).unwrap();
        let c = st.create_profile(&mut cx(&ann), &tid, prof("c")).unwrap();
        assert_eq!(c.profile.serial_number, 3);
        // a second team starts at 1 again
        let t2 = st.create_team(&mut cx(&admin), "Beta").unwrap().team.id;
        assert_eq!(st.create_profile(&mut cx(&admin), &t2, prof("x")).unwrap().profile.serial_number, 1);
    }

    #[test]
    fn create_ignores_server_owned_fields_and_checks_the_group() {
        let (mut st, _admin, ann, _bob, tid) = world();
        let mut p = prof("a");
        (p.profile_id, p.serial_number, p.created_time, p.last_open_time) = ("kevil".into(), 99, 5, 6);
        let out = st.create_profile(&mut cx(&ann), &tid, p).unwrap();
        assert_ne!(out.profile.profile_id, "kevil");
        assert_eq!((out.profile.serial_number, out.profile.created_time, out.profile.last_open_time), (1, 1000, 0));
        assert_eq!(out.created_by, "ann");
        let mut bad = prof("b");
        bad.group_id = "nope".into();
        assert_eq!(st.create_profile(&mut cx(&ann), &tid, bad).unwrap_err().code, "invalid");
        let g = st.create_group(&mut cx(&ann), &tid, "Shops", "").unwrap();
        let mut by_name = prof("c");
        by_name.group_id = "shops".into();
        assert_eq!(st.create_profile(&mut cx(&ann), &tid, by_name).unwrap().profile.group_id, g.id);
    }

    #[test]
    fn members_get_masked_views_admins_and_holders_get_secrets() {
        let (mut st, admin, ann, bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let dump = |p: &HubProfile| serde_json::to_string(p).unwrap();
        let masked = st.profile_detail(&ann.user, &pid, None, 1_000_000).unwrap();
        assert!(!dump(&masked).contains("pw-secret") && !dump(&masked).contains("px-secret"));
        assert!(dump(&st.profile_detail(&admin.user, &pid, None, 1_000_000).unwrap()).contains("pw-secret"));
        let lease = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        let held = st.profile_detail(&ann.user, &pid, Some(&lease.lease), 1_000_001).unwrap();
        assert!(dump(&held).contains("pw-secret") && dump(&held).contains("c00kie"));
        assert_eq!(held.lock.as_ref().unwrap().device_name, "Laptop");
        assert!(held.lock.as_ref().unwrap().device_id == "d1");
        // a made-up lease is rejected loudly (and says who holds the profile), an outsider sees nothing
        let e = st.profile_detail(&ann.user, &pid, Some("ls_x"), 1_000_001).unwrap_err();
        assert_eq!((e.code, e.holder.map(|h| h.user)), ("lease_invalid", Some("ann".to_owned())));
        assert_eq!(st.profile_detail(&bob.user, &pid, None, 1_000_001).unwrap_err().code, "not_found");
        // lists never reveal
        let page = st.list_profiles(&admin.user, &tid, &ProfileQuery::default(), 1_000_001).unwrap();
        assert!(!dump(&page.profiles[0]).contains("pw-secret"));
        assert_eq!(page.profiles[0].lock.as_ref().map(|l| l.user.as_str()), Some("ann"));
    }

    #[test]
    fn lock_state_machine() {
        let (mut st, admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let l1 = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        assert_eq!(st.profiles[&pid].profile.last_open_time, 1000);
        assert_eq!(st.profiles[&pid].last_open_by, "ann");
        // the same device asks again (a restarted client): it gets a fresh lease and the old one is void
        let again = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        assert_ne!(again.lease, l1.lease);
        assert_eq!(st.heartbeat(&mut cx(&ann), &pid, Some(&l1.lease)).unwrap_err().code, "lease_invalid");
        st.heartbeat(&mut cx(&ann), &pid, Some(&again.lease)).unwrap();
        let l1 = again;
        // other device of the same user, and another user (admin): denied with holder info
        for (user, dev) in [(&ann, "d2"), (&admin, "d1")] {
            let e = st.lock_profile(&mut cx(user), &pid, dev, "Other").unwrap_err();
            assert_eq!(e.code, "locked");
            let h = e.holder.unwrap();
            assert_eq!((h.user.as_str(), h.device_name.as_str()), ("ann", "Laptop"));
        }
        // heartbeat extends; a wrong lease and a missing lease are told apart
        let mut late = cx(&ann);
        late.now_ms += 60_000;
        let hb = st.heartbeat(&mut late, &pid, Some(&l1.lease)).unwrap();
        assert!(hb.expires_at > l1.expires_at);
        assert_eq!(st.heartbeat(&mut cx(&ann), &pid, Some("ls_nope")).unwrap_err().code, "lease_invalid");
        assert_eq!(st.heartbeat(&mut cx(&ann), &pid, None).unwrap_err().code, "locked");
        // expiry: the lock stops counting and the next device takes over
        let mut after = cx(&admin);
        after.now_ms += 60_000 + 90_000;
        let l2 = st.lock_profile(&mut after, &pid, "d9", "Desk").unwrap();
        assert_ne!(l2.lease, l1.lease);
        assert!(after.events.iter().any(|e| e.message.contains("took over the expired lock of ann on Laptop")));
        // the old lease is dead for heartbeat/unlock/data
        let mut t = cx(&ann);
        t.now_ms = after.now_ms;
        assert_eq!(st.heartbeat(&mut t, &pid, Some(&l1.lease)).unwrap_err().code, "lease_invalid");
        assert_eq!(st.unlock(&mut t, &pid, Some(&l1.lease), true).unwrap_err().code, "lease_invalid");
        assert_eq!(st.data_access(&ann.user, &pid, Some(&l1.lease), t.now_ms).unwrap_err().code, "lease_invalid");
        // unlock(closed) stamps the close fields
        let mut c = cx(&admin);
        c.now_ms = after.now_ms + 5_000;
        st.unlock(&mut c, &pid, Some(&l2.lease), true).unwrap();
        let p = &st.profiles[&pid];
        assert_eq!((p.profile.last_close_time, p.last_close_by.as_str()), (1000 + 150 + 5, "root"));
        assert!(st.effective_lock(&pid, c.now_ms).is_none());
    }

    #[test]
    fn unlock_without_closing_leaves_the_close_stamp() {
        let (mut st, _admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let l = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        st.unlock(&mut cx(&ann), &pid, Some(&l.lease), false).unwrap();
        assert_eq!(st.profiles[&pid].profile.last_close_time, 0);
    }

    #[test]
    fn force_unlock_needs_owner_or_admin_and_is_idempotent() {
        let (mut st, admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        assert_eq!(st.force_unlock(&mut cx(&ann), &pid).unwrap_err().code, "forbidden");
        assert!(st.force_unlock(&mut cx(&admin), &pid).unwrap());
        assert!(!st.force_unlock(&mut cx(&admin), &pid).unwrap());
    }

    #[test]
    fn edits_are_gated_by_the_lock() {
        let (mut st, admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let l = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        // someone else (and the same user without the lease) -> locked
        assert_eq!(st.patch_profile(&mut cx(&admin), &pid, &json!({"name": "x"}), None).unwrap_err().code, "locked");
        assert_eq!(st.patch_profile(&mut cx(&ann), &pid, &json!({"name": "x"}), None).unwrap_err().code, "locked");
        assert_eq!(st.delete_profile(&mut cx(&admin), &pid).unwrap_err().code, "locked");
        // the holder edits with the lease; a bogus lease is rejected loudly
        let out = st.patch_profile(&mut cx(&ann), &pid, &json!({"name": "mine"}), Some(&l.lease)).unwrap();
        assert_eq!(out.profile.name, "mine");
        assert_eq!(
            st.patch_profile(&mut cx(&ann), &pid, &json!({"name": "y"}), Some("ls_bad")).unwrap_err().code,
            "lease_invalid"
        );
        st.unlock(&mut cx(&ann), &pid, Some(&l.lease), true).unwrap();
        assert_eq!(
            st.patch_profile(&mut cx(&admin), &pid, &json!({"remark": "ok"}), None).unwrap().profile.remark,
            "ok"
        );
    }

    #[test]
    fn patch_merges_protects_server_fields_and_keeps_masked_secrets() {
        let (mut st, admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let g = st.create_group(&mut cx(&ann), &tid, "Shops", "").unwrap();
        let out = st
            .patch_profile(
                &mut cx(&ann),
                &pid,
                &json!({"remark": "r", "group_id": g.id, "profile_id": "kzzz", "serial_number": 77, "created_time": 1,
                        "last_open_time": 9, "password": "********", "user_proxy_config": {"proxy_host": "h2"}}),
                None,
            )
            .unwrap();
        assert_eq!((out.profile.remark.as_str(), out.profile.group_id.as_str()), ("r", g.id.as_str()));
        let p = &st.profiles[&pid].profile;
        assert_eq!(
            (p.profile_id.as_str(), p.serial_number, p.created_time, p.last_open_time),
            (pid.as_str(), 1, 1000, 0)
        );
        assert_eq!(p.password, "pw-secret", "an echoed mask must not overwrite the secret");
        assert_eq!(p.user_proxy_config.proxy_host.as_deref(), Some("h2"));
        assert_eq!(p.user_proxy_config.proxy_password.as_deref(), Some("px-secret"), "merge keeps sibling keys");
        // members get a masked answer, admins a full one
        assert_eq!(out.profile.password, "********");
        let full = st.patch_profile(&mut cx(&admin), &pid, &json!({"name": "n2"}), None).unwrap();
        assert_eq!(full.profile.password, "pw-secret");
        // bad input
        assert_eq!(st.patch_profile(&mut cx(&ann), &pid, &json!([1]), None).unwrap_err().code, "invalid");
        assert_eq!(
            st.patch_profile(&mut cx(&ann), &pid, &json!({"tabs": "not-a-list"}), None).unwrap_err().code,
            "invalid"
        );
        assert_eq!(
            st.patch_profile(&mut cx(&ann), &pid, &json!({"group_id": "ghost"}), None).unwrap_err().code,
            "invalid"
        );
        // the failed patches changed nothing
        assert_eq!(st.profiles[&pid].profile.name, "n2");
        // null removes optional keys through the merge
        st.patch_profile(&mut cx(&ann), &pid, &json!({"user_proxy_config": {"proxy_host": null}}), None).unwrap();
        assert!(st.profiles[&pid].profile.user_proxy_config.proxy_host.is_none());
    }

    #[test]
    fn only_admins_delete_and_outsiders_see_nothing() {
        let (mut st, admin, ann, bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        assert_eq!(st.delete_profile(&mut cx(&ann), &pid).unwrap_err().code, "forbidden");
        assert_eq!(st.delete_profile(&mut cx(&bob), &pid).unwrap_err().code, "not_found");
        assert_eq!(st.lock_profile(&mut cx(&bob), &pid, "d", "x").unwrap_err().code, "not_found");
        assert_eq!(st.list_profiles(&bob.user, &tid, &ProfileQuery::default(), 0).unwrap_err().code, "not_found");
        st.delete_profile(&mut cx(&admin), &pid).unwrap();
        assert!(st.profiles.is_empty());
        assert_eq!(st.delete_profile(&mut cx(&admin), &pid).unwrap_err().code, "not_found");
    }

    #[test]
    fn list_filters_by_group_query_and_pages() {
        let (mut st, _admin, ann, _bob, tid) = world();
        let g = st.create_group(&mut cx(&ann), &tid, "Shops", "").unwrap();
        for (n, grp) in [("alpha shop", g.id.as_str()), ("beta", "0"), ("Gamma SHOP", g.id.as_str())] {
            let mut p = prof(n);
            p.group_id = grp.into();
            p.remark = format!("remark of {n}");
            st.create_profile(&mut cx(&ann), &tid, p).unwrap();
        }
        let ls = |st: &State, q: ProfileQuery| st.list_profiles(&ann.user, &tid, &q, 0).unwrap();
        let names = |p: &ProfilePage| p.profiles.iter().map(|x| x.profile.name.clone()).collect::<Vec<_>>();
        assert_eq!(ls(&st, ProfileQuery::default()).total, 3);
        assert_eq!(
            names(&ls(&st, ProfileQuery { group: Some("shops".into()), ..Default::default() })),
            ["alpha shop", "Gamma SHOP"]
        );
        assert_eq!(names(&ls(&st, ProfileQuery { group: Some("0".into()), ..Default::default() })), ["beta"]);
        assert!(ls(&st, ProfileQuery { group: Some("ghost".into()), ..Default::default() }).profiles.is_empty());
        assert_eq!(
            names(&ls(&st, ProfileQuery { q: Some("SHOP".into()), ..Default::default() })),
            ["alpha shop", "Gamma SHOP"]
        );
        // the serial number matches exactly (random profile ids may contain a `2` too, so only check inclusion)
        assert!(
            names(&ls(&st, ProfileQuery { q: Some("2".into()), ..Default::default() })).contains(&"beta".to_string())
        );
        assert!(
            !names(&ls(&st, ProfileQuery { q: Some("4".into()), ..Default::default() })).contains(&"beta".to_string())
        );
        let page = ls(&st, ProfileQuery { limit: Some(1), offset: Some(1), ..Default::default() });
        assert_eq!((names(&page), page.total), (vec!["beta".to_string()], 3));
        assert_eq!(st.ungrouped(&tid), 1);
    }

    #[test]
    fn data_commit_checks_lease_and_base_version() {
        let (mut st, _admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        assert_eq!(st.data_access(&ann.user, &pid, None, 1_000_000).unwrap_err().code, "lease_required");
        let l = st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        assert_eq!(st.data_access(&ann.user, &pid, Some(&l.lease), 1_000_000).unwrap().version, 0);
        let mut c = cx(&ann);
        assert_eq!(st.data_commit_check(&c, &pid, Some(&l.lease), Some(0)).unwrap(), 0);
        let m = st.data_commit_apply(&mut c, &pid, 10, "ab".into());
        assert_eq!((m.version, m.size), (1, 10));
        let e = st.data_commit_check(&c, &pid, Some(&l.lease), Some(0)).unwrap_err();
        assert_eq!((e.code, e.current_version), ("stale", Some(1)));
        assert_eq!(st.data_commit_check(&c, &pid, Some(&l.lease), None).unwrap(), 1, "no base_version = no check");
    }

    #[test]
    fn removing_a_member_releases_their_locks() {
        let (mut st, admin, ann, _bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        st.lock_profile(&mut cx(&ann), &pid, "d1", "Laptop").unwrap();
        st.remove_member(&mut cx(&admin), &tid, &ann.user.id).unwrap();
        assert!(st.effective_lock(&pid, 1_000_001).is_none());
    }

    #[test]
    fn bulk_delete_reports_skips() {
        let (mut st, admin, ann, _bob, tid) = world();
        let a = st.create_profile(&mut cx(&ann), &tid, prof("a")).unwrap().profile.profile_id;
        let b = st.create_profile(&mut cx(&ann), &tid, prof("b")).unwrap().profile.profile_id;
        st.lock_profile(&mut cx(&ann), &b, "d1", "Laptop").unwrap();
        let (del, skipped) = st.bulk_delete(&mut cx(&admin), &tid, &[a.clone(), b.clone(), "kmissing".into()]).unwrap();
        assert_eq!(del, [a]);
        assert_eq!(skipped, [(b, "locked".to_string()), ("kmissing".to_string(), "not found".to_string())]);
        assert_eq!(st.bulk_delete(&mut cx(&ann), &tid, &[]).unwrap_err().code, "forbidden");
    }
}
