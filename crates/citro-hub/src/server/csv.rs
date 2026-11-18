//! CSV export of a team's profiles in the format every spreadsheet reads (RFC 4180, UTF-8, CRLF).
//!
//! The format lives once, in [`citro_core::csv`]: the command line (`citro export`), the app and this endpoint write
//! the same 24 columns. No secrets ever reach the file: only named, non-secret columns are written, URLs inside tabs
//! have their credentials masked, and every text cell is neutralised against spreadsheet formula injection.

use super::error::{ApiError, ApiResult};
use super::state::State;
use crate::model::{TeamRole, UNGROUPED, User};
use citro_core::csv::{self, CsvRow};

/// Render the team's profiles (optionally one group) as CSV bytes.
pub fn export_profiles(
    st: &State,
    user: &User,
    team_id: &str,
    group: Option<&str>,
    bom: bool,
    now_ms: i64,
) -> ApiResult<Vec<u8>> {
    // The file holds nothing a viewer cannot already see in the profile list.
    st.require_role(user, team_id, TeamRole::Viewer)?;
    let group_id = match group.map(str::trim).filter(|g| !g.is_empty()) {
        None => None,
        Some(g) => Some(st.resolve_group(team_id, g).ok_or_else(|| ApiError::invalid(format!("unknown group `{g}`")))?),
    };
    let mut profiles: Vec<_> = st
        .profiles
        .values()
        .filter(|p| p.team_id == team_id && group_id.as_deref().is_none_or(|g| p.profile.group_id == g))
        .filter(|p| st.may_see_profile(user, p))
        .collect();
    profiles.sort_by_key(|p| p.profile.serial_number);

    let rows: Vec<CsvRow> = profiles
        .into_iter()
        .map(|hp| {
            let p = &hp.profile;
            let lock = st.effective_lock(&p.profile_id, now_ms);
            let group_name = if p.group_id == UNGROUPED || p.group_id.is_empty() {
                String::new()
            } else {
                st.group_name(&p.group_id)
            };
            let mut row = CsvRow::from_profile(p, &group_name, lock.is_some());
            row.created_by = hp.created_by.clone();
            row.last_open_by = hp.last_open_by.clone();
            row.last_close_by = hp.last_close_by.clone();
            row.locked_by = lock.map(|l| l.user.clone()).unwrap_or_default();
            row.locked_device = lock.map(|l| l.device_name.clone()).unwrap_or_default();
            row
        })
        .collect();
    Ok(csv::write(&rows, bom))
}

#[cfg(test)]
mod tests {
    use super::super::state::testkit::{cx, world};
    use super::*;
    use citro_core::csv::UTF8_BOM;
    use citro_core::profile::Profile;
    use serde_json::json;

    #[test]
    fn export_has_no_secrets_and_the_documented_columns() {
        let (mut st, admin, ann, bob, tid) = world();
        let p: Profile = serde_json::from_value(json!({
            "name": "Shop, \"main\"\nline2", "remark": "=HYPERLINK(\"x\")", "username": "bob@example.com",
            "password": "pw-secret", "fakey": "2FA-SECRET", "cookie": "COOKIE-SECRET",
            "tabs": ["https://a.example/", "https://u:tabsecret@b.example/x"], "country": "DE", "persona_os": "windows",
            "user_proxy_config": {"proxy_type": "socks5", "proxy_host": "1.2.3.4", "proxy_port": 1080,
                "proxy_user": "pu", "proxy_password": "px-secret"},
            "fingerprint_config": {"language": ["de-DE", "en-US"], "timezone": "Europe/Berlin"}
        }))
        .unwrap();
        let pid = st.create_profile(&mut cx(&ann), &tid, p).unwrap().profile.profile_id;
        let l = st.lock_profile(&mut cx(&ann), &pid, "d1", "Büro-PC").unwrap();
        let bytes = export_profiles(&st, &admin.user, &tid, None, false, 1_000_001).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for secret in ["pw-secret", "2FA-SECRET", "COOKIE-SECRET", "px-secret", "tabsecret", &l.lease] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        assert!(text.starts_with("no,id,name,"));
        assert!(text.contains("\"Shop, \"\"main\"\"\nline2\""));
        assert!(text.contains("\"'=HYPERLINK(\"\"x\"\")\""));
        assert!(text.contains(",\"de-DE,en-US\",Europe/Berlin,"));
        assert!(text.contains(",open,ann,Büro-PC"));
        assert!(text.contains("https://a.example/ | https://u:***@b.example/x"));
        assert!(text.ends_with("\r\n"));
        // outsiders cannot export
        assert_eq!(export_profiles(&st, &bob.user, &tid, None, false, 0).unwrap_err().code, "not_found");
        // the BOM is optional
        let with_bom = export_profiles(&st, &admin.user, &tid, None, true, 0).unwrap();
        assert!(with_bom.starts_with(UTF8_BOM));
        assert!(!export_profiles(&st, &admin.user, &tid, None, false, 0).unwrap().starts_with(UTF8_BOM));
        // an unknown group is an error, a known one filters
        assert_eq!(export_profiles(&st, &admin.user, &tid, Some("ghost"), false, 0).unwrap_err().code, "invalid");
        let ungrouped = export_profiles(&st, &admin.user, &tid, Some("0"), false, 0).unwrap();
        assert_eq!(String::from_utf8(ungrouped).unwrap().matches("\r\n").count(), 2, "header + the ungrouped profile");
        let g = st.create_group(&mut cx(&ann), &tid, "Empty", "").unwrap();
        let empty = export_profiles(&st, &admin.user, &tid, Some(&g.name), false, 0).unwrap();
        assert_eq!(String::from_utf8(empty).unwrap().matches("\r\n").count(), 1, "header only");
    }
}
