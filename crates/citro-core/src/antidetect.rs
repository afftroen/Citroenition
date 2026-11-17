//! Reading profiles out of a running antidetect browser through its Local API (`http://127.0.0.1:50325`, the address
//! The antidetect browser shows under API & MCP), for [`crate::import`].
//!
//! What the antidetect browser hands out: name, group, remark, proxy with its login, platform and account, and (one request per
//! profile) the cookies. It does not hand out its fingerprint settings, so imported profiles get a fresh Citroenition
//! persona. The API allows about one request per second; requests are paced, and "too many requests" answers retried.
//!
//! Plain HTTP to the machine itself only, so a small client is enough: no TLS, `Connection: close`, content-length or
//! chunked bodies. The API key goes in the `Authorization` header and nowhere else.

use crate::import::{Parsed, from_json};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub const DEFAULT_URL: &str = "http://127.0.0.1:50325";
const GAP: Duration = Duration::from_millis(1100);
const TIMEOUT: Duration = Duration::from_secs(20);

struct Client {
    host: String,
    port: u16,
    key: String,
    last: Option<Instant>,
}

fn decode_chunked(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        b = &b[nl + 2..];
        if size == 0 || size > b.len() {
            break;
        }
        out.extend_from_slice(&b[..size]);
        b = b.get(size + 2..).unwrap_or(&[]);
    }
    out
}

impl Client {
    fn new(url: &str, key: &str) -> Result<Client, String> {
        let rest = url.trim().trim_end_matches('/');
        let rest = rest.strip_prefix("http://").ok_or("The antidetect browser address starts with http://")?;
        let hostport = rest.split('/').next().unwrap_or(rest);
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse().map_err(|_| "The antidetect browser address has a bad port")?),
            None => (hostport.to_owned(), 80),
        };
        Ok(Client { host, port, key: key.trim().to_owned(), last: None })
    }

    fn get_once(&mut self, path: &str) -> Result<Value, String> {
        if let Some(t) = self.last {
            let since = t.elapsed();
            if since < GAP {
                std::thread::sleep(GAP - since);
            }
        }
        self.last = Some(Instant::now());
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("cannot find {}: {e}", self.host))?
            .next()
            .ok_or_else(|| format!("cannot find {}", self.host))?;
        let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|_| {
            "The antidetect browser does not answer: is it open, with the Local API turned on (API & MCP)?".to_owned()
        })?;
        s.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
        let auth = if self.key.is_empty() { String::new() } else { format!("Authorization: Bearer {}\r\n", self.key) };
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: {}:{}\r\n{auth}Accept: application/json\r\nConnection: close\r\n\r\n",
            self.host, self.port
        )
        .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).map_err(|e| format!("The antidetect browser stopped answering: {e}"))?;
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("The antidetect browser sent no answer")?;
        let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
        let body = &raw[split + 4..];
        let body = if head.contains("transfer-encoding: chunked") { decode_chunked(body) } else { body.to_vec() };
        let status = head.split_whitespace().nth(1).unwrap_or("");
        if status != "200" {
            return Err(format!("The antidetect browser answered HTTP {status}"));
        }
        serde_json::from_slice(&body).map_err(|_| "The antidetect browser's answer is not JSON".to_owned())
    }

    /// GET a JSON endpoint; `code != 0` is an error, a rate-limit answer is retried.
    fn get(&mut self, path: &str) -> Result<Value, String> {
        for _ in 0..4 {
            let v = self.get_once(path)?;
            if v.get("code").and_then(Value::as_i64) == Some(0) {
                return Ok(v);
            }
            let msg = v.get("msg").and_then(Value::as_str).unwrap_or("").to_owned();
            let lower = msg.to_lowercase();
            if lower.contains("too many") || lower.contains("frequen") {
                std::thread::sleep(GAP * 2);
                continue;
            }
            if lower.contains("api key")
                || lower.contains("apikey")
                || lower.contains("unauthor")
                || lower.contains("token")
            {
                return Err(format!("The antidetect browser refused the API key: {msg}"));
            }
            return Err(format!("The antidetect browser: {msg}"));
        }
        Err("The antidetect browser keeps saying \"too many requests\"; try again in a minute".into())
    }
}

/// The cookies the antidetect browser keeps for profile `id` (its JSON text), or "" when it has none.
fn cookies_of(c: &mut Client, id: &str) -> Result<String, String> {
    let v = c.get(&format!("/api/v2/browser-profile/cookies?profile_id={id}"))?;
    let cookies = v.pointer("/data/cookies").cloned().unwrap_or(Value::Null);
    Ok(match cookies {
        Value::String(s) => s,
        Value::Array(_) => cookies.to_string(),
        _ => String::new(),
    })
}

/// Fetch, from the antidetect browser at `url`, the cookies an export left out (and those of any the antidetect browser profile without
/// cookies in `parsed`), by each profile's the antidetect browser id. Returns how many profiles got cookies.
pub fn fill_cookies(
    url: &str,
    key: &str,
    parsed: &mut Parsed,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<usize, String> {
    let wanted: Vec<usize> = parsed
        .profiles
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            i.profile.source.starts_with("api:") && (i.cookies_elsewhere || i.profile.cookie.trim().is_empty())
        })
        .map(|(n, _)| n)
        .collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let mut c = Client::new(url, key)?;
    c.get("/status").map_err(|e| {
        if e.starts_with("The antidetect browser") {
            e
        } else {
            format!("{url} does not look like other antidetect browsers' Local API ({e})")
        }
    })?;
    let (total, mut got) = (wanted.len(), 0);
    for (done, n) in wanted.into_iter().enumerate() {
        progress(done, total);
        let inc = &mut parsed.profiles[n];
        let id = inc.profile.source.trim_start_matches("api:").to_owned();
        match cookies_of(&mut c, &id) {
            Ok(text) if !text.trim().is_empty() => {
                inc.profile.cookie = text;
                inc.cookies_elsewhere = false;
                got += 1;
            }
            Ok(_) => inc.cookies_elsewhere = false,
            Err(e) => inc.warnings.push(format!("cookies not fetched from the antidetect browser: {e}")),
        }
    }
    progress(total, total);
    Ok(got)
}

/// Every profile in the antidetect browser at `url` (with `key`, its API key), with groups and cookies.
/// `progress(done, total)` follows the per-profile cookie requests.
pub fn fetch(url: &str, key: &str, progress: &mut dyn FnMut(usize, usize)) -> Result<Parsed, String> {
    let mut c = Client::new(url, key)?;
    c.get("/status").map_err(|e| {
        if e.starts_with("The antidetect browser") {
            e
        } else {
            format!("{url} does not look like other antidetect browsers' Local API ({e})")
        }
    })?;
    let mut groups: Vec<(String, String)> = Vec::new();
    for page in 1..=50 {
        let v = c.get(&format!("/api/v1/group/list?page={page}&page_size=100"))?;
        let list = v.pointer("/data/list").and_then(Value::as_array).cloned().unwrap_or_default();
        for g in &list {
            let s = |k: &str| g.get(k).map(|v| v.as_str().map_or(v.to_string(), str::to_owned)).unwrap_or_default();
            groups.push((s("group_id"), s("group_name")));
        }
        if list.len() < 100 {
            break;
        }
    }
    let mut items: Vec<Value> = Vec::new();
    for page in 1..=200 {
        let v = c.get(&format!("/api/v1/user/list?page={page}&page_size=100"))?;
        let list = v.pointer("/data/list").and_then(Value::as_array).cloned().unwrap_or_default();
        let n = list.len();
        items.extend(list);
        if n < 100 {
            break;
        }
    }
    let total = items.len();
    let mut missing = 0;
    for (i, item) in items.iter_mut().enumerate() {
        progress(i, total);
        let Some(id) = item.get("user_id").and_then(Value::as_str).map(str::to_owned) else { continue };
        match cookies_of(&mut c, &id) {
            Ok(text) if !text.trim().is_empty() => item["cookie"] = Value::String(text),
            Ok(_) => {}
            Err(_) => missing += 1,
        }
    }
    progress(total, total);
    let mut out = Parsed::default();
    from_json(&Value::Array(items), &groups, &mut out);
    let used: std::collections::BTreeSet<&str> =
        out.profiles.iter().map(|p| p.group.as_str()).filter(|g| !g.is_empty()).collect();
    out.notes.push(format!(
        "{} profiles and {} groups in the antidetect browser ({} used by profiles).",
        total,
        groups.len(),
        used.len()
    ));
    if missing > 0 {
        out.notes.push(format!("The cookies of {missing} profile(s) could not be read; they are imported without."));
    }
    out.groups = groups.into_iter().map(|(_, n)| n).filter(|n| !n.is_empty()).collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn chunked_bodies() {
        assert_eq!(decode_chunked(b"4\r\nWiki\r\n6;x=1\r\npedia \r\n0\r\n\r\n"), b"Wikipedia ");
    }

    /// A fake the antidetect browser: checks the key, answers status, groups, one page of profiles and their cookies.
    #[test]
    fn a_whole_fetch_against_a_fake_local_api() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut req = String::new();
                let mut buf = [0u8; 1024];
                while !req.contains("\r\n\r\n") {
                    let n = s.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                let body = if !req.contains("Authorization: Bearer KEY") {
                    r#"{"code":-1,"msg":"api key is invalid"}"#.to_owned()
                } else if path == "/status" {
                    r#"{"code":0,"msg":"success"}"#.to_owned()
                } else if path.starts_with("/api/v1/group/list") {
                    r#"{"code":0,"data":{"list":[{"group_id":"7","group_name":"TikTok Ads"}]}}"#.to_owned()
                } else if path.starts_with("/api/v1/user/list") {
                    r#"{"code":0,"data":{"list":[{"user_id":"k1","serial_number":"5","name":"A","group_id":"7","group_name":"","user_proxy_config":{"proxy_soft":"no_proxy"}}]}}"#.to_owned()
                } else {
                    r#"{"code":0,"data":{"cookies":"[{\"name\":\"n\",\"value\":\"v\",\"domain\":\".x.com\"}]"}}"#
                        .to_owned()
                };
                // chunked, as other antidetect browsers sends it
                let chunked = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{chunked}"
                );
                let _ = s.shutdown(std::net::Shutdown::Write);
            }
        });
        let url = format!("http://127.0.0.1:{port}");
        let e = fetch(&url, "WRONG", &mut |_, _| {}).unwrap_err();
        assert!(e.contains("API key"), "{e}");
        let mut seen = (0, 0);
        let p = fetch(&url, "KEY", &mut |d, t| seen = (d, t)).unwrap();
        assert_eq!(seen, (1, 1));
        assert_eq!(p.profiles.len(), 1);
        assert_eq!((p.profiles[0].group.as_str(), p.profiles[0].profile.source.as_str()), ("TikTok Ads", "api:k1"));
        assert!(p.profiles[0].profile.cookie.contains("x.com"));
        assert_eq!(p.groups, ["TikTok Ads"]);
    }
}
