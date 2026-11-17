//! "Check proxy": is the proxy reachable, does it accept the login, and what does the outside world see?
//!
//! No TLS stack is linked into the GUI (it must build without a C toolchain), so the check speaks plain HTTP to an
//! IP-echo service *through* the proxy: an absolute-form `GET` for HTTP proxies, a SOCKS5 tunnel for SOCKS5. Proxy
//! types that need more (HTTPS proxies, SOCKS4) are only checked for TCP reachability, and the result says so.
//!
//! Messages never contain the proxy password.

use super::{BResult, BackendError, ProxyCheck};
use citro_core::profile::{ProxyConfig, ProxyHop};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// The echo service. Plain HTTP on purpose; `ip-api.com` answers `{"status","countryCode","query"}` over port 80.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Echo {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Default for Echo {
    fn default() -> Self {
        Echo { host: "ip-api.com".into(), port: 80, path: "/json?fields=status,message,countryCode,query".into() }
    }
}

#[derive(Debug, Deserialize)]
struct EchoReply {
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default, rename = "countryCode")]
    country_code: String,
    #[serde(default)]
    query: String,
}

pub fn check(c: &ProxyConfig, timeout: Duration) -> BResult<ProxyCheck> {
    let Some(hop) = c.hop() else {
        return Err(BackendError::Invalid("no proxy is configured (direct connection)".into()));
    };
    Ok(check_hop(&hop, &Echo::default(), timeout))
}

fn ms(t: Instant) -> u32 {
    u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX)
}

pub fn check_hop(hop: &ProxyHop, echo: &Echo, timeout: Duration) -> ProxyCheck {
    let started = Instant::now();
    let fail = |message: String| ProxyCheck {
        ok: false,
        ip: String::new(),
        country: String::new(),
        latency_ms: ms(started),
        message,
    };
    let Ok(port) = hop.port.trim().parse::<u16>() else {
        return fail(format!("`{}` is not a valid port", hop.port));
    };
    let target = format!("{}:{port}", hop.host);
    let addr = match (hop.host.as_str(), port).to_socket_addrs().map(|mut a| a.next()) {
        Ok(Some(a)) => a,
        Ok(None) | Err(_) => return fail(format!("cannot resolve {}", hop.host)),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => return fail(format!("cannot connect to {target}: {}", io_reason(&e))),
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_nodelay(true);

    let scheme = hop.scheme.to_ascii_lowercase();
    let request = match scheme.as_str() {
        "http" => {
            let auth = if hop.account.is_empty() {
                String::new()
            } else {
                format!(
                    "Proxy-Authorization: Basic {}\r\n",
                    base64(format!("{}:{}", hop.account, hop.password).as_bytes())
                )
            };
            format!(
                "GET http://{host}{path} HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n",
                host = echo.host,
                path = echo.path
            )
        }
        "socks5" | "socks5h" => {
            if let Err(e) = socks5_connect(&mut stream, hop, echo) {
                return fail(e);
            }
            format!(
                "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
                host = echo.host,
                path = echo.path
            )
        }
        other => {
            return ProxyCheck {
                ok: true,
                ip: String::new(),
                country: String::new(),
                latency_ms: ms(started),
                message: format!(
                    "{target} accepts connections (a {other} proxy is only checked for reachability here)"
                ),
            };
        }
    };
    if let Err(e) = stream.write_all(request.as_bytes()) {
        return fail(format!("{target} closed the connection: {}", io_reason(&e)));
    }
    let raw = read_answer(&mut stream, started + timeout);
    let text = String::from_utf8_lossy(&raw);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return fail(format!("{target} did not answer with HTTP"));
    };
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    match status {
        200 => {}
        407 => return fail("the proxy rejected the user name / password (407)".into()),
        0 => return fail(format!("{target} did not answer with HTTP")),
        s => return fail(format!("the proxy answered HTTP {s}")),
    }
    // `Connection: close` + no Content-Length games: the JSON object is the first `{` .. last `}`.
    let json = match (body.find('{'), body.rfind('}')) {
        (Some(a), Some(b)) if a < b => &body[a..=b],
        _ => return fail("the IP service sent an unexpected answer".into()),
    };
    match serde_json::from_str::<EchoReply>(json) {
        Ok(r) if r.status == "success" || (r.status.is_empty() && !r.query.is_empty()) => ProxyCheck {
            ok: true,
            country: r.country_code.to_ascii_lowercase(),
            message: format!("exit IP {}", r.query),
            ip: r.query,
            latency_ms: ms(started),
        },
        Ok(r) => fail(format!(
            "the IP service refused the lookup: {}",
            if r.message.is_empty() { &r.status } else { &r.message }
        )),
        Err(_) => fail("the IP service sent an unexpected answer".into()),
    }
}

/// Read one HTTP answer and stop as soon as it is complete. Many proxies keep the tunnel open after the IP service
/// has answered (`Connection: close` notwithstanding), so waiting for the end of the stream would sit out the whole
/// timeout on every check. Complete = `Content-Length` bytes of body, the last chunk of a chunked body, or (no length
/// given) a JSON object whose braces balance.
fn read_answer(s: &mut TcpStream, deadline: Instant) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    while raw.len() < 64 * 1024 {
        if answer_complete(&raw) {
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let _ = s.set_read_timeout(Some(left));
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    raw
}

fn answer_complete(raw: &[u8]) -> bool {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    let body = &raw[end + 4..];
    let header = |name: &str| {
        head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim() == name).map(|(_, v)| v.trim().to_owned()))
    };
    if let Some(n) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        return body.len() >= n;
    }
    if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return body.windows(5).any(|w| w == b"0\r\n\r\n");
    }
    // no length: a whole JSON object is enough
    let mut depth = 0i32;
    let mut seen = false;
    for &b in body {
        match b {
            b'{' => {
                depth += 1;
                seen = true;
            }
            b'}' => depth -= 1,
            _ => {}
        }
        if seen && depth == 0 {
            return true;
        }
    }
    false
}

fn io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => "connection refused".into(),
        TimedOut | WouldBlock => "timed out".into(),
        ConnectionReset | BrokenPipe | UnexpectedEof => "connection closed".into(),
        _ => e.to_string(),
    }
}

fn socks5_connect(s: &mut TcpStream, hop: &ProxyHop, echo: &Echo) -> Result<(), String> {
    let io = |e: std::io::Error| format!("SOCKS5 handshake failed: {}", io_reason(&e));
    let has_login = !hop.account.is_empty();
    s.write_all(if has_login { &[5, 2, 0, 2][..] } else { &[5, 1, 0][..] }).map_err(io)?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).map_err(io)?;
    if reply[0] != 5 {
        return Err("that port does not speak SOCKS5".into());
    }
    match reply[1] {
        0 => {}
        2 if has_login => {
            let (u, p) = (hop.account.as_bytes(), hop.password.as_bytes());
            if u.len() > 255 || p.len() > 255 {
                return Err("the SOCKS5 user name / password is longer than 255 bytes".into());
            }
            let mut msg = vec![1, u.len() as u8];
            msg.extend_from_slice(u);
            msg.push(p.len() as u8);
            msg.extend_from_slice(p);
            s.write_all(&msg).map_err(io)?;
            let mut r = [0u8; 2];
            s.read_exact(&mut r).map_err(io)?;
            if r[1] != 0 {
                return Err("the proxy rejected the user name / password".into());
            }
        }
        2 => return Err("the proxy needs a user name and password".into()),
        0xFF => return Err("the proxy accepts none of our login methods".into()),
        m => return Err(format!("the proxy chose an unknown login method ({m})")),
    }
    let host = echo.host.as_bytes();
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host);
    req.extend_from_slice(&echo.port.to_be_bytes());
    s.write_all(&req).map_err(io)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).map_err(io)?;
    if head[1] != 0 {
        return Err(format!(
            "the proxy could not reach the IP service: {}",
            match head[1] {
                1 => "general failure",
                2 => "connection not allowed",
                3 => "network unreachable",
                4 => "host unreachable",
                5 => "connection refused",
                6 => "TTL expired",
                7 => "command not supported",
                8 => "address type not supported",
                _ => "unknown error",
            }
        ));
    }
    let skip = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).map_err(io)?;
            usize::from(l[0]) + 2
        }
        _ => return Err("the proxy sent a malformed SOCKS5 reply".into()),
    };
    let mut rest = vec![0u8; skip];
    s.read_exact(&mut rest).map_err(io)?;
    Ok(())
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const BODY: &str = r#"{"status":"success","countryCode":"DE","query":"198.51.100.23"}"#;

    fn http_ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    fn hop(scheme: &str, port: u16, user: &str, pass: &str) -> ProxyHop {
        ProxyHop {
            scheme: scheme.into(),
            host: "127.0.0.1".into(),
            port: port.to_string(),
            account: user.into(),
            password: pass.into(),
        }
    }

    fn read_request(s: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut b = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).unwrap_or(0) == 0 {
                break;
            }
            buf.push(b[0]);
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"bob:pa55"), "Ym9iOnBhNTU=");
    }

    #[test]
    fn http_proxy_with_login_reports_ip_and_country() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let req = read_request(&mut s);
            s.write_all(http_ok(BODY).as_bytes()).unwrap();
            req
        });
        let r = check_hop(&hop("http", port, "bob", "pa55"), &Echo::default(), Duration::from_secs(5));
        let req = srv.join().unwrap();
        assert!(req.starts_with("GET http://ip-api.com/json?"), "{req}");
        assert!(req.contains("Proxy-Authorization: Basic Ym9iOnBhNTU="), "{req}");
        assert!(r.ok, "{r:?}");
        assert_eq!((r.ip.as_str(), r.country.as_str()), ("198.51.100.23", "de"));
        assert!(!r.message.contains("pa55"));
    }

    #[test]
    fn http_proxy_407_is_a_clear_login_error() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_request(&mut s);
            s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n").unwrap();
        });
        let r = check_hop(&hop("http", port, "bob", "wrong"), &Echo::default(), Duration::from_secs(5));
        srv.join().unwrap();
        assert!(!r.ok);
        assert!(r.message.contains("407"), "{}", r.message);
        assert!(!r.message.contains("wrong"));
    }

    fn socks5_server(expect_login: Option<(&'static str, &'static str)>) -> (u16, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut g = [0u8; 2];
            s.read_exact(&mut g).unwrap();
            let mut methods = vec![0u8; usize::from(g[1])];
            s.read_exact(&mut methods).unwrap();
            if let Some((u, p)) = expect_login {
                assert!(methods.contains(&2));
                s.write_all(&[5, 2]).unwrap();
                let mut v = [0u8; 2];
                s.read_exact(&mut v).unwrap();
                let mut user = vec![0u8; usize::from(v[1])];
                s.read_exact(&mut user).unwrap();
                let mut pl = [0u8; 1];
                s.read_exact(&mut pl).unwrap();
                let mut pass = vec![0u8; usize::from(pl[0])];
                s.read_exact(&mut pass).unwrap();
                let ok = user == u.as_bytes() && pass == p.as_bytes();
                s.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                if !ok {
                    return String::from("login rejected");
                }
            } else {
                s.write_all(&[5, 0]).unwrap();
            }
            let mut h = [0u8; 5];
            s.read_exact(&mut h).unwrap();
            assert_eq!((h[0], h[1], h[3]), (5, 1, 3));
            let mut name = vec![0u8; usize::from(h[4]) + 2];
            s.read_exact(&mut name).unwrap();
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
            let req = read_request(&mut s);
            s.write_all(http_ok(BODY).as_bytes()).unwrap();
            format!("{}|{req}", String::from_utf8_lossy(&name[..name.len() - 2]))
        });
        (port, h)
    }

    /// A proxy that answers at once but keeps the tunnel open: the check must not wait for it to close.
    #[test]
    fn a_proxy_that_keeps_the_connection_open_does_not_slow_the_check() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_request(&mut s);
            s.write_all(http_ok(BODY).as_bytes()).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10)); // hold the connection open
        });
        let t = Instant::now();
        let r = check_hop(&hop("http", port, "", ""), &Echo::default(), Duration::from_secs(8));
        assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
        assert!(r.ok, "{r:?}");
        assert_eq!((r.ip.as_str(), r.country.as_str()), ("198.51.100.23", "de"));
        let _ = tx.send(());
        srv.join().unwrap();
    }

    #[test]
    fn complete_answers_are_recognised() {
        let ok = |b: &str| answer_complete(b.as_bytes());
        assert!(ok("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"));
        assert!(!ok("HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"a\""));
        assert!(ok("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"));
        assert!(ok("HTTP/1.1 200 OK\r\n\r\n{\"a\":{\"b\":1}}"));
        assert!(!ok("HTTP/1.1 200 OK\r\n\r\n{\"a\":{\"b\":1}"));
        assert!(!ok("HTTP/1.1 200 OK\r\nContent-Le"));
    }

    #[test]
    fn socks5_with_login_tunnels_to_the_echo_service() {
        let (port, srv) = socks5_server(Some(("bob", "pa55")));
        let r = check_hop(&hop("socks5", port, "bob", "pa55"), &Echo::default(), Duration::from_secs(5));
        let seen = srv.join().unwrap();
        assert!(seen.starts_with("ip-api.com|GET /json?"), "{seen}");
        assert!(r.ok, "{r:?}");
        assert_eq!(r.ip, "198.51.100.23");
    }

    #[test]
    fn socks5_wrong_password_is_reported_without_echoing_it() {
        let (port, srv) = socks5_server(Some(("bob", "pa55")));
        let r = check_hop(&hop("socks5", port, "bob", "nope"), &Echo::default(), Duration::from_secs(5));
        assert_eq!(srv.join().unwrap(), "login rejected");
        assert!(!r.ok);
        assert!(r.message.contains("rejected"), "{}", r.message);
        assert!(!r.message.contains("nope"));
    }

    #[test]
    fn socks5_without_login() {
        let (port, srv) = socks5_server(None);
        let r = check_hop(&hop("socks5", port, "", ""), &Echo::default(), Duration::from_secs(5));
        srv.join().unwrap();
        assert!(r.ok, "{r:?}");
    }

    #[test]
    fn a_closed_port_is_reported_as_refused() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let r = check_hop(&hop("http", port, "", ""), &Echo::default(), Duration::from_secs(2));
        assert!(!r.ok);
        assert!(r.message.contains("refused"), "{}", r.message);
    }

    #[test]
    fn https_and_socks4_are_reachability_only() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let r = check_hop(&hop("socks4", port, "", ""), &Echo::default(), Duration::from_secs(2));
        assert!(r.ok && r.ip.is_empty());
        assert!(r.message.contains("reachability"), "{}", r.message);
    }

    #[test]
    fn direct_has_nothing_to_check() {
        assert!(matches!(check(&ProxyConfig::default(), Duration::from_secs(1)), Err(BackendError::Invalid(_))));
    }
}
