//! The profile's `cookie` field -> cookies in the browser.
//!
//! Three formats are accepted, as AdsPower does:
//! * **JSON**: an array of cookie objects as browser extensions export them (Cookie-Editor, EditThisCookie, AdsPower):
//!   `name`, `value`, `domain`, `path`, `expirationDate` / `expires` / `expiry`, `secure`, `httpOnly`, `sameSite`,
//!   `hostOnly`, `session`;
//! * **Netscape** `cookies.txt`: tab-separated `domain flag path secure expiry name value` lines (`#HttpOnly_`
//!   prefix understood, other `#` lines skipped);
//! * **`name=value; name2=value2`**: for the profile's platform domain (or the first start URL's host).
//!
//! [`import`] sends them to a running browser with the DevTools `Storage.setCookies` command, after which the browser
//! keeps them in its own cookie store like any other. The launcher imports once per change of the field
//! ([`crate::launch::start`] remembers a digest), so cookies the sites renew later are not overwritten on every launch.

use crate::cdp::Cdp;
use serde_json::{Value, json};

/// Cookies in the shape of DevTools' `Network.CookieParam`.
pub fn parse(text: &str, default_domain: &str) -> Result<Vec<Value>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    if t.starts_with('[') || t.starts_with('{') {
        return parse_json(t);
    }
    if t.lines().any(|l| l.split('\t').count() >= 7) {
        return Ok(parse_netscape(t));
    }
    parse_pairs(t, default_domain)
}

fn parse_json(t: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| format!("the cookies are not valid JSON: {e}"))?;
    let list = match v {
        Value::Array(a) => a,
        Value::Object(ref o) if o.get("cookies").is_some_and(Value::is_array) => {
            o["cookies"].as_array().cloned().unwrap_or_default()
        }
        o @ Value::Object(_) => vec![o],
        _ => return Err("the cookie JSON must be a list of cookies".into()),
    };
    let s = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).map(str::to_owned);
    let b = |c: &Value, k: &str| c.get(k).and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)));
    let mut out = Vec::new();
    for c in &list {
        let (Some(name), Some(value)) =
            (s(c, "name"), c.get("value").map(|v| v.as_str().map_or(v.to_string(), str::to_owned)))
        else {
            continue;
        };
        let Some(mut domain) = s(c, "domain").filter(|d| !d.is_empty()) else { continue };
        if b(c, "hostOnly") == Some(true) {
            domain = domain.trim_start_matches('.').to_owned();
        }
        let mut p = json!({ "name": name, "value": value, "domain": domain, "path": s(c, "path").unwrap_or_else(|| "/".into()) });
        let secure = b(c, "secure").unwrap_or(false);
        p["secure"] = json!(secure);
        if let Some(h) = b(c, "httpOnly") {
            p["httpOnly"] = json!(h);
        }
        let expires = ["expirationDate", "expires", "expiry"]
            .iter()
            .find_map(|k| c.get(*k).and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())));
        if b(c, "session") != Some(true)
            && let Some(e) = expires.filter(|e| *e > 0.0)
        {
            // some exporters write milliseconds
            p["expires"] = json!(if e > 1e11 { e / 1000.0 } else { e });
        }
        match s(c, "sameSite").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("no_restriction" | "none") => {
                p["sameSite"] = json!("None");
                p["secure"] = json!(true); // a SameSite=None cookie must be Secure, or the browser drops it
            }
            Some("lax") => p["sameSite"] = json!("Lax"),
            Some("strict") => p["sameSite"] = json!("Strict"),
            _ => {}
        }
        out.push(p);
    }
    if out.is_empty() && !list.is_empty() {
        return Err("no cookie in the JSON has a name, a value and a domain".into());
    }
    Ok(out)
}

fn parse_netscape(t: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in t.lines() {
        let (line, http_only) = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => (rest, true),
            None => (line, false),
        };
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let mut p = json!({
            "domain": f[0],
            "path": f[2],
            "secure": f[3].eq_ignore_ascii_case("TRUE"),
            "name": f[5],
            "value": f[6..].join("\t"),
            "httpOnly": http_only,
        });
        if let Ok(e) = f[4].parse::<f64>()
            && e > 0.0
        {
            p["expires"] = json!(e);
        }
        out.push(p);
    }
    out
}

fn parse_pairs(t: &str, default_domain: &str) -> Result<Vec<Value>, String> {
    let domain = default_domain.trim();
    if domain.is_empty() {
        return Err("`name=value` cookies need the profile's platform (or a start URL) to know their site".into());
    }
    let out: Vec<Value> = t
        .split([';', '\n'])
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| json!({ "name": k.trim(), "value": v.trim(), "domain": format!(".{}", domain.trim_start_matches('.')), "path": "/" }))
        .collect();
    if out.is_empty() {
        return Err("no `name=value` pair found".into());
    }
    Ok(out)
}

/// The site `name=value` cookies belong to: the platform domain, else the first start URL's host.
pub fn default_domain(domain_name: &str, tabs: &[String]) -> String {
    let host = |u: &str| {
        let rest = u.split_once("://").map_or(u, |(_, r)| r);
        rest.split(['/', '?', '#', ':']).next().unwrap_or("").trim_start_matches("www.").to_owned()
    };
    let d = domain_name.trim();
    if !d.is_empty() {
        return host(d);
    }
    tabs.iter().map(|t| host(t)).find(|h| !h.is_empty() && h.contains('.')).unwrap_or_default()
}

/// Put `cookies` into the browser whose DevTools browser endpoint is `ws` (`ws://127.0.0.1:port/devtools/browser/…`).
pub fn import(ws: &str, cookies: &[Value]) -> Result<usize, String> {
    if cookies.is_empty() {
        return Ok(0);
    }
    let mut cdp = Cdp::connect(ws).map_err(|e| format!("cannot reach the browser: {e}"))?;
    cdp.call("Storage.setCookies", json!({ "cookies": cookies }))
        .map_err(|e| format!("the browser refused the cookies: {e}"))?;
    Ok(cookies.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_exports_of_cookie_editor_and_edit_this_cookie() {
        let t = r#"[{"domain":".tiktok.com","expirationDate":1999999999.5,"hostOnly":false,"httpOnly":true,"name":"sid","path":"/","sameSite":"no_restriction","secure":false,"session":false,"value":"abc"},
                    {"domain":"www.example.com","hostOnly":true,"name":"a","value":"1","session":true,"expirationDate":1999999999},
                    {"name":"skip-no-domain","value":"x"}]"#;
        let c = parse(t, "").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["domain"], ".tiktok.com");
        assert_eq!((c[0]["sameSite"].as_str(), c[0]["secure"].as_bool()), (Some("None"), Some(true)));
        assert_eq!(c[0]["expires"], 1999999999.5);
        assert_eq!(c[1]["domain"], "www.example.com");
        assert!(c[1].get("expires").is_none(), "a session cookie has no expiry");
        // milliseconds are turned into seconds
        let ms = parse(r#"[{"name":"n","value":"v","domain":"x.com","expires":1999999999000}]"#, "").unwrap();
        assert_eq!(ms[0]["expires"], 1999999999.0);
    }

    #[test]
    fn netscape_files() {
        let t = "# Netscape HTTP Cookie File\n.example.com\tTRUE\t/\tTRUE\t1999999999\tsid\tabc\n#HttpOnly_.example.com\tTRUE\t/\tFALSE\t0\th\tv\n";
        let c = parse(t, "").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0]["name"].as_str(), c[0]["secure"].as_bool(), c[0]["expires"].as_f64()),
            (Some("sid"), Some(true), Some(1999999999.0))
        );
        assert_eq!((c[1]["httpOnly"].as_bool(), c[1].get("expires")), (Some(true), None));
    }

    #[test]
    fn name_value_pairs_need_a_site() {
        let c = parse("a=1; b = two==", "tiktok.com").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[1]["name"].as_str(), c[1]["value"].as_str(), c[1]["domain"].as_str()),
            (Some("b"), Some("two=="), Some(".tiktok.com"))
        );
        assert!(parse("a=1", "").is_err());
        assert_eq!(default_domain("", &["https://www.tiktok.com/foryou".into()]), "tiktok.com");
        assert_eq!(default_domain("https://shop.example.com/x", &[]), "shop.example.com");
    }
}
