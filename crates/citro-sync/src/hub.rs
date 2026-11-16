//! The hub as one device uses it: log in, mirror the team's profiles, put a local profile on the hub, see who has
//! what open, edit through to the hub.
//!
//! The hub is the source of truth for everything about a hub profile. Lists never carry secrets (passwords, 2FA
//! keys, cookies, proxy passwords): the hub serves those only to the holder of the profile's lock, so the local
//! copy of a hub profile shows them masked and [`Hub::open`] fetches the real record with the lease.

use crate::error::{Error, Result};
use crate::session::{self, Outcome};
use crate::state::{self, Bound, Login};
use citro_core::Result as CoreResult;
use citro_core::launch;
use citro_core::profile::Profile;
use citro_core::store::{self, Home, Store};
use citro_hub::client::Client;
use citro_hub::model::{HubProfile, MASK, ProfileFilter, TeamEntry, UNGROUPED};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::fs;

#[derive(Clone, Debug)]
pub struct Hub {
    pub(crate) home: Home,
    pub(crate) client: Client,
    pub(crate) login: Login,
}

/// A profile named in a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub added: Vec<Entry>,
    pub updated: Vec<Entry>,
    /// Hub profiles of this team that are no longer on the hub (deleted there). They stay on this device, still
    /// bound; `citro hub unbind` frees one, `citro delete` removes it.
    pub gone: Vec<Entry>,
}

/// One profile that is open somewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRow {
    pub profile_id: String,
    pub name: String,
    pub user: String,
    pub device: String,
    /// Unix seconds.
    pub since: i64,
    /// Unix seconds: when the lock frees itself if its holder stops heartbeating.
    pub expires_at: i64,
    /// Held by this very device.
    pub mine: bool,
}

#[derive(Debug, Clone)]
pub struct PushReport {
    /// The profile as it is now: the hub's id, the hub's team serial number when it was free here.
    pub profile: Profile,
    /// What happened to the data (`uploaded` carries the version).
    pub outcome: Outcome,
}

fn team_names(teams: &[TeamEntry]) -> String {
    teams.iter().map(|t| t.team.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn pick_team<'a>(teams: &'a [TeamEntry], want: Option<&str>) -> Result<&'a TeamEntry> {
    match want.map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) => teams.iter().find(|t| t.team.id == w || t.team.name.eq_ignore_ascii_case(w)).ok_or_else(|| {
            Error::Invalid(format!("you are not in a team called `{w}` (yours: {})", team_names(teams)))
        }),
        None => match teams {
            [one] => Ok(one),
            [] => Err(Error::Invalid("this account is in no team yet: ask a hub admin to add you".into())),
            many => Err(Error::Invalid(format!(
                "this account is in several teams ({}): pick one with --team",
                team_names(many)
            ))),
        },
    }
}

/// The local copy of a hub profile keeps the secrets it already has when the hub's answer carries them masked.
pub(crate) fn keep_unmasked(local: Option<&Profile>, p: &mut Profile) {
    let Some(old) = local else { return };
    let keep = |new: &mut String, was: &str| {
        if new == MASK {
            *new = was.to_owned();
        }
    };
    keep(&mut p.password, &old.password);
    keep(&mut p.fakey, &old.fakey);
    keep(&mut p.cookie, &old.cookie);
    if p.user_proxy_config.proxy_password.as_deref() == Some(MASK) {
        p.user_proxy_config.proxy_password = old.user_proxy_config.proxy_password.clone();
    }
}

/// The RFC 7386 merge patch that turns `old` into `new`: objects merge key by key, anything else is replaced whole,
/// and a key that went away is `null` (so replacing a proxy does not leave the old proxy's keys behind on the hub).
pub(crate) fn merge_diff(old: &Value, new: &Value) -> Option<Value> {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut patch = Map::new();
            for (k, nv) in n {
                match o.get(k) {
                    Some(ov) => {
                        if let Some(d) = merge_diff(ov, nv) {
                            patch.insert(k.clone(), d);
                        }
                    }
                    None if nv.is_null() => {}
                    None => {
                        patch.insert(k.clone(), nv.clone());
                    }
                }
            }
            for k in o.keys().filter(|k| !n.contains_key(*k)) {
                patch.insert(k.clone(), Value::Null);
            }
            (!patch.is_empty()).then_some(Value::Object(patch))
        }
        _ if old == new => None,
        _ => Some(new.clone()),
    }
}

/// What differs between two versions of a profile. The id, serial number and the open/close times belong to the
/// hub and never go into a patch.
pub(crate) fn diff_patch(old: &Profile, new: &Profile) -> Result<Map<String, Value>> {
    const HUB_OWNED: [&str; 5] = ["profile_id", "serial_number", "created_time", "last_open_time", "last_close_time"];
    let (old, new) = (serde_json::to_value(old)?, serde_json::to_value(new)?);
    let Some(Value::Object(mut patch)) = merge_diff(&old, &new) else { return Ok(Map::new()) };
    patch.retain(|k, _| !HUB_OWNED.contains(&k.as_str()));
    Ok(patch)
}

impl Hub {
    /// The hub this home is logged in to; `None` when it is not logged in.
    pub fn connect(home: &Home) -> Result<Option<Hub>> {
        let Some(login) = state::load_login(home)? else { return Ok(None) };
        let client = Client::new(&login.url, &login.token, state::device(home)?)?;
        Ok(Some(Hub { home: home.clone(), client, login }))
    }

    /// Check the token against the hub and remember the login. `team` is needed only when the account is in
    /// several teams.
    pub fn log_in(home: &Home, url: &str, token: &str, device_name: Option<&str>, team: Option<&str>) -> Result<Hub> {
        let device = match device_name {
            Some(name) => state::rename_device(home, name)?,
            None => state::device(home)?,
        };
        let client = Client::new(url, token.trim(), device)?;
        let me = client.me()?;
        let entry = pick_team(&me.teams, team)?;
        let login = Login {
            url: client.base_url().to_owned(),
            token: token.trim().to_owned(),
            team_id: entry.team.id.clone(),
            team_name: entry.team.name.clone(),
            user: me.user.name.clone(),
        };
        state::save_login(home, &login)?;
        Ok(Hub { home: home.clone(), client, login })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn login(&self) -> &Login {
        &self.login
    }

    pub fn team_id(&self) -> &str {
        &self.login.team_id
    }

    pub fn device_name(&self) -> &str {
        &self.client.device().name
    }

    fn all_profiles(&self) -> Result<Vec<HubProfile>> {
        let mut all: Vec<HubProfile> = Vec::new();
        loop {
            let filter = ProfileFilter { limit: Some(500), offset: Some(all.len()), ..ProfileFilter::default() };
            let page = self.client.profiles(&self.login.team_id, &filter)?;
            let got = page.profiles.len();
            all.extend(page.profiles);
            if got == 0 || all.len() >= page.total {
                return Ok(all);
            }
        }
    }

    fn group_names(&self) -> Result<HashMap<String, String>> {
        Ok(self.client.groups(&self.login.team_id)?.into_iter().map(|g| (g.id, g.name)).collect())
    }

    /// The hub's id of the group called `name` (made when there is none); `""` = no group.
    fn hub_group_id(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name == UNGROUPED || name.eq_ignore_ascii_case("ungrouped") {
            return Ok(UNGROUPED.to_owned());
        }
        let groups = self.client.groups(&self.login.team_id)?;
        if let Some(g) = groups.iter().find(|g| g.name.eq_ignore_ascii_case(name)) {
            return Ok(g.id.clone());
        }
        Ok(self.client.create_group(&self.login.team_id, name, "")?.id)
    }

    /// Bring the team's profiles (names, remarks, groups, proxies, last open/close times) into the local store.
    /// Cookies and browser data are not touched: they come down when the profile is opened.
    pub fn pull(&self) -> Result<PullReport> {
        let groups = self.group_names()?;
        let all = self.all_profiles()?;
        let mut report = PullReport::default();
        store::locked(&self.home, |s| {
            for hp in &all {
                let mut p = hp.profile.clone();
                let (id, name) = (p.profile_id.clone(), p.name.clone());
                p.group_id = match groups.get(&p.group_id).filter(|n| !n.trim().is_empty()) {
                    Some(group) => s.group_id_for(group)?,
                    None => "0".into(),
                };
                let existing = s.profiles().iter().find(|x| x.profile_id == id).cloned();
                keep_unmasked(existing.as_ref(), &mut p);
                s.upsert_synced(p)?;
                let entry = Entry { id, name };
                if existing.is_some() { report.updated.push(entry) } else { report.added.push(entry) }
            }
            Ok(())
        })?;
        for e in report.added.iter().chain(&report.updated) {
            if Bound::load(&self.home, &e.id).is_none() {
                Bound::new(&self.login.team_id).save(&self.home, &e.id)?;
            }
        }
        let on_hub: BTreeSet<&str> = all.iter().map(|h| h.profile.profile_id.as_str()).collect();
        let local = store::Store::open(&self.home)?;
        for id in state::bound_ids(&self.home) {
            let ours = Bound::load(&self.home, &id).is_some_and(|b| b.team_id == self.login.team_id);
            if ours && !on_hub.contains(id.as_str()) {
                let name = local.profiles().iter().find(|p| p.profile_id == id).map(|p| p.name.clone());
                report.gone.push(Entry { name: name.unwrap_or_default(), id });
            }
        }
        Ok(report)
    }

    /// Put a local profile on the hub: the hub gives it its id (the persona seed is pinned to the old one, so the
    /// browser keeps its identity), and its cookies and data go up. From now on it belongs to the hub.
    pub fn push(&self, key: &str) -> Result<PushReport> {
        let (mut payload, group_name) = {
            let s = store::Store::open(&self.home)?;
            let p = s.get(key)?.clone();
            let group = s.group_name(&p.group_id);
            (p, group)
        };
        let old_id = payload.profile_id.clone();
        if self.home.hub_bound(&old_id) {
            return Err(Error::Invalid(format!("{old_id} is on a team hub already")));
        }
        if launch::running(&self.home, &old_id).is_some() {
            return Err(Error::Invalid(format!("{old_id} is open: close it first, its data goes up with it")));
        }
        let pinned = payload.fingerprint_config.str("seed").is_some_and(|s| !s.trim().is_empty());
        if !pinned {
            payload.fingerprint_config.0.insert("seed".into(), json!(old_id));
        }
        payload.group_id = self.hub_group_id(&group_name)?;
        let created = self.client.create_profile(&self.login.team_id, &payload)?;
        let new_id = created.profile.profile_id.clone();

        let (from, to) = (self.home.profile_dir(&old_id), self.home.profile_dir(&new_id));
        if from.exists() {
            fs::rename(&from, &to)?;
        }
        // the persona seed is pinned to the old id, so the browser keeps its identity under the new one; serial
        // numbers stay local (they are a per-device shorthand: the id is the same everywhere)
        let seed = payload.fingerprint_config.clone();
        let profile = store::locked(&self.home, |s| s.rekey(&old_id, &new_id, |p| p.fingerprint_config = seed))?;
        // what was in the data directory is the first copy: it is newer than the hub's (empty) one
        Bound { team_id: self.login.team_id.clone(), dirty: true, ..Bound::default() }.save(&self.home, &new_id)?;
        let outcome = session::publish(self, &new_id)?;
        Ok(PushReport { profile, outcome })
    }

    /// Who has what open.
    pub fn locks(&self) -> Result<Vec<LockRow>> {
        let me = &self.client.device().id;
        Ok(self
            .all_profiles()?
            .into_iter()
            .filter_map(|hp| {
                let l = hp.lock?;
                Some(LockRow {
                    profile_id: hp.profile.profile_id,
                    name: hp.profile.name,
                    mine: &l.device_id == me,
                    user: l.user,
                    device: l.device_name,
                    since: l.since,
                    expires_at: l.expires_at,
                })
            })
            .collect())
    }

    /// Break somebody's lock (team owners and admins). `true` when there was a live lock.
    pub fn force_unlock(&self, key: &str) -> Result<bool> {
        let id = self.resolve(key)?;
        Ok(self.client.force_unlock(&id)?)
    }

    /// A profile key (id, serial number or name) as the hub profile id it means.
    pub fn resolve(&self, key: &str) -> Result<String> {
        let s = store::Store::open(&self.home)?;
        Ok(s.find(key).map_or_else(|| key.to_owned(), |p| p.profile_id.clone()))
    }

    /// Change a hub profile: `apply` edits a copy of the local record (it sees the store, to look groups up), what
    /// differs goes to the hub (which refuses while anybody has the profile open), then the local copy follows.
    pub fn edit(&self, key: &str, apply: impl FnOnce(&Store, &mut Profile) -> CoreResult<()>) -> Result<Profile> {
        let local = Store::open(&self.home)?;
        let old = local.get(key)?.clone();
        let mut new = old.clone();
        apply(&local, &mut new)?;
        let mut patch = diff_patch(&old, &new)?;
        if patch.is_empty() {
            return Ok(old);
        }
        if patch.contains_key("group_id") {
            patch.insert("group_id".into(), json!(self.hub_group_id(&local.group_name(&new.group_id))?));
        }
        let hp = self.client.patch_profile(&old.profile_id, &Value::Object(patch), None)?;
        let groups = self.group_names()?;
        let mut synced = hp.profile;
        store::locked(&self.home, |s| {
            synced.group_id = match groups.get(&synced.group_id).filter(|n| !n.trim().is_empty()) {
                Some(group) => s.group_id_for(group)?,
                None => "0".into(),
            };
            keep_unmasked(Some(&old), &mut synced);
            s.upsert_synced(synced)
        })
        .map_err(Error::from)
    }

    /// Delete a hub profile for everybody (and its local copy).
    pub fn delete(&self, key: &str) -> Result<Profile> {
        let p = store::Store::open(&self.home)?.get(key)?.clone();
        if launch::running(&self.home, &p.profile_id).is_some() {
            return Err(Error::Invalid(format!("{} is open: close it first", p.profile_id)));
        }
        self.client.delete_profile(&p.profile_id)?;
        store::locked(&self.home, |s| s.delete(std::slice::from_ref(&p.profile_id)))?;
        launch::purge(&self.home, &p.profile_id)?;
        Bound::remove(&self.home, &p.profile_id)?;
        Ok(p)
    }
}

/// Detach a profile from the hub: it stays on this device, with its data, as an ordinary local profile. It is not
/// the hub's copy any more (nothing is uploaded or deleted there).
pub fn unbind(home: &Home, key: &str) -> Result<Profile> {
    let p = store::Store::open(home)?.get(key)?.clone();
    if launch::running(home, &p.profile_id).is_some() || session::is_syncing(&p.profile_id) {
        return Err(Error::Invalid(format!("{} is open: close it first", p.profile_id)));
    }
    if Bound::remove(home, &p.profile_id)? {
        Ok(p)
    } else {
        Err(Error::Invalid(format!("{} is not a hub profile", p.profile_id)))
    }
}

/// Forget the hub login. Hub profiles stay on the device, bound (they cannot be opened until you log in again or
/// unbind them).
pub fn log_out(home: &Home) -> Result<bool> {
    if let Some(id) = state::bound_ids(home).into_iter().find(|id| launch::running(home, id).is_some()) {
        return Err(Error::Invalid(format!("{id} is open: close it first")));
    }
    state::remove_login(home)
}

/// Change a profile wherever it lives: a local one is edited in the store, a hub one is written through to the hub.
/// `apply` works on a copy, so a bad value leaves the stored profile untouched.
pub fn edit(home: &Home, key: &str, apply: impl FnOnce(&Store, &mut Profile) -> CoreResult<()>) -> Result<Profile> {
    let id = Store::open(home)?.get(key)?.profile_id.clone();
    if home.hub_bound(&id) {
        return Hub::connect(home)?.ok_or(Error::NotLoggedIn)?.edit(&id, apply);
    }
    Ok(store::locked(home, |s| {
        let mut staged = s.get(&id)?.clone();
        apply(s, &mut staged)?;
        s.update(&id, |p| *p = staged)
    })?)
}

/// Delete a profile and its browser data. A hub profile is the whole team's: it goes only with `everywhere`, and
/// then on the hub for everybody ([`Error::TeamProfile`] otherwise).
pub fn delete(home: &Home, key: &str, everywhere: bool) -> Result<Profile> {
    let id = Store::open(home)?.get(key)?.profile_id.clone();
    if home.hub_bound(&id) {
        if !everywhere {
            return Err(Error::TeamProfile(id));
        }
        return Hub::connect(home)?.ok_or(Error::NotLoggedIn)?.delete(&id);
    }
    let gone = store::locked(home, |s| s.delete(std::slice::from_ref(&id)))?;
    let p = gone.into_iter().next().ok_or_else(|| Error::Invalid(format!("no such profile: {key}")))?;
    launch::purge(home, &p.profile_id)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use citro_core::profile::ProxyConfig;

    #[test]
    fn a_merge_patch_turns_the_old_value_into_the_new_one_and_nothing_else() {
        let old = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1, 2], "gone": "x"});
        let new = json!({"a": 1, "b": {"c": 9, "d": 3, "x": 1}, "e": [3], "n": "new"});
        let patch = merge_diff(&old, &new).unwrap();
        assert_eq!(patch, json!({"b": {"c": 9, "x": 1}, "e": [3], "n": "new", "gone": null}));
        assert_eq!(merge_diff(&old, &old), None);
        // applying it (RFC 7386) gives the new value back
        let mut applied = old.clone();
        merge(&mut applied, &patch);
        assert_eq!(applied, new);
    }

    fn merge(target: &mut Value, patch: &Value) {
        match patch {
            Value::Object(p) => {
                if !target.is_object() {
                    *target = json!({});
                }
                for (k, v) in p {
                    if v.is_null() {
                        target.as_object_mut().unwrap().remove(k);
                    } else {
                        merge(target.as_object_mut().unwrap().entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
            other => *target = other.clone(),
        }
    }

    #[test]
    fn replacing_a_proxy_removes_what_the_old_one_had_and_the_new_one_does_not() {
        let old = Profile {
            profile_id: "kabc123".into(),
            user_proxy_config: ProxyConfig::from_url("http://u:pw@10.0.0.1:8080").unwrap(),
            ..Default::default()
        };
        let mut new = old.clone();
        new.user_proxy_config = ProxyConfig::from_url("socks5://9.9.9.9:1080").unwrap();
        new.name = "renamed".into();
        new.serial_number = 99; // the hub's, never sent
        let patch = diff_patch(&old, &new).unwrap();
        assert!(!patch.contains_key("serial_number") && !patch.contains_key("profile_id"));
        assert_eq!(patch["name"], "renamed");
        let mut hub_side = serde_json::to_value(&old).unwrap();
        merge(&mut hub_side, &Value::Object(patch));
        let after: Profile = serde_json::from_value(hub_side).unwrap();
        assert_eq!(after.user_proxy_config, new.user_proxy_config, "no stale user/password left behind");
        assert_eq!(after.name, "renamed");
        assert_eq!(after.serial_number, old.serial_number);
    }

    #[test]
    fn a_masked_secret_is_replaced_by_the_one_this_device_already_had() {
        let local = Profile { password: "hunter2".into(), cookie: "c=1".into(), ..Default::default() };
        let mut from_hub =
            Profile { password: MASK.into(), cookie: MASK.into(), name: "n".into(), ..Default::default() };
        keep_unmasked(Some(&local), &mut from_hub);
        assert_eq!((from_hub.password.as_str(), from_hub.cookie.as_str()), ("hunter2", "c=1"));
        assert_eq!(from_hub.name, "n");
        let mut fresh = Profile { password: MASK.into(), ..Default::default() };
        keep_unmasked(None, &mut fresh);
        assert_eq!(fresh.password, MASK, "nothing to restore: it stays masked, it is never invented");
    }
}
