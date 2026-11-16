//! CSV export of profiles in a format every spreadsheet reads.
//!
//! * RFC 4180: comma separated, CRLF line ends (also after the last record), fields with a comma, a quote, CR or
//!   LF are quoted and quotes are doubled.
//! * UTF-8 with a byte order mark, so Excel does not guess a legacy code page.
//! * Formula-injection safe: a text cell that would start a formula (`= + - @`, TAB, CR) gets a leading `'`.
//! * **No secrets, ever**: only the named, non-secret columns below are written. [`CsvRow`] has no field for a
//!   password, a 2FA key, a cookie or a proxy password, so a bug elsewhere cannot leak one into a file.
//!
//! The 24 columns and their order are the same as the team hub's export; columns that only exist with a hub
//! (`created_by`, `last_open_by`, `last_close_by`, `locked_by`, `locked_device`) are empty for a local export.

use crate::timefmt::DateTime;
use citro_core::locale;
use citro_core::profile::Profile;
use std::fmt::Write as _;

/// The header row, exactly as promised to everybody who parses these files.
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

/// One exported profile. Every field is already final text; timestamps are unix seconds (`0` = never).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CsvRow {
    pub no: u64,
    pub id: String,
    pub name: String,
    /// The group's *name*; empty for Ungrouped.
    pub group: String,
    pub remark: String,
    pub persona_os: String,
    pub country: String,
    pub proxy_type: String,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub language: String,
    pub timezone: String,
    pub tabs: Vec<String>,
    pub account_username: String,
    pub created_at: i64,
    pub created_by: String,
    pub last_open_at: i64,
    pub last_open_by: String,
    pub last_close_at: i64,
    pub last_close_by: String,
    /// `open` or `closed`.
    pub status: String,
    pub locked_by: String,
    pub locked_device: String,
}

/// `fingerprint_config.language`: an array of tags or a single string -> a comma list.
fn pinned_language(p: &Profile) -> String {
    let fp = &p.fingerprint_config;
    let list = fp.list("language");
    if !list.is_empty() {
        return list.join(",");
    }
    fp.str("language").unwrap_or_default()
}

impl CsvRow {
    /// Builds the row for a stored profile. `group_name` is empty for Ungrouped. Passwords, 2FA keys, cookies and
    /// the proxy password are never read.
    ///
    /// `language` / `timezone` are what the profile pins; when it pins nothing they are the ones its country implies
    /// (the launcher derives them the same way), so the sheet is useful for profiles made in the GUI too.
    pub fn from_profile(p: &Profile, group_name: &str, running: bool) -> CsvRow {
        let hop = p.user_proxy_config.hop();
        let country = locale::country(&p.country);
        let mut language = pinned_language(p);
        if language.is_empty() {
            language = country.map(|(_, tag)| tag.to_owned()).unwrap_or_default();
        }
        let mut timezone = p.fingerprint_config.str("timezone").filter(|_| {
            p.fingerprint_config.str("automatic_timezone").as_deref() == Some("0")
                || p.fingerprint_config.str("automatic_timezone").is_none()
        });
        if timezone.as_deref().is_none_or(str::is_empty) {
            timezone = country.map(|(tz, _)| tz.to_owned());
        }
        CsvRow {
            no: p.serial_number,
            id: p.profile_id.clone(),
            name: p.name.clone(),
            group: group_name.to_owned(),
            remark: p.remark.clone(),
            persona_os: p.persona_os.clone(),
            country: p.country.to_ascii_uppercase(),
            proxy_type: hop.as_ref().map(|h| h.scheme.clone()).unwrap_or_default(),
            proxy_host: hop.as_ref().map(|h| h.host.clone()).unwrap_or_default(),
            proxy_port: hop.as_ref().map(|h| h.port.clone()).unwrap_or_default(),
            proxy_user: hop.as_ref().map(|h| h.account.clone()).unwrap_or_default(),
            language,
            timezone: timezone.unwrap_or_default(),
            tabs: p.tabs.iter().map(|t| mask_url_credentials(t)).collect(),
            account_username: p.username.clone(),
            created_at: p.created_time,
            created_by: String::new(),
            last_open_at: p.last_open_time,
            last_open_by: String::new(),
            last_close_at: p.last_close_time,
            last_close_by: String::new(),
            status: if running { "open" } else { "closed" }.to_owned(),
            locked_by: String::new(),
            locked_device: String::new(),
        }
    }
}

/// `scheme://user:pass@host/x` -> `scheme://user:***@host/x`; a user-only login (a token) is masked whole.
pub fn mask_url_credentials(url: &str) -> String {
    let Some(i) = url.find("://") else { return url.to_owned() };
    let (head, rest) = url.split_at(i + 3);
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    match authority.rsplit_once('@') {
        None => url.to_owned(),
        Some((userinfo, host)) => {
            let masked = match userinfo.split_once(':') {
                Some((user, _)) => format!("{user}:***"),
                None => "***".to_owned(),
            };
            format!("{head}{masked}@{host}{tail}")
        }
    }
}

/// Unix seconds -> `2026-11-15T13:03:22Z`; empty for "never".
pub fn iso_utc(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let d = DateTime::from_unix(ts, 0);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", d.year, d.month, d.day, d.hour, d.minute, d.second)
}

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

fn push_row(out: &mut String, cells: &[String]) {
    out.push_str(&cells.join(","));
    out.push_str("\r\n");
}

/// The whole file: optional BOM, the header, one CRLF-terminated line per row.
pub fn write(rows: &[CsvRow], bom: bool) -> Vec<u8> {
    let mut text = String::with_capacity(256 + rows.len() * 220);
    push_row(&mut text, &HEADER.map(str::to_owned));
    for r in rows {
        let mut no = String::new();
        let _ = write!(no, "{}", r.no);
        let cells = [
            no,
            text_cell(&r.id),
            text_cell(&r.name),
            text_cell(&r.group),
            text_cell(&r.remark),
            text_cell(&r.persona_os),
            text_cell(&r.country),
            text_cell(&r.proxy_type),
            text_cell(&r.proxy_host),
            text_cell(&r.proxy_port),
            text_cell(&r.proxy_user),
            text_cell(&r.language),
            text_cell(&r.timezone),
            text_cell(&r.tabs.join(" | ")),
            text_cell(&r.account_username),
            iso_utc(r.created_at),
            text_cell(&r.created_by),
            iso_utc(r.last_open_at),
            text_cell(&r.last_open_by),
            iso_utc(r.last_close_at),
            text_cell(&r.last_close_by),
            text_cell(&r.status),
            text_cell(&r.locked_by),
            text_cell(&r.locked_device),
        ];
        push_row(&mut text, &cells);
    }
    let mut out = Vec::with_capacity(text.len() + 3);
    if bom {
        out.extend_from_slice(UTF8_BOM);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use citro_core::profile::{Profile, ProxyConfig};
    use serde_json::json;

    fn secret_profile() -> Profile {
        let mut p: Profile = serde_json::from_value(json!({
            "profile_id": "kabc123", "serial_number": 7,
            "name": "Shop, \"main\"\nline2", "remark": "=HYPERLINK(\"http://x\")", "username": "amelie@example.com",
            "password": "pw-SECRET", "fakey": "2FA-SECRET", "cookie": "COOKIE-SECRET",
            "tabs": ["https://a.example/", "https://u:tab-SECRET@b.example/x"],
            "country": "de", "persona_os": "linux", "group_id": "3",
            "created_time": 1_700_000_000_i64, "last_open_time": 1_700_003_600_i64, "last_close_time": 0,
            "user_proxy_config": {"proxy_type": "socks5", "proxy_host": "203.0.113.9", "proxy_port": 1080,
                "proxy_user": "pu", "proxy_password": "px-SECRET"},
            "fingerprint_config": {"language": ["de-DE", "en-US"], "timezone": "Europe/Berlin", "automatic_timezone": "0"}
        }))
        .unwrap();
        p.user_proxy_config.proxy_soft = Some("other".into());
        p
    }

    fn text(rows: &[CsvRow], bom: bool) -> String {
        String::from_utf8(write(rows, bom)).unwrap()
    }

    #[test]
    fn header_is_the_contract() {
        assert_eq!(
            HEADER.join(","),
            "no,id,name,group,remark,persona_os,country,proxy_type,proxy_host,proxy_port,proxy_user,language,timezone,\
             tabs,account_username,created_at,created_by,last_open_at,last_open_by,last_close_at,last_close_by,status,\
             locked_by,locked_device"
        );
        assert_eq!(HEADER.len(), 24);
    }

    #[test]
    fn quoting_follows_rfc_4180() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a,b"), "\"a,b\"");
        assert_eq!(quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(quote("two\nlines"), "\"two\nlines\"");
        assert_eq!(quote("cr\rhere"), "\"cr\rhere\"");
        assert_eq!(quote("ünï ß é"), "ünï ß é");
        assert_eq!(quote(""), "");
    }

    #[test]
    fn formulas_are_neutralised() {
        for bad in ["=1+1", "+1", "-2", "@SUM(A1)", "\tx", "\rx"] {
            assert!(neutralize(bad).starts_with('\''), "{bad:?}");
        }
        for ok in ["a=b", "1+1", " =x", "", "'quoted", "x-y"] {
            assert_eq!(neutralize(ok), ok);
        }
        assert_eq!(text_cell("=cmd|' /C calc'!A0"), "'=cmd|' /C calc'!A0");
        // the quote goes first, then the field is CSV-quoted
        assert_eq!(text_cell("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn bom_crlf_and_trailing_newline() {
        let rows = [CsvRow::from_profile(&secret_profile(), "Vinted FR", false)];
        let with = write(&rows, true);
        assert!(with.starts_with(UTF8_BOM));
        assert!(!write(&rows, false).starts_with(UTF8_BOM));
        let t = text(&rows, false);
        assert!(t.ends_with("\r\n"));
        // every record ends in CRLF, and no bare LF exists outside quoted fields (the name has one inside quotes)
        let unquoted: String = t.split('"').step_by(2).collect();
        assert!(!unquoted.replace("\r\n", "").contains(['\n', '\r']));
        assert_eq!(unquoted.matches("\r\n").count(), 2, "header + one record");
    }

    #[test]
    fn no_secret_ever_reaches_the_file() {
        let rows = [CsvRow::from_profile(&secret_profile(), "Vinted FR", true)];
        let t = text(&rows, true);
        for secret in ["pw-SECRET", "2FA-SECRET", "COOKIE-SECRET", "px-SECRET", "tab-SECRET"] {
            assert!(!t.contains(secret), "{secret} leaked");
        }
        // what is meant to be there is there
        assert!(t.contains("amelie@example.com"));
        assert!(t.contains("https://u:***@b.example/x"));
        assert!(t.contains(",socks5,203.0.113.9,1080,pu,"));
    }

    #[test]
    fn a_full_record_matches_the_documented_columns() {
        let rows = [CsvRow::from_profile(&secret_profile(), "Vinted FR", true)];
        let t = text(&rows, false);
        let mut lines = t.split("\r\n");
        assert_eq!(lines.next().unwrap(), HEADER.join(","));
        let rec = lines.next().unwrap_or_default();
        // the record starts with the serial, id and the quoted multi-line name, and the quoted formula remark
        assert!(rec.starts_with("7,kabc123,\"Shop, \"\"main\"\""), "{rec:?}");
        assert!(t.contains("\"'=HYPERLINK(\"\"http://x\"\")\""));
        assert!(t.contains(",Vinted FR,"));
        assert!(t.contains(",linux,DE,socks5,"));
        assert!(t.contains(",\"de-DE,en-US\",Europe/Berlin,"));
        assert!(t.contains("https://a.example/ | https://u:***@b.example/x"));
        assert!(t.contains(",2023-11-14T22:13:20Z,,2023-11-14T23:13:20Z,,,,open,,"));
    }

    #[test]
    fn rows_parse_back_with_24_fields() {
        // a tiny RFC 4180 reader proves that the writer's quoting round-trips
        fn parse(t: &str) -> Vec<Vec<String>> {
            let (mut rows, mut row, mut cell, mut q) = (Vec::new(), Vec::new(), String::new(), false);
            let mut it = t.chars().peekable();
            while let Some(c) = it.next() {
                match (c, q) {
                    ('"', true) if it.peek() == Some(&'"') => {
                        cell.push('"');
                        it.next();
                    }
                    ('"', _) => q = !q,
                    (',', false) => row.push(std::mem::take(&mut cell)),
                    ('\r', false) if it.peek() == Some(&'\n') => {
                        it.next();
                        row.push(std::mem::take(&mut cell));
                        rows.push(std::mem::take(&mut row));
                    }
                    (c, _) => cell.push(c),
                }
            }
            rows
        }
        let mut rows = vec![CsvRow::from_profile(&secret_profile(), "Vinted FR", false)];
        rows.push(CsvRow { no: 8, name: "plain".into(), ..Default::default() });
        let parsed = parse(&text(&rows, false));
        assert_eq!(parsed.len(), 3);
        assert!(parsed.iter().all(|r| r.len() == 24), "{:?}", parsed.iter().map(Vec::len).collect::<Vec<_>>());
        assert_eq!(parsed[1][2], "Shop, \"main\"\nline2");
        assert_eq!(parsed[1][4], "'=HYPERLINK(\"http://x\")");
        assert_eq!(parsed[2][0], "8");
    }

    #[test]
    fn empty_export_is_just_the_header() {
        assert_eq!(text(&[], false), format!("{}\r\n", HEADER.join(",")));
    }

    #[test]
    fn locale_is_derived_from_the_country_when_nothing_is_pinned() {
        let p = Profile { country: "fr".into(), ..Default::default() };
        let r = CsvRow::from_profile(&p, "", false);
        assert_eq!((r.language.as_str(), r.timezone.as_str(), r.country.as_str()), ("fr-FR", "Europe/Paris", "FR"));
        let none = CsvRow::from_profile(&Profile::default(), "", false);
        assert_eq!((none.language.as_str(), none.timezone.as_str()), ("", ""));
    }

    #[test]
    fn a_direct_connection_has_empty_proxy_columns() {
        let mut p = secret_profile();
        p.user_proxy_config = ProxyConfig { proxy_soft: Some("no_proxy".into()), ..Default::default() };
        let r = CsvRow::from_profile(&p, "", false);
        assert_eq!((r.proxy_type.as_str(), r.proxy_host.as_str(), r.proxy_port.as_str()), ("", "", ""));
    }

    #[test]
    fn url_credentials_are_masked() {
        assert_eq!(mask_url_credentials("https://u:p@h.example/x"), "https://u:***@h.example/x");
        assert_eq!(mask_url_credentials("https://TOKEN@h.example/"), "https://***@h.example/");
        assert_eq!(mask_url_credentials("https://h.example/@user"), "https://h.example/@user");
        assert_eq!(mask_url_credentials("about:blank"), "about:blank");
        assert_eq!(mask_url_credentials("http://h.example"), "http://h.example");
    }

    #[test]
    fn timestamps_are_iso_utc() {
        assert_eq!(iso_utc(0), "");
        assert_eq!(iso_utc(-5), "");
        assert_eq!(iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
