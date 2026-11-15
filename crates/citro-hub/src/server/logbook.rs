//! Validation and stamping of client-posted log entries (`POST /v1/logs`).

use super::error::{ApiError, ApiResult};
use super::state::State;
use super::util::{clean_text, scrub_secrets};
use crate::model::{LogEntry, LogRecord, User};

pub const MAX_ENTRIES: usize = 500;
pub const MAX_MESSAGE_CHARS: usize = 4000;

impl State {
    /// Check a batch and turn it into `(team_id, record)` pairs: the user is the caller, the team comes from the
    /// profile (or the explicit `team_id`, or the caller's only team). The batch is all-or-nothing.
    pub fn stamp_log_entries(
        &self,
        user: &User,
        device_id: &str,
        entries: &[LogEntry],
        now: i64,
    ) -> ApiResult<Vec<(String, LogRecord)>> {
        if entries.len() > MAX_ENTRIES {
            return Err(ApiError::too_large(format!("at most {MAX_ENTRIES} log entries per request")));
        }
        let device_id =
            clean_text(device_id, 128).ok_or_else(|| ApiError::invalid("device_id must be 1-128 characters"))?;
        let only_team = {
            let teams = self.teams_of(user);
            (teams.len() == 1).then(|| teams[0].team.id.clone())
        };
        let mut out = Vec::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            let team_id = match (&e.profile_id, &e.team_id) {
                (Some(pid), explicit) => {
                    let hp = self
                        .profiles
                        .get(pid)
                        .filter(|hp| self.role_in(user, &hp.team_id).is_some())
                        .ok_or_else(|| ApiError::invalid(format!("entry {i}: unknown profile `{pid}`")))?;
                    if explicit.as_deref().is_some_and(|t| t != hp.team_id) {
                        return Err(ApiError::invalid(format!("entry {i}: team_id does not match the profile")));
                    }
                    hp.team_id.clone()
                }
                (None, Some(t)) => {
                    if self.role_in(user, t).is_none() {
                        return Err(ApiError::invalid(format!("entry {i}: unknown team `{t}`")));
                    }
                    t.clone()
                }
                (None, None) => only_team
                    .clone()
                    .ok_or_else(|| ApiError::invalid(format!("entry {i}: needs a profile_id or a team_id")))?,
            };
            let mut message = scrub_secrets(&e.message);
            if message.chars().count() > MAX_MESSAGE_CHARS {
                message = message.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "…";
            }
            let ts = if e.ts <= 0 || e.ts > now + 86_400 { now } else { e.ts };
            out.push((
                team_id.clone(),
                LogRecord {
                    ts,
                    kind: e.kind,
                    level: e.level,
                    team_id,
                    user: user.name.clone(),
                    device_id: device_id.clone(),
                    profile_id: e.profile_id.clone(),
                    message,
                },
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::testkit::{cx, world};
    use super::*;
    use crate::model::{LogKind, LogLevel};
    use citro_core::profile::Profile;

    fn entry(pid: Option<&str>, team: Option<&str>, msg: &str) -> LogEntry {
        LogEntry {
            ts: 500,
            kind: LogKind::Browser,
            level: LogLevel::Warn,
            profile_id: pid.map(str::to_owned),
            team_id: team.map(str::to_owned),
            message: msg.into(),
        }
    }

    #[test]
    fn entries_are_stamped_with_the_user_and_the_profiles_team() {
        let (mut st, _admin, ann, bob, tid) = world();
        let pid = st.create_profile(&mut cx(&ann), &tid, Profile { name: "a".into(), ..Default::default() }).unwrap().profile.profile_id;
        let out = st.stamp_log_entries(&ann.user, "dev1", &[entry(Some(&pid), None, "crash")], 1000).unwrap();
        assert_eq!(out.len(), 1);
        let (team, rec) = &out[0];
        assert_eq!(team, &tid);
        assert_eq!((rec.user.as_str(), rec.device_id.as_str(), rec.ts), ("ann", "dev1", 500));
        // an unknown profile and a profile of a foreign team are both just "unknown"
        for who in [&ann, &bob] {
            let e = st.stamp_log_entries(&who.user, "d", &[entry(Some("kzzzzzz"), None, "x")], 1000).unwrap_err();
            assert_eq!(e.code, "invalid");
        }
        assert_eq!(st.stamp_log_entries(&bob.user, "d", &[entry(Some(&pid), None, "x")], 1000).unwrap_err().code, "invalid");
        // explicit team, implicit team (single membership), no team at all
        assert_eq!(st.stamp_log_entries(&ann.user, "d", &[entry(None, Some(&tid), "x")], 1000).unwrap()[0].0, tid);
        assert_eq!(st.stamp_log_entries(&ann.user, "d", &[entry(None, None, "x")], 1000).unwrap()[0].0, tid);
        assert_eq!(st.stamp_log_entries(&bob.user, "d", &[entry(None, None, "x")], 1000).unwrap_err().code, "invalid");
        assert_eq!(st.stamp_log_entries(&bob.user, "d", &[entry(None, Some(&tid), "x")], 1000).unwrap_err().code, "invalid");
        // a mismatching explicit team
        let e = st.stamp_log_entries(&ann.user, "d", &[entry(Some(&pid), Some("tother"), "x")], 1000).unwrap_err();
        assert_eq!(e.code, "invalid");
    }

    #[test]
    fn limits_scrubbing_and_clock_sanity() {
        let (st, _admin, ann, _bob, tid) = world();
        let many: Vec<LogEntry> = (0..=MAX_ENTRIES).map(|_| entry(None, Some(&tid), "x")).collect();
        assert_eq!(st.stamp_log_entries(&ann.user, "d", &many, 1000).unwrap_err().code, "too_large");
        assert_eq!(st.stamp_log_entries(&ann.user, "d", &many[..MAX_ENTRIES], 1000).unwrap().len(), MAX_ENTRIES);
        assert_eq!(st.stamp_log_entries(&ann.user, "", &many[..1], 1000).unwrap_err().code, "invalid");
        let token = format!("citro_{}", "ab".repeat(32));
        let long = "é".repeat(MAX_MESSAGE_CHARS + 10);
        let mut skewed = entry(None, Some(&tid), &format!("leaked {token}"));
        skewed.ts = 10_000_000;
        let mut zero = entry(None, Some(&tid), &long);
        zero.ts = 0;
        let out = st.stamp_log_entries(&ann.user, "d", &[skewed, zero], 1000).unwrap();
        assert!(!out[0].1.message.contains(&token[6..]) && out[0].1.message.contains("citro_****"));
        assert_eq!(out[0].1.ts, 1000, "a timestamp from the far future is replaced");
        assert_eq!(out[1].1.ts, 1000, "a missing timestamp too");
        assert_eq!(out[1].1.message.chars().count(), MAX_MESSAGE_CHARS + 1);
    }
}
