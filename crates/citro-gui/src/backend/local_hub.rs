//! The team side of [`LocalBackend`].
//!
//! Everything that touches a hub goes through `citro-sync`, the layer the command line and the Local API use too, so
//! the app obeys the same rules: a team profile is opened only with the hub's lock, closing it uploads its cookies
//! and data and hands the lock back, edits are written through to the hub.
//!
//! The profile table must never wait for the network. Who has what open comes from a cache that a background
//! thread keeps fresh while a hub is connected, and the team's profiles are pulled the same way.

use super::local::LocalBackend;
use super::{BResult, BackendError, HubStatus, Level, Member, Notice, OpenError, Role, RunState, TeamInfo};
use crate::eventlog::EventLog;
use citro_core::store::Home;
use citro_hub::model::{ProfileFilter, TeamRole};
use citro_sync::{Hub, LockRow, Outcome, PullReport};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How old the table of locks may be before a background refresh starts.
const LOCKS_EVERY: Duration = Duration::from_secs(4);
/// How often the team's profiles are pulled while the app is open.
const PULL_EVERY: Duration = Duration::from_secs(60);
/// Closing a team profile waits this long for the upload and for the lock to be handed back.
pub(super) const CLOSE_PATIENCE: Duration = Duration::from_secs(180);
/// Quitting waits this long per profile.
pub(super) const QUIT_PATIENCE: Duration = Duration::from_secs(180);

#[derive(Default)]
pub(super) struct HubState {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    locks: Mutex<Vec<LockRow>>,
    /// When the last background refresh started (`None` = due now).
    refreshed: Mutex<Option<Instant>>,
    pulled: Mutex<Option<Instant>>,
    working: AtomicBool,
    adopted: AtomicBool,
    error: Mutex<Option<String>>,
    notices: Mutex<Vec<Notice>>,
}

fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl HubState {
    /// The lock somebody holds on a profile, as of the last refresh.
    pub(super) fn lock_of(&self, id: &str) -> Option<LockRow> {
        held(&self.shared.locks).iter().find(|l| l.profile_id == id).cloned()
    }

    /// The next list refreshes the locks (something just changed them).
    pub(super) fn stale(&self) {
        *held(&self.shared.refreshed) = None;
    }

    /// Back to square one (a new login, a logout).
    fn reset(&self) {
        held(&self.shared.locks).clear();
        *held(&self.shared.refreshed) = None;
        *held(&self.shared.pulled) = None;
        *held(&self.shared.error) = None;
        self.shared.adopted.store(false, Ordering::SeqCst);
    }

    pub(super) fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *held(&self.shared.notices))
    }

    fn notify(&self, level: Level, text: String) {
        let mut n = held(&self.shared.notices);
        n.push(Notice { level, text });
        if n.len() > 20 {
            n.remove(0);
        }
    }

    fn error(&self) -> Option<String> {
        held(&self.shared.error).clone()
    }

    fn set_error(&self, e: Option<String>) {
        *held(&self.shared.error) = e;
    }
}

/// The status column of a team profile. The browser running here wins; then "the browser is gone and its data is
/// still going up"; then somebody else's lock.
pub(super) fn run_state(running: Option<(u32, i64)>, syncing: bool, lock: Option<&LockRow>) -> RunState {
    if let Some((pid, since)) = running {
        return RunState::Open { pid, since };
    }
    if syncing {
        return RunState::Closing;
    }
    match lock {
        Some(l) if !l.mine => {
            RunState::LockedElsewhere { user: l.user.clone(), device: l.device.clone(), since: l.since }
        }
        _ => RunState::Closed,
    }
}

/// An open that the hub refused because somebody else has the profile.
pub(super) fn open_error(e: citro_sync::Error) -> OpenError {
    match e.locked_by() {
        Some(l) => OpenError::LockedElsewhere { user: l.user, device: l.device, since: l.since },
        None => OpenError::Other(BackendError::from(e).to_string()),
    }
}

fn role(r: TeamRole) -> Role {
    match r {
        TeamRole::Owner => Role::Owner,
        TeamRole::Admin => Role::Admin,
        TeamRole::Member => Role::Member,
        TeamRole::Viewer => Role::Viewer,
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// One sentence about what a pull did.
pub(super) fn pull_sentence(r: &PullReport) -> String {
    let mut parts = Vec::new();
    if !r.added.is_empty() {
        parts.push(format!("{} new", r.added.len()));
    }
    if !r.updated.is_empty() {
        parts.push(format!("{} updated", r.updated.len()));
    }
    if !r.gone.is_empty() {
        let names = r.gone.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ");
        parts.push(format!("{} gone from the hub ({names}; they stay here until you delete them)", r.gone.len()));
    }
    if parts.is_empty() { "the team's profiles are up to date".to_owned() } else { parts.join(", ") }
}

/// What a finished hub session means for the user: one line in the log, and a notice for the app.
fn report(home: &Home, state: &HubState, events: &EventLog, o: &Outcome, notify: bool) {
    let name = citro_core::store::Store::open(home)
        .ok()
        .and_then(|s| s.find(&o.profile_id).map(|p| p.name.clone()))
        .unwrap_or_else(|| o.profile_id.clone());
    let level = if o.is_clean() {
        Level::Info
    } else if o.error.is_some() || o.lost.is_some() || !o.released {
        Level::Error
    } else {
        Level::Warn
    };
    let line = format!("{name}: {}", o.summary());
    events.push(level, Some(&o.profile_id), line.clone());
    if notify {
        state.notify(level, line);
    }
}

fn refresh(home: &Home, state: &HubState, events: &EventLog, pull: bool) {
    let hub = match Hub::connect(home) {
        Ok(Some(h)) => h,
        Ok(None) => return,
        Err(e) => {
            state.set_error(Some(e.to_string()));
            return;
        }
    };
    if !state.shared.adopted.swap(true, Ordering::SeqCst) {
        for o in hub.adopt() {
            report(home, state, events, &o, true);
        }
    }
    if pull {
        match hub.pull() {
            Ok(r) => {
                *held(&state.shared.pulled) = Some(Instant::now());
                if !(r.added.is_empty() && r.updated.is_empty() && r.gone.is_empty()) {
                    events.info(None, format!("team: {}", pull_sentence(&r)));
                }
            }
            Err(e) => {
                state.set_error(Some(e.to_string()));
                return;
            }
        }
    }
    match hub.locks() {
        Ok(rows) => {
            *held(&state.shared.locks) = rows;
            state.set_error(None);
        }
        Err(e) => state.set_error(Some(e.to_string())),
    }
}

impl LocalBackend {
    /// Starts a background refresh when the lock table (and now and then the pull) is due. Never blocks.
    pub(super) fn keep_hub_fresh(&self) {
        if !matches!(citro_sync::load_login(self.home()), Ok(Some(_))) {
            return;
        }
        let state = &self.hub;
        if held(&state.shared.refreshed).is_some_and(|t| t.elapsed() < LOCKS_EVERY) {
            return;
        }
        if state.shared.working.swap(true, Ordering::SeqCst) {
            return;
        }
        let pull = held(&state.shared.pulled).is_none_or(|t| t.elapsed() >= PULL_EVERY);
        *held(&state.shared.refreshed) = Some(Instant::now());
        let (home, shared, events) = (self.home().clone(), state.shared.clone(), self.events().clone());
        let spawned = std::thread::Builder::new().name("citro-hub-refresh".into()).spawn(move || {
            let state = HubState { shared };
            refresh(&home, &state, &events, pull);
            state.shared.working.store(false, Ordering::SeqCst);
        });
        if spawned.is_err() {
            state.shared.working.store(false, Ordering::SeqCst);
        }
    }

    /// The hub profiles of this device (the bound ones) and the team each belongs to.
    pub(super) fn bound_teams(&self) -> Vec<(String, String)> {
        citro_sync::bound_ids(self.home())
            .into_iter()
            .filter_map(|id| citro_sync::Bound::load(self.home(), &id).map(|b| (id, b.team_id)))
            .collect()
    }

    pub(super) fn is_bound(&self, id: &str) -> bool {
        self.home().hub_bound(id)
    }

    /// The names of those of these profiles that belong to a hub.
    pub(super) fn bound_names(&self, ids: &[String]) -> Vec<String> {
        let want: HashSet<&String> = ids.iter().collect();
        self.bound_teams()
            .into_iter()
            .filter(|(id, _)| want.contains(id))
            .map(|(id, _)| self.profile_name(&id))
            .collect()
    }

    /// A finished hub session that nobody waited for (the browser was closed by its window): log it, tell the app.
    pub(super) fn report_outcome(&self, o: &Outcome, notify: bool) {
        report(self.home(), &self.hub, self.events(), o, notify);
    }

    pub(super) fn hub_info(&self) -> HubStatus {
        match citro_sync::load_login(self.home()) {
            Ok(Some(l)) => HubStatus {
                connected: true,
                url: l.url,
                user: l.user,
                team: l.team_name,
                device_name: citro_sync::device(self.home()).map(|d| d.name).unwrap_or_default(),
                error: self.hub.error(),
            },
            Ok(None) => HubStatus::default(),
            Err(e) => HubStatus { error: Some(e.to_string()), ..HubStatus::default() },
        }
    }

    pub(super) fn hub_connect(&self, url: &str, token: &str, device: &str, team: &str) -> BResult<HubStatus> {
        let (device, team) = (device.trim(), team.trim());
        let hub = Hub::log_in(
            self.home(),
            url.trim(),
            token,
            (!device.is_empty()).then_some(device),
            (!team.is_empty()).then_some(team),
        )?;
        self.hub.reset();
        let login = hub.login();
        self.events().info(None, format!("connected to {} as {} (team {})", login.url, login.user, login.team_name));
        match hub.pull() {
            Ok(r) => {
                *held(&self.hub.shared.pulled) = Some(Instant::now());
                self.events().info(None, format!("team: {}", pull_sentence(&r)));
            }
            Err(e) => self.events().warn(None, format!("connected, but the first pull failed: {e}")),
        }
        Ok(self.hub_info())
    }

    pub(super) fn hub_disconnect(&self) -> BResult<HubStatus> {
        let was = citro_sync::log_out(self.home())?;
        self.hub.reset();
        if was {
            self.events()
                .info(None, "disconnected from the team hub (team profiles stay here, locked until you connect again)");
        }
        Ok(self.hub_info())
    }

    pub(super) fn hub_teams(&self) -> BResult<Vec<TeamInfo>> {
        let Some(hub) = Hub::connect(self.home())? else { return Ok(Vec::new()) };
        let team_id = hub.team_id().to_owned();
        let mine = hub.client().teams()?.into_iter().find(|t| t.team.id == team_id);
        let Some(entry) = mine else {
            return Err(BackendError::Invalid(format!(
                "you are not in the team `{}` any more: connect again on the Teams page",
                hub.login().team_name
            )));
        };
        // the member list is for owners and admins on some hubs: a plain member just sees no list
        let members = hub
            .client()
            .members(&team_id)
            .unwrap_or_default()
            .into_iter()
            .map(|m| Member { name: m.user_name, role: role(m.role) })
            .collect();
        let profile_count = hub
            .client()
            .profiles(&team_id, &ProfileFilter { limit: Some(1), ..ProfileFilter::default() })
            .map(|p| p.total)
            .unwrap_or(0);
        Ok(vec![TeamInfo { id: team_id, name: entry.team.name, role: role(entry.role), members, profile_count }])
    }

    pub(super) fn hub_force_unlock(&self, id: &str) -> BResult<()> {
        let hub = Hub::connect(self.home())?.ok_or(citro_sync::Error::NotLoggedIn)?;
        let name = self.profile_name(id);
        let had = hub.force_unlock(id)?;
        self.hub.stale();
        if had {
            self.events().warn(Some(id), format!("force-unlocked {name}"));
            Ok(())
        } else {
            Err(BackendError::NotFound(format!("nobody has {name} open")))
        }
    }

    pub(super) fn hub_pull_now(&self) -> BResult<String> {
        let hub = Hub::connect(self.home())?.ok_or(citro_sync::Error::NotLoggedIn)?;
        let r = hub.pull()?;
        *held(&self.hub.shared.pulled) = Some(Instant::now());
        self.hub.stale();
        let sentence = pull_sentence(&r);
        self.events().info(None, format!("team: {sentence}"));
        Ok(sentence)
    }

    pub(super) fn hub_share(&self, ids: &[String]) -> BResult<String> {
        let hub = Hub::connect(self.home())?.ok_or(citro_sync::Error::NotLoggedIn)?;
        let mut done = 0usize;
        for id in ids {
            let name = self.profile_name(id);
            match hub.push(id) {
                Ok(r) => {
                    done += 1;
                    self.events().info(
                        Some(&r.profile.profile_id),
                        format!("shared {name} with the team as {}: {}", r.profile.profile_id, r.outcome.summary()),
                    );
                }
                Err(e) => {
                    let msg = BackendError::from(e).to_string();
                    self.events().error(Some(id), format!("could not share {name}: {msg}"));
                    let first = if done > 0 {
                        format!("shared {} first, then ", count(done, "profile", "profiles"))
                    } else {
                        String::new()
                    };
                    return Err(BackendError::Other(format!("{first}{name}: {msg}")));
                }
            }
        }
        self.hub.stale();
        Ok(format!("Shared {} with {}", count(done, "profile", "profiles"), hub.login().team_name))
    }

    pub(super) fn quit(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for o in citro_sync::shutdown(QUIT_PATIENCE) {
            self.report_outcome(&o, false);
            lines.push(format!("{}: {}", self.profile_name(&o.profile_id), o.summary()));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use citro_sync::Entry;

    fn lock(mine: bool) -> LockRow {
        LockRow {
            profile_id: "k1".into(),
            name: "Shop".into(),
            user: "ben".into(),
            device: "Ben's PC".into(),
            since: 100,
            expires_at: 200,
            mine,
        }
    }

    #[test]
    fn the_status_column_of_a_team_profile() {
        let other = lock(false);
        assert_eq!(
            run_state(Some((7, 5)), true, Some(&other)),
            RunState::Open { pid: 7, since: 5 },
            "the browser here wins"
        );
        assert_eq!(run_state(None, true, Some(&other)), RunState::Closing, "its data is still going up");
        assert_eq!(
            run_state(None, false, Some(&other)),
            RunState::LockedElsewhere { user: "ben".into(), device: "Ben's PC".into(), since: 100 }
        );
        assert_eq!(run_state(None, false, Some(&lock(true))), RunState::Closed, "my own stale lock is not 'elsewhere'");
        assert_eq!(run_state(None, false, None), RunState::Closed);
    }

    #[test]
    fn a_refused_open_names_who_has_the_profile() {
        let e = citro_sync::Error::Invalid("x".into());
        assert_eq!(open_error(e), OpenError::Other("x".into()));
        let e = citro_sync::Error::NotLoggedIn;
        assert!(matches!(open_error(e), OpenError::Other(m) if m.contains("Teams page")));
    }

    #[test]
    fn the_pull_sentence() {
        let mut r = PullReport::default();
        assert_eq!(pull_sentence(&r), "the team's profiles are up to date");
        r.added.push(Entry { id: "a".into(), name: "A".into() });
        r.updated.push(Entry { id: "b".into(), name: "B".into() });
        r.gone.push(Entry { id: "c".into(), name: "C".into() });
        let s = pull_sentence(&r);
        assert!(s.starts_with("1 new, 1 updated, 1 gone from the hub (C;"), "{s}");
    }
}
