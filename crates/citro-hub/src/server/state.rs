//! The persisted hub state (`<data>/state.json`) and the rules about users, teams, members and groups.
//!
//! Everything in here is plain synchronous data manipulation on `&mut State`; the server runs each request's
//! operation under one mutex, so every method is atomic with respect to every other request. A method must validate
//! first and mutate afterwards: when it returns `Err` the state is unchanged.

use super::error::{ApiError, ApiResult};
use super::util::{clean_text, ct_eq, random_id, token_hash};
use crate::model::{
    Holder, HubGroup, HubProfile, Lease, LockInfo, LogLevel, Member, Team, TeamEntry, TeamRole, UNGROUPED, User, UserRole,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bump when the layout of `state.json` changes incompatibly.
pub const SCHEMA: u32 = 1;

/// A user plus the SHA-256 of its token (the token itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRec {
    #[serde(flatten)]
    pub user: User,
    /// Lower-case hex SHA-256 of the `citro_...` token; empty = no valid token (revoked).
    #[serde(default)]
    pub token_hash: String,
}

fn one() -> u64 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamRec {
    #[serde(flatten)]
    pub team: Team,
    /// The serial number the next profile of this team gets (never reused).
    #[serde(default = "one")]
    pub next_serial: u64,
}

/// A lock with its secret lease. Expiry is kept in milliseconds so sub-second TTLs behave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRec {
    pub user_id: String,
    pub user: String,
    pub device_id: String,
    pub device_name: String,
    pub lease: String,
    pub since: i64,
    pub expires_ms: i64,
}

impl LockRec {
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_ms
    }

    /// Whole seconds, rounded up (so a lock never looks expired to a client before it is).
    pub fn expires_at(&self) -> i64 {
        (self.expires_ms + 999).div_euclid(1000)
    }

    pub fn holder(&self) -> Holder {
        Holder {
            user: self.user.clone(),
            device_name: self.device_name.clone(),
            since: self.since,
            expires_at: self.expires_at(),
        }
    }

    pub fn info(&self) -> LockInfo {
        LockInfo {
            user: self.user.clone(),
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            since: self.since,
            expires_at: self.expires_at(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub schema: u32,
    pub created_at: i64,
    /// Group ids are global (`PATCH /v1/groups/{gid}` has no team in the path), numeric, never reused; `"0"` is
    /// the reserved "ungrouped" id.
    pub next_group_id: u64,
    pub users: Vec<UserRec>,
    pub teams: BTreeMap<String, TeamRec>,
    pub members: Vec<Member>,
    pub groups: BTreeMap<String, HubGroup>,
    pub profiles: BTreeMap<String, HubProfile>,
    /// Locks by profile id. A record may be expired; it only counts while `!expired(now)`.
    pub locks: BTreeMap<String, LockRec>,
}

impl Default for State {
    fn default() -> State {
        State {
            schema: SCHEMA,
            created_at: 0,
            next_group_id: 1,
            users: Vec::new(),
            teams: BTreeMap::new(),
            members: Vec::new(),
            groups: BTreeMap::new(),
            profiles: BTreeMap::new(),
            locks: BTreeMap::new(),
        }
    }
}

/// One log line the server wants to write about what a request did.
#[derive(Debug, Clone)]
pub struct Event {
    pub team_id: String,
    pub level: LogLevel,
    pub user: String,
    pub device_id: String,
    pub profile_id: Option<String>,
    pub message: String,
}

/// Per-request context handed to the state operations.
#[derive(Debug)]
pub struct Cx {
    pub user: User,
    pub device_id: String,
    pub now_ms: i64,
    pub ttl_ms: i64,
    pub max_blob: u64,
    pub events: Vec<Event>,
    /// Set by operations that only extend a lease: persisting them may be deferred a few seconds.
    pub lazy: bool,
}

impl Cx {
    pub fn new(user: User, device_id: &str, now_ms: i64, ttl_ms: i64, max_blob: u64) -> Cx {
        Cx { user, device_id: device_id.to_owned(), now_ms, ttl_ms, max_blob, events: Vec::new(), lazy: false }
    }

    pub fn now(&self) -> i64 {
        self.now_ms.div_euclid(1000)
    }

    pub fn log(&mut self, team_id: &str, level: LogLevel, profile_id: Option<&str>, message: impl Into<String>) {
        self.events.push(Event {
            team_id: team_id.to_owned(),
            level,
            user: self.user.name.clone(),
            device_id: self.device_id.clone(),
            profile_id: profile_id.map(str::to_owned),
            message: message.into(),
        });
    }

    /// The lease answer for a lock record.
    pub fn lease_of(&self, pid: &str, l: &LockRec) -> Lease {
        Lease {
            lease: l.lease.clone(),
            expires_at: l.expires_at(),
            ttl_secs: (self.ttl_ms / 1000).max(1) as u64,
            profile_id: pid.to_owned(),
            max_data_bytes: self.max_blob,
        }
    }
}

// ---- lookups and access control ------------------------------------------------------------------------------------

impl State {
    pub fn user_by_token(&self, token: &str) -> Option<&UserRec> {
        let hash = token_hash(token);
        let mut found = None;
        // No early exit: every stored hash is compared, whatever the outcome.
        for u in &self.users {
            if !u.token_hash.is_empty() && ct_eq(&u.token_hash, &hash) {
                found = Some(u);
            }
        }
        found.filter(|u| !u.user.disabled)
    }

    pub fn user_by_name(&self, name: &str) -> Option<&UserRec> {
        let name = name.trim();
        self.users.iter().find(|u| u.user.name.eq_ignore_ascii_case(name))
    }

    /// The caller's role in a team: its membership, `Owner` for server admins, `None` for outsiders and for teams
    /// that do not exist (so non-members cannot tell the difference).
    pub fn role_in(&self, user: &User, team_id: &str) -> Option<TeamRole> {
        if !self.teams.contains_key(team_id) {
            return None;
        }
        if user.role == UserRole::Admin {
            return Some(TeamRole::Owner);
        }
        self.members.iter().find(|m| m.team_id == team_id && m.user_id == user.id).map(|m| m.role)
    }

    /// `404` for outsiders, `403` for members that lack `min`.
    pub fn require_role(&self, user: &User, team_id: &str, min: TeamRole) -> ApiResult<TeamRole> {
        match self.role_in(user, team_id) {
            None => Err(ApiError::not_found("no such team")),
            Some(r) if r < min => Err(ApiError::forbidden(match min {
                TeamRole::Owner => "this needs the team owner",
                _ => "this needs a team owner or admin",
            })),
            Some(r) => Ok(r),
        }
    }

    pub fn profile_role(&self, user: &User, pid: &str) -> ApiResult<(&HubProfile, TeamRole)> {
        let hp = self.profiles.get(pid).ok_or_else(|| ApiError::not_found("no such profile"))?;
        match self.role_in(user, &hp.team_id) {
            Some(r) => Ok((hp, r)),
            None => Err(ApiError::not_found("no such profile")),
        }
    }

    pub fn group_role(&self, user: &User, gid: &str) -> ApiResult<(&HubGroup, TeamRole)> {
        let g = self.groups.get(gid).ok_or_else(|| ApiError::not_found("no such group"))?;
        match self.role_in(user, &g.team_id) {
            Some(r) => Ok((g, r)),
            None => Err(ApiError::not_found("no such group")),
        }
    }

    /// The lock of a profile unless it has expired.
    pub fn effective_lock(&self, pid: &str, now_ms: i64) -> Option<&LockRec> {
        self.locks.get(pid).filter(|l| !l.expired(now_ms))
    }

    pub fn team_name(&self, team_id: &str) -> String {
        self.teams.get(team_id).map(|t| t.team.name.clone()).unwrap_or_default()
    }

    // ---- teams and members -------------------------------------------------------------------------------------

    /// The teams the user can see: its own, or every team for a server admin.
    pub fn teams_of(&self, user: &User) -> Vec<TeamEntry> {
        let mut out: Vec<TeamEntry> = self
            .teams
            .values()
            .filter_map(|t| self.role_in(user, &t.team.id).map(|role| TeamEntry { team: t.team.clone(), role }))
            .collect();
        out.sort_by(|a, b| a.team.created_at.cmp(&b.team.created_at).then_with(|| a.team.id.cmp(&b.team.id)));
        out
    }

    pub fn create_team(&mut self, cx: &mut Cx, name: &str) -> ApiResult<TeamEntry> {
        if cx.user.role != UserRole::Admin {
            return Err(ApiError::forbidden("only server admins create teams"));
        }
        let name = clean_text(name, 100).ok_or_else(|| ApiError::invalid("team name must be 1-100 characters"))?;
        if self.teams.values().any(|t| t.team.name.eq_ignore_ascii_case(&name)) {
            return Err(ApiError::invalid("a team with this name already exists"));
        }
        let id = loop {
            let id = random_id("t", 8);
            if !self.teams.contains_key(&id) {
                break id;
            }
        };
        let team = Team { id: id.clone(), name: name.clone(), created_at: cx.now() };
        self.teams.insert(id.clone(), TeamRec { team: team.clone(), next_serial: 1 });
        self.members.push(Member {
            team_id: id.clone(),
            user_id: cx.user.id.clone(),
            user_name: cx.user.name.clone(),
            role: TeamRole::Owner,
        });
        cx.log(&id, LogLevel::Info, None, format!("team `{name}` created"));
        Ok(TeamEntry { team, role: TeamRole::Owner })
    }

    pub fn members_of(&self, user: &User, team_id: &str) -> ApiResult<Vec<Member>> {
        self.require_role(user, team_id, TeamRole::Member)?;
        let mut out: Vec<Member> = self.members.iter().filter(|m| m.team_id == team_id).cloned().collect();
        out.sort_by(|a, b| b.role.cmp(&a.role).then_with(|| a.user_name.cmp(&b.user_name)));
        Ok(out)
    }

    fn owner_count(&self, team_id: &str) -> usize {
        self.members.iter().filter(|m| m.team_id == team_id && m.role == TeamRole::Owner).count()
    }

    /// Add a user to a team or change its role there.
    pub fn add_member(&mut self, cx: &mut Cx, team_id: &str, user_name: &str, role: TeamRole) -> ApiResult<Member> {
        let caller = self.require_role(&cx.user, team_id, TeamRole::Admin)?;
        let target = match self.user_by_name(user_name) {
            Some(u) if !u.user.disabled => u.user.clone(),
            Some(_) => return Err(ApiError::invalid(format!("user `{}` is revoked", user_name.trim()))),
            None => return Err(ApiError::invalid(format!("unknown user `{}`", user_name.trim()))),
        };
        if role == TeamRole::Owner && caller < TeamRole::Owner {
            return Err(ApiError::forbidden("only an owner can make someone an owner"));
        }
        let existing = self.members.iter().position(|m| m.team_id == team_id && m.user_id == target.id);
        if let Some(i) = existing {
            let old = self.members[i].role;
            if old == TeamRole::Owner && role != TeamRole::Owner {
                if caller < TeamRole::Owner {
                    return Err(ApiError::forbidden("only an owner can demote an owner"));
                }
                if self.owner_count(team_id) <= 1 {
                    return Err(ApiError::invalid("a team must keep at least one owner"));
                }
            }
            self.members[i].role = role;
            let m = self.members[i].clone();
            cx.log(team_id, LogLevel::Info, None, format!("member `{}` is now {}", m.user_name, role.as_str()));
            return Ok(m);
        }
        let m = Member { team_id: team_id.to_owned(), user_id: target.id.clone(), user_name: target.name.clone(), role };
        self.members.push(m.clone());
        cx.log(team_id, LogLevel::Info, None, format!("member `{}` added as {}", m.user_name, role.as_str()));
        Ok(m)
    }

    /// Remove a member (or let a member leave). Locks the user holds in the team are released.
    pub fn remove_member(&mut self, cx: &mut Cx, team_id: &str, user_id: &str) -> ApiResult<Member> {
        let caller = self.require_role(&cx.user, team_id, TeamRole::Member)?;
        let idx = self
            .members
            .iter()
            .position(|m| m.team_id == team_id && m.user_id == user_id)
            .ok_or_else(|| ApiError::not_found("no such member"))?;
        let target = self.members[idx].clone();
        let leaving = target.user_id == cx.user.id;
        if !leaving && caller < TeamRole::Admin {
            return Err(ApiError::forbidden("this needs a team owner or admin"));
        }
        if target.role == TeamRole::Owner {
            if !leaving && caller < TeamRole::Owner {
                return Err(ApiError::forbidden("only an owner can remove an owner"));
            }
            if self.owner_count(team_id) <= 1 {
                return Err(ApiError::invalid("a team must keep at least one owner"));
            }
        }
        self.members.remove(idx);
        let held: Vec<String> = self
            .locks
            .iter()
            .filter(|(pid, l)| l.user_id == target.user_id && self.profiles.get(*pid).is_some_and(|p| p.team_id == team_id))
            .map(|(pid, _)| pid.clone())
            .collect();
        for pid in held {
            self.locks.remove(&pid);
            cx.log(team_id, LogLevel::Warn, Some(&pid), format!("lock of removed member `{}` released", target.user_name));
        }
        cx.log(team_id, LogLevel::Info, None, format!("member `{}` removed", target.user_name));
        Ok(target)
    }

    // ---- groups ------------------------------------------------------------------------------------------------

    pub fn groups_of(&self, user: &User, team_id: &str) -> ApiResult<Vec<HubGroup>> {
        self.require_role(user, team_id, TeamRole::Member)?;
        let mut out: Vec<HubGroup> = self.groups.values().filter(|g| g.team_id == team_id).cloned().collect();
        out.sort_by_key(|g| g.id.parse::<u64>().unwrap_or(u64::MAX));
        Ok(out)
    }

    /// A group id or exact (case-insensitive) group name of the team -> the group id; `"0"` / `""` = ungrouped.
    pub fn resolve_group(&self, team_id: &str, key: &str) -> Option<String> {
        let key = key.trim();
        if key.is_empty() || key == UNGROUPED {
            return Some(UNGROUPED.to_owned());
        }
        if self.groups.get(key).is_some_and(|g| g.team_id == team_id) {
            return Some(key.to_owned());
        }
        self.groups.values().find(|g| g.team_id == team_id && g.name.eq_ignore_ascii_case(key)).map(|g| g.id.clone())
    }

    pub fn group_name(&self, group_id: &str) -> String {
        if group_id.is_empty() || group_id == UNGROUPED {
            return "Ungrouped".to_owned();
        }
        self.groups.get(group_id).map(|g| g.name.clone()).unwrap_or_default()
    }

    fn check_group_text(name: &str, remark: &str) -> ApiResult<String> {
        let name = clean_text(name, 100).ok_or_else(|| ApiError::invalid("group name must be 1-100 characters"))?;
        if remark.chars().count() > 2000 {
            return Err(ApiError::invalid("group remark is longer than 2000 characters"));
        }
        Ok(name)
    }

    pub fn create_group(&mut self, cx: &mut Cx, team_id: &str, name: &str, remark: &str) -> ApiResult<HubGroup> {
        self.require_role(&cx.user, team_id, TeamRole::Member)?;
        let name = Self::check_group_text(name, remark)?;
        if self.groups.values().any(|g| g.team_id == team_id && g.name.eq_ignore_ascii_case(&name)) {
            return Err(ApiError::invalid("a group with this name already exists"));
        }
        let id = self.next_group_id.to_string();
        self.next_group_id += 1;
        let g = HubGroup { id: id.clone(), team_id: team_id.to_owned(), name: name.clone(), remark: remark.to_owned() };
        self.groups.insert(id, g.clone());
        cx.log(team_id, LogLevel::Info, None, format!("group `{name}` created"));
        Ok(g)
    }

    pub fn update_group(
        &mut self,
        cx: &mut Cx,
        gid: &str,
        name: Option<&str>,
        remark: Option<&str>,
    ) -> ApiResult<HubGroup> {
        let (g, _) = self.group_role(&cx.user, gid)?;
        let team_id = g.team_id.clone();
        let new_name = match name {
            Some(n) => Self::check_group_text(n, remark.unwrap_or(&g.remark))?,
            None => Self::check_group_text(&g.name, remark.unwrap_or(&g.remark))?,
        };
        if self.groups.values().any(|o| o.team_id == team_id && o.id != gid && o.name.eq_ignore_ascii_case(&new_name)) {
            return Err(ApiError::invalid("a group with this name already exists"));
        }
        let g = self.groups.get_mut(gid).expect("checked above");
        let old = g.name.clone();
        g.name = new_name.clone();
        if let Some(r) = remark {
            g.remark = r.to_owned();
        }
        let out = g.clone();
        cx.log(&team_id, LogLevel::Info, None, format!("group `{old}` updated (now `{new_name}`)"));
        Ok(out)
    }

    /// Delete a group; its profiles move to the ungrouped group. Returns how many profiles moved.
    pub fn delete_group(&mut self, cx: &mut Cx, gid: &str) -> ApiResult<usize> {
        let (g, role) = self.group_role(&cx.user, gid)?;
        if role < TeamRole::Admin {
            return Err(ApiError::forbidden("this needs a team owner or admin"));
        }
        let (team_id, name) = (g.team_id.clone(), g.name.clone());
        let now = cx.now();
        let mut moved = 0;
        for p in self.profiles.values_mut().filter(|p| p.team_id == team_id && p.profile.group_id == gid) {
            p.profile.group_id = UNGROUPED.to_owned();
            p.updated_at = now;
            moved += 1;
        }
        self.groups.remove(gid);
        cx.log(&team_id, LogLevel::Info, None, format!("group `{name}` deleted ({moved} profiles moved to ungrouped)"));
        Ok(moved)
    }

    /// Every user, as listed by the admin tooling.
    pub fn users(&self) -> Vec<User> {
        self.users.iter().map(|u| u.user.clone()).collect()
    }
}

/// Fixtures shared by the unit tests of the state modules.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;

    pub fn user(id: &str, name: &str, role: UserRole) -> UserRec {
        UserRec {
            user: User { id: id.into(), name: name.into(), role, disabled: false, created_at: 1 },
            token_hash: token_hash(&format!("citro_{name}")),
        }
    }

    /// A request context at t = 1000 s with a 90 s lease TTL and a 1 MiB blob limit.
    pub fn cx(u: &UserRec) -> Cx {
        Cx::new(u.user.clone(), "dev", 1_000_000, 90_000, 1 << 20)
    }

    /// root (server admin), ann and bob (members), one team "Alpha" (root = owner, ann = member, bob outside).
    pub fn world() -> (State, UserRec, UserRec, UserRec, String) {
        let mut st = State::default();
        let (admin, ann, bob) =
            (user("u1", "root", UserRole::Admin), user("u2", "ann", UserRole::Member), user("u3", "bob", UserRole::Member));
        st.users = vec![admin.clone(), ann.clone(), bob.clone()];
        let tid = st.create_team(&mut cx(&admin), "Alpha").unwrap().team.id;
        st.add_member(&mut cx(&admin), &tid, "ann", TeamRole::Member).unwrap();
        (st, admin, ann, bob, tid)
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{cx, user};
    use super::*;

    fn world() -> (State, UserRec, UserRec, UserRec) {
        let mut st = State::default();
        let (admin, ann, bob) =
            (user("u1", "root", UserRole::Admin), user("u2", "ann", UserRole::Member), user("u3", "bob", UserRole::Member));
        st.users = vec![admin.clone(), ann.clone(), bob.clone()];
        (st, admin, ann, bob)
    }

    #[test]
    fn token_lookup_ignores_disabled_and_blank_hashes() {
        let (mut st, ..) = world();
        assert_eq!(st.user_by_token("citro_ann").unwrap().user.name, "ann");
        assert!(st.user_by_token("citro_nobody").is_none());
        st.users[1].user.disabled = true;
        assert!(st.user_by_token("citro_ann").is_none());
        st.users[2].token_hash.clear();
        assert!(st.user_by_token("").is_none());
    }

    #[test]
    fn only_admins_create_teams_and_outsiders_get_404() {
        let (mut st, admin, ann, bob) = world();
        assert_eq!(st.create_team(&mut cx(&ann), "x").unwrap_err().code, "forbidden");
        let t = st.create_team(&mut cx(&admin), "Alpha").unwrap();
        assert_eq!(t.role, TeamRole::Owner);
        assert_eq!(st.create_team(&mut cx(&admin), "alpha").unwrap_err().code, "invalid");
        // outsiders: 404 whether or not the team exists
        assert_eq!(st.require_role(&ann.user, &t.team.id, TeamRole::Member).unwrap_err().code, "not_found");
        assert_eq!(st.require_role(&ann.user, "nope", TeamRole::Member).unwrap_err().code, "not_found");
        st.add_member(&mut cx(&admin), &t.team.id, "ANN", TeamRole::Member).unwrap();
        assert_eq!(st.require_role(&ann.user, &t.team.id, TeamRole::Member).unwrap(), TeamRole::Member);
        assert_eq!(st.require_role(&ann.user, &t.team.id, TeamRole::Admin).unwrap_err().code, "forbidden");
        assert!(st.teams_of(&bob.user).is_empty());
        assert_eq!(st.teams_of(&ann.user).len(), 1);
        assert_eq!(st.teams_of(&admin.user).len(), 1);
    }

    #[test]
    fn admins_cannot_mint_or_remove_owners_and_the_last_owner_stays() {
        let (mut st, admin, ann, bob) = world();
        let tid = st.create_team(&mut cx(&admin), "Alpha").unwrap().team.id;
        st.add_member(&mut cx(&admin), &tid, "ann", TeamRole::Admin).unwrap();
        // an admin may add members but not owners
        assert_eq!(st.add_member(&mut cx(&ann), &tid, "bob", TeamRole::Owner).unwrap_err().code, "forbidden");
        st.add_member(&mut cx(&ann), &tid, "bob", TeamRole::Member).unwrap();
        // ... and cannot touch the owner (root)
        assert_eq!(st.remove_member(&mut cx(&ann), &tid, "u1").unwrap_err().code, "forbidden");
        // the sole owner cannot be removed or demoted, even by a server admin
        assert_eq!(st.remove_member(&mut cx(&admin), &tid, "u1").unwrap_err().code, "invalid");
        assert_eq!(st.add_member(&mut cx(&admin), &tid, "root", TeamRole::Member).unwrap_err().code, "invalid");
        // a plain member may leave but not remove others
        assert_eq!(st.remove_member(&mut cx(&bob), &tid, "u2").unwrap_err().code, "forbidden");
        st.remove_member(&mut cx(&bob), &tid, "u3").unwrap();
        assert!(st.role_in(&bob.user, &tid).is_none());
        // unknown / revoked users
        assert_eq!(st.add_member(&mut cx(&admin), &tid, "ghost", TeamRole::Member).unwrap_err().code, "invalid");
    }

    #[test]
    fn groups_are_unique_per_team_and_deleting_moves_profiles() {
        let (mut st, admin, ann, _) = world();
        let tid = st.create_team(&mut cx(&admin), "Alpha").unwrap().team.id;
        st.add_member(&mut cx(&admin), &tid, "ann", TeamRole::Member).unwrap();
        let g = st.create_group(&mut cx(&ann), &tid, "Shops", "all shops").unwrap();
        assert_eq!(g.id, "1");
        assert_eq!(st.create_group(&mut cx(&ann), &tid, "shops", "").unwrap_err().code, "invalid");
        assert_eq!(st.create_group(&mut cx(&ann), &tid, "  ", "").unwrap_err().code, "invalid");
        let renamed = st.update_group(&mut cx(&ann), &g.id, Some("Stores"), None).unwrap();
        assert_eq!((renamed.name.as_str(), renamed.remark.as_str()), ("Stores", "all shops"));
        assert_eq!(st.resolve_group(&tid, "stores").as_deref(), Some("1"));
        assert_eq!(st.resolve_group(&tid, "0").as_deref(), Some("0"));
        assert!(st.resolve_group(&tid, "nope").is_none());
        // members cannot delete groups, admins can
        assert_eq!(st.delete_group(&mut cx(&ann), &g.id).unwrap_err().code, "forbidden");
        let mut hp = HubProfile { team_id: tid.clone(), ..Default::default() };
        hp.profile.profile_id = "kabc123".into();
        hp.profile.group_id = g.id.clone();
        st.profiles.insert("kabc123".into(), hp);
        assert_eq!(st.delete_group(&mut cx(&admin), &g.id).unwrap(), 1);
        assert_eq!(st.profiles["kabc123"].profile.group_id, "0");
        assert!(st.groups.is_empty());
        assert_eq!(st.delete_group(&mut cx(&admin), &g.id).unwrap_err().code, "not_found");
    }

    #[test]
    fn lock_expiry_is_millisecond_exact_and_expires_at_rounds_up() {
        let l = LockRec {
            user_id: "u".into(),
            user: "ann".into(),
            device_id: "d".into(),
            device_name: "Laptop".into(),
            lease: "ls_x".into(),
            since: 10,
            expires_ms: 100_001,
        };
        assert!(!l.expired(100_000) && l.expired(100_001));
        assert_eq!(l.expires_at(), 101);
        assert_eq!(l.holder().device_name, "Laptop");
        assert_eq!(l.info().device_id, "d");
    }
}
