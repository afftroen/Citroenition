//! Secret handling for profiles: masking for list views, RFC 7386 merge patches, and un-masking on edit.
//!
//! A profile carries four obvious secrets (`password`, `fakey`, `cookie`, `user_proxy_config.proxy_password`) and a
//! few less obvious places a credential can hide: `user:pass@` inside URLs (`proxy_url`, `tabs`, `launch_args`) and
//! free-form keys of the proxy / fingerprint config whose *name* says secret (`ProxyPassword`, `api_token`, ...).

use super::util::scrub_secrets;
use crate::model::MASK;
use citro_core::profile::Profile;
use serde_json::{Map, Value};

/// Key fragments (lower case) that mark a free-form config value as a secret.
const SECRET_KEYS: [&str; 8] = ["pass", "pwd", "secret", "token", "cookie", "fakey", "2fa", "apikey"];

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|frag| k.contains(frag)) || k == "api_key"
}

/// `scheme://user:secret@host/..` -> `scheme://user:********@host/..` for every URL inside `s`; a bare token as
/// user (`https://ghp_xxx@host`) is masked entirely.
pub fn mask_url_credentials(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail.find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace()).unwrap_or(tail.len());
        let (authority, after) = tail.split_at(end);
        match authority.rfind('@') {
            Some(at) => {
                let (userinfo, host) = authority.split_at(at);
                if let Some((user, _)) = userinfo.split_once(':') {
                    out.push_str(user);
                    out.push(':');
                }
                out.push_str(MASK);
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

// ---- free-text log lines --------------------------------------------------------------------------------------------

/// Key fragments that mark a value in a *log line* as secret: everything profiles mask plus the HTTP / session words.
fn is_secret_log_key(key: &str) -> bool {
    const FRAGMENTS: [&str; 8] = ["auth", "bearer", "credential", "private", "csrf", "jwt", "session_id", "sessionid"];
    let k = key.to_ascii_lowercase();
    is_secret_key(&k) || FRAGMENTS.iter().any(|f| k.contains(f)) || matches!(k.as_str(), "sid" | "otp")
}

/// Bytes a word can be made of when it is the `key` of `key=value`.
fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Where an unquoted value stops.
fn ends_value(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b',' | b';' | b'&' | b'}' | b']' | b')' | b'"' | b'\'')
}

/// The end of the run of value bytes that starts at `from`.
fn run_end(b: &[u8], from: usize) -> usize {
    b[from..].iter().position(|&c| ends_value(c)).map_or(b.len(), |p| from + p)
}

/// The span of the secret that starts at `at` (right behind the separator): the inside of a quoted string (escapes
/// honoured), or the run up to the next delimiter. A scheme word in front (`Bearer x`, `Basic x`) is skipped; a cookie
/// runs to the end of its line because its own `;` separate more secrets.
fn value_span(s: &str, at: usize, cookie: bool) -> (usize, usize) {
    let b = s.as_bytes();
    if at >= b.len() {
        return (at, at);
    }
    if matches!(b[at], b'"' | b'\'') {
        let (quote, start) = (b[at], at + 1);
        let mut end = start;
        while end < b.len() && b[end] != quote {
            end += if b[end] == b'\\' { 2 } else { 1 };
        }
        return (start, end.min(b.len()));
    }
    if cookie {
        return (at, b[at..].iter().position(|&c| c == b'\n' || c == b'\r').map_or(b.len(), |p| at + p));
    }
    let first_end = run_end(b, at);
    let first = &s[at..first_end];
    if (first.eq_ignore_ascii_case("bearer") || first.eq_ignore_ascii_case("basic")) && b.get(first_end) == Some(&b' ')
    {
        return (first_end + 1, run_end(b, first_end + 1));
    }
    (at, first_end)
}

/// Mask the value of every `key=value`, `key: value` and `"key":"value"` whose key says secret (`password`,
/// `proxy_password`, `api_token`, `Cookie`, `Authorization`, ...) and the credential behind a bare `Bearer`.
fn mask_key_values(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if !is_key_byte(b[i]) {
            let Some(c) = s[i..].chars().next() else { break };
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let start = i;
        while i < b.len() && is_key_byte(b[i]) {
            i += 1;
        }
        let word = &s[start..i];
        out.push_str(word);
        if word.eq_ignore_ascii_case("bearer") && b.get(i) == Some(&b' ') && run_end(b, i + 1) > i + 1 {
            out.push(' ');
            out.push_str(MASK);
            i = run_end(b, i + 1);
            continue;
        }
        if !is_secret_log_key(word) {
            continue;
        }
        let mut j = i;
        if j < b.len() && matches!(b[j], b'"' | b'\'') {
            j += 1; // the closing quote of a JSON key
        }
        while j < b.len() && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        if j >= b.len() || !matches!(b[j], b'=' | b':') {
            continue;
        }
        j += 1;
        while j < b.len() && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        let (from, to) = value_span(s, j, word.to_ascii_lowercase().contains("cookie"));
        if from < to {
            out.push_str(&s[i..from]);
            out.push_str(MASK);
            i = to;
        }
    }
    out
}

/// Scrub a free-text log line before it is stored (and before a client sends it): token look-alikes, `user:pass@` in
/// URLs, `password=...` / `"token": "..."` style pairs, `Authorization: Bearer ...` and cookies. Idempotent.
pub fn scrub_log_text(s: &str) -> String {
    mask_key_values(&mask_url_credentials(&scrub_secrets(s)))
}

fn mask_if_set(s: &mut String) {
    if !s.is_empty() {
        *s = MASK.to_owned();
    }
}

/// Replace every non-empty string anywhere inside `v` by the mask.
fn mask_strings(v: &mut Value) {
    match v {
        Value::String(s) => mask_if_set(s),
        Value::Array(items) => items.iter_mut().for_each(mask_strings),
        Value::Object(o) => o.values_mut().for_each(mask_strings),
        _ => {}
    }
}

/// Mask the values of secret-named keys in a free-form map (recursively).
fn scrub_map(map: &mut Map<String, Value>) {
    for (k, v) in map.iter_mut() {
        if is_secret_key(k) {
            mask_strings(v);
            continue;
        }
        match v {
            Value::Object(o) => scrub_map(o),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(o) = item {
                        scrub_map(o);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Mask every secret of a profile in place (list responses, CSV, non-privileged detail views).
pub fn redact(p: &mut Profile) {
    mask_if_set(&mut p.password);
    mask_if_set(&mut p.fakey);
    mask_if_set(&mut p.cookie);
    let proxy = &mut p.user_proxy_config;
    if proxy.proxy_password.as_deref().is_some_and(|s| !s.is_empty()) {
        proxy.proxy_password = Some(MASK.to_owned());
    }
    if let Some(url) = proxy.proxy_url.as_mut() {
        *url = mask_url_credentials(url);
    }
    scrub_map(&mut proxy.extra);
    scrub_map(&mut p.fingerprint_config.0);
    for s in p.tabs.iter_mut().chain(p.launch_args.iter_mut()) {
        *s = mask_url_credentials(s);
    }
    p.domain_name = mask_url_credentials(&p.domain_name);
}

/// RFC 7386 JSON merge patch: objects merge recursively, `null` deletes a key, everything else replaces.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            if let Some(t) = target.as_object_mut() {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// Restore masked values in free-form maps (recursively): a string equal to the mask is replaced by the old value
/// at the same path, whatever its type.
fn restore_in_map(old: &Map<String, Value>, new: &mut Map<String, Value>) {
    for (k, v) in new.iter_mut() {
        let Some(ov) = old.get(k) else { continue };
        match v {
            Value::String(s) if s == MASK => *v = ov.clone(),
            Value::Object(n) => {
                if let Value::Object(o) = ov {
                    restore_in_map(o, n);
                }
            }
            _ => {}
        }
    }
}

/// Put the old secret back wherever the client echoed the mask (`"********"`) or a mask-bearing URL: a GUI that
/// edited the name of a redacted profile and PATCHed the whole object back must not wipe the real secrets.
pub fn restore_masked(old: &Profile, new: &mut Profile) {
    fn keep(old: &str, new: &mut String) {
        if new == MASK {
            *new = old.to_owned();
        }
    }
    fn keep_url(old: &str, new: &mut String) {
        if *new == mask_url_credentials(old) {
            *new = old.to_owned();
        }
    }
    keep(&old.password, &mut new.password);
    keep(&old.fakey, &mut new.fakey);
    keep(&old.cookie, &mut new.cookie);
    keep_url(&old.domain_name, &mut new.domain_name);
    if new.user_proxy_config.proxy_password.as_deref() == Some(MASK) {
        new.user_proxy_config.proxy_password = old.user_proxy_config.proxy_password.clone();
    }
    if let (Some(n), Some(o)) = (new.user_proxy_config.proxy_url.as_mut(), old.user_proxy_config.proxy_url.as_ref()) {
        keep_url(o, n);
    }
    for (o, n) in old.tabs.iter().zip(new.tabs.iter_mut()) {
        keep_url(o, n);
    }
    for (o, n) in old.launch_args.iter().zip(new.launch_args.iter_mut()) {
        keep_url(o, n);
    }
    restore_in_map(&old.user_proxy_config.extra, &mut new.user_proxy_config.extra);
    restore_in_map(&old.fingerprint_config.0, &mut new.fingerprint_config.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secret_profile() -> Profile {
        serde_json::from_value(json!({
            "profile_id": "kabc123", "name": "shop", "username": "bob@example.com",
            "password": "pw-secret", "fakey": "JBSWY3DPEHPK3PXP", "cookie": "[{\"name\":\"sid\",\"value\":\"c00kie\"}]",
            "tabs": ["https://shop.example.com/", "https://adm:tabpass@intranet.example.com/x"],
            "launch_args": ["--proxy-server=http://pu:argpass@h:3128", "--lang=en"],
            "user_proxy_config": {"proxy_type": "socks5", "proxy_host": "1.2.3.4", "proxy_port": "1080",
                "proxy_user": "pu", "proxy_password": "px-secret", "proxy_url": "socks5://pu:urlpass@1.2.3.4:1080",
                "ProxyPassword": "extra-secret", "region": "de"},
            "fingerprint_config": {"language": ["de-DE"], "timezone": "Europe/Berlin",
                "ProxyPassword": "fp-secret", "nested": {"api_token": "tok-123", "note": "visible"}}
        }))
        .unwrap()
    }

    #[test]
    fn redact_masks_every_known_secret_place() {
        let mut p = secret_profile();
        redact(&mut p);
        let dump = serde_json::to_string(&p).unwrap();
        for leaked in [
            "pw-secret",
            "JBSWY3DPEHPK3PXP",
            "c00kie",
            "px-secret",
            "urlpass",
            "tabpass",
            "argpass",
            "extra-secret",
            "fp-secret",
            "tok-123",
        ] {
            assert!(!dump.contains(leaked), "{leaked} leaked: {dump}");
        }
        assert_eq!((p.password.as_str(), p.fakey.as_str()), (MASK, MASK));
        assert_eq!(p.user_proxy_config.proxy_password.as_deref(), Some(MASK));
        assert_eq!(p.user_proxy_config.proxy_url.as_deref(), Some("socks5://pu:********@1.2.3.4:1080"));
        // non-secrets stay readable
        assert_eq!(p.username, "bob@example.com");
        assert_eq!(p.user_proxy_config.proxy_user.as_deref(), Some("pu"));
        assert_eq!(p.user_proxy_config.extra["region"], "de");
        assert_eq!(p.fingerprint_config.0["nested"]["note"], "visible");
        assert_eq!(p.tabs[0], "https://shop.example.com/");
        assert_eq!(p.launch_args[1], "--lang=en");
    }

    #[test]
    fn empty_secrets_stay_empty() {
        let mut p = Profile { name: "x".into(), ..Default::default() };
        redact(&mut p);
        assert_eq!((p.password.as_str(), p.fakey.as_str(), p.cookie.as_str()), ("", "", ""));
        assert!(p.user_proxy_config.proxy_password.is_none());
    }

    #[test]
    fn url_credentials_are_masked_anywhere_in_a_string() {
        assert_eq!(mask_url_credentials("https://u:p@h/x"), "https://u:********@h/x");
        assert_eq!(mask_url_credentials("https://TOKEN@h/x"), "https://********@h/x");
        assert_eq!(
            mask_url_credentials("a http://x:y@h1 b https://h2/p?q=a@b"),
            "a http://x:********@h1 b https://h2/p?q=a@b"
        );
        assert_eq!(mask_url_credentials("no urls here"), "no urls here");
        assert_eq!(mask_url_credentials("https://h/path@notauth"), "https://h/path@notauth");
    }

    #[test]
    fn merge_patch_follows_rfc_7386() {
        let mut v = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1, 2]});
        merge_patch(&mut v, &json!({"a": null, "b": {"c": 9, "x": 1}, "e": [3], "n": "new"}));
        assert_eq!(v, json!({"b": {"c": 9, "d": 3, "x": 1}, "e": [3], "n": "new"}));
        let mut s = json!("scalar");
        merge_patch(&mut s, &json!({"k": 1}));
        assert_eq!(s, json!({"k": 1}));
    }

    #[test]
    fn echoing_the_mask_back_keeps_the_real_secrets() {
        let old = secret_profile();
        let mut echoed = old.clone();
        redact(&mut echoed);
        echoed.name = "renamed".into();
        restore_masked(&old, &mut echoed);
        let mut expected = old.clone();
        expected.name = "renamed".into();
        assert_eq!(echoed, expected);
        // a genuinely new password wins
        let mut changed = old.clone();
        changed.password = "brand-new".into();
        restore_masked(&old, &mut changed);
        assert_eq!(changed.password, "brand-new");
    }

    #[test]
    fn log_lines_lose_every_kind_of_secret() {
        let t = format!("citro_{}", "ab".repeat(32));
        let m = MASK;
        let cases: Vec<(String, String)> = vec![
            (format!("Authorization: Bearer {t}"), format!("Authorization: Bearer {m}")),
            ("password=hunter2 next".into(), format!("password={m} next")),
            (r#"{"password":"a b\"c","x":1}"#.into(), format!(r#"{{"password":"{m}","x":1}}"#)),
            ("proxy http://user:s3cret@10.0.0.1:8080/ ok".into(), format!("proxy http://user:{m}@10.0.0.1:8080/ ok")),
            ("GET /v1/x?token=abc&y=1".into(), format!("GET /v1/x?token={m}&y=1")),
            ("Cookie: sid=abc; other=1\nnext line".into(), format!("Cookie: {m}\nnext line")),
            ("sent header Bearer abc123 ok".into(), format!("sent header Bearer {m} ok")),
            ("proxy_password = 'p w' and API_KEY:k1".into(), format!("proxy_password = '{m}' and API_KEY:{m}")),
            ("Passwort=geheim ünï".into(), format!("Passwort={m} ünï")),
            (
                "tab navigated to https://example.test/page/3 (200 ms); session restored: 3 tabs".into(),
                "tab navigated to https://example.test/page/3 (200 ms); session restored: 3 tabs".into(),
            ),
            ("password= odd and token:".into(), format!("password= {m} and token:")),
            ("ends with a key password".into(), "ends with a key password".into()),
        ];
        for (input, want) in cases {
            let got = scrub_log_text(&input);
            assert_eq!(got, want, "input: {input}");
            assert_eq!(scrub_log_text(&got), got, "scrubbing twice changes nothing: {input}");
        }
        assert!(!scrub_log_text(&format!("login with {t} failed")).contains(&t[6..]));
    }
}
