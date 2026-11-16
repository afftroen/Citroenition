//! CSV export of a team's profiles in a format every spreadsheet reads (RFC 4180, UTF-8, CRLF).
//!
//! No secrets ever reach this file: only named, non-secret columns are written, URLs inside tabs have their
//! credentials masked, and every text cell is neutralised against spreadsheet formula injection.

use super::error::{ApiError, ApiResult};
use super::redact::mask_url_credentials;
use super::state::State;
use super::util::iso_utc;
use crate::model::{TeamRole, UNGROUPED, User};
use serde_json::Value;

/// The header row, exactly as promised in the API contract.
pub const HEADER: [&str; 24] = [
    "no",
    "id",
    "name",
    "group",
    "remark",
    "persona_os",
    "country",
    "proxy_type",
    "proxy_host",
    "proxy_port",
    "proxy_user",
    "language",
    "timezone",
    "tabs",
    "account_username",
    "created_at",
    "created_by",
    "last_open_at",
    "last_open_by",
    "last_close_at",
    "last_close_by",
    "status",
    "locked_by",
    "locked_device",
];

pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Prefix a single quote when a text cell would start a formula in a spreadsheet (`= + - @`, TAB, CR).
pub fn neutralize(s: &str) -> String {
    match s.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{s}"),
        _ => s.to_owned(),
    }
}

/// RFC 4180 field: quoted when it holds a comma, a quote, CR or LF; quotes are doubled.
pub fn quote(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_owned() }
}

/// A text cell: formula-safe, then CSV-quoted.
pub fn text_cell(s: &str) -> String {
    quote(&neutralize(s))
}

/// One CRLF-terminated row of already-final cells.
pub fn row(cells: &[String]) -> String {
    let mut line = cells.join(",");
    line.push_str("\r\n");
    line
}

/// `fingerprint_config.language` as a comma list (it is an array of tags, or a single string).
fn language_of(cfg: &serde_json::Map<String, Value>) -> String {
    match cfg.get("language") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn timezone_of(cfg: &serde_json::Map<String, Value>) -> String {
    cfg.get("timezone").and_then(Value::as_str).unwrap_or_default().to_owned()
}

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
        .collect();
    profiles.sort_by_key(|p| p.profile.serial_number);

    let mut out: Vec<u8> = Vec::new();
    if bom {
        out.extend_from_slice(UTF8_BOM);
    }
    out.extend_from_slice(row(&HEADER.map(str::to_owned)).as_bytes());
    for hp in profiles {
        let p = &hp.profile;
        let lock = st.effective_lock(&p.profile_id, now_ms);
        let group_name =
            if p.group_id == UNGROUPED || p.group_id.is_empty() { String::new() } else { st.group_name(&p.group_id) };
        let proxy = &p.user_proxy_config;
        let tabs = p.tabs.iter().map(|t| mask_url_credentials(t)).collect::<Vec<_>>().join(" | ");
        let cells = [
            p.serial_number.to_string(),
            text_cell(&p.profile_id),
            text_cell(&p.name),
            text_cell(&group_name),
            text_cell(&p.remark),
            text_cell(&p.persona_os),
            text_cell(&p.country),
            text_cell(proxy.proxy_type.as_deref().unwrap_or_default()),
            text_cell(proxy.proxy_host.as_deref().unwrap_or_default()),
            text_cell(proxy.proxy_port.as_deref().unwrap_or_default()),
            text_cell(proxy.proxy_user.as_deref().unwrap_or_default()),
            text_cell(&language_of(&p.fingerprint_config.0)),
            text_cell(&timezone_of(&p.fingerprint_config.0)),
            text_cell(&tabs),
            text_cell(&p.username),
            iso_utc(p.created_time),
            text_cell(&hp.created_by),
            iso_utc(p.last_open_time),
            text_cell(&hp.last_open_by),
            iso_utc(p.last_close_time),
            text_cell(&hp.last_close_by),
            if lock.is_some() { "open" } else { "closed" }.to_owned(),
            text_cell(lock.map(|l| l.user.as_str()).unwrap_or_default()),
            text_cell(lock.map(|l| l.device_name.as_str()).unwrap_or_default()),
        ];
        out.extend_from_slice(row(&cells).as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::state::testkit::{cx, world};
    use super::*;
    use citro_core::profile::Profile;
    use serde_json::json;

    #[test]
    fn quoting_follows_rfc_4180() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a,b"), "\"a,b\"");
        assert_eq!(quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(quote("two\nlines"), "\"two\nlines\"");
        assert_eq!(quote("cr\rhere"), "\"cr\rhere\"");
        assert_eq!(quote("ünï ✓"), "ünï ✓");
    }

    #[test]
    fn formulas_are_neutralised() {
        for bad in ["=1+1", "+1", "-2", "@SUM(A1)", "\tx", "\rx"] {
            assert!(neutralize(bad).starts_with('\''), "{bad:?}");
        }
        for ok in ["a=b", "1+1", " =x", "", "'quoted"] {
            assert_eq!(neutralize(ok), ok);
        }
        assert_eq!(text_cell("=cmd|' /C calc'!A0"), "'=cmd|' /C calc'!A0");
        assert_eq!(text_cell("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn header_is_the_contract() {
        assert_eq!(
            HEADER.join(","),
            "no,id,name,group,remark,persona_os,country,proxy_type,proxy_host,proxy_port,proxy_user,language,timezone,\
             tabs,account_username,created_at,created_by,last_open_at,last_open_by,last_close_at,last_close_by,status,\
             locked_by,locked_device"
        );
    }

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
        assert!(text.contains("https://a.example/ | https://u:********@b.example/x"));
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
