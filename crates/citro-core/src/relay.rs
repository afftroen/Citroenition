//! A local proxy relay for kernels that cannot log in to a proxy.
//!
//! Stock Chromium cannot send a SOCKS5 username and password at all, and asks for an HTTP proxy's login in a dialog.
//! Clearcote's Linux builds add `--socks5-credentials` / `--proxy-auth`; its macOS build (150) has neither, so a
//! proxy that needs a login fails there with `ERR_SOCKS_CONNECTION_FAILED`.
//!
//! For such a kernel the launcher starts this relay first: a SOCKS5 server on `127.0.0.1:<random port>` without a
//! login, which opens every connection through the real proxy with the credentials (SOCKS5 with RFC 1929
//! username/password, or HTTP `CONNECT` with `Proxy-Authorization: Basic`). The browser gets
//! `--proxy-server=socks5://127.0.0.1:<port>`. Host names travel to the upstream proxy unresolved, as with a direct
//! `socks5://` proxy (Chromium's `socks5` resolves remotely), so DNS does not leak.
//!
//! The relay is the launching program itself, started again with [`RELAY_ARG`] (every program that launches
//! browsers calls [`run_if_requested`] first thing in `main`). It reads its upstream from stdin (never argv: a command
//! line is readable by every local user), prints its port, then the browser's pid, and exits when that browser has.
//! It runs in its own process group, so quitting the app does not cut an open browser off.

use crate::kernel_env::base64;
use crate::profile::ProxyHop;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// `argv[1]` that turns a Citroenition program into a relay.
pub const RELAY_ARG: &str = "__citro-proxy-relay";
/// Points at the program to run as the relay (tests, unusual installs); default: the running program.
pub const RELAY_EXE_ENV: &str = "CITRO_RELAY_EXE";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// The relay gives up when it is not told which browser to follow.
const PID_TIMEOUT: Duration = Duration::from_secs(120);

/// The proxy the relay connects through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    /// `socks5` or `http`.
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl Upstream {
    /// The upstream for `hop` when the relay can carry it (`socks5` and `http` with a login).
    pub fn for_hop(hop: &ProxyHop) -> Option<Upstream> {
        let port = hop.port.parse().ok()?;
        (matches!(hop.scheme.as_str(), "socks5" | "http") && !hop.account.is_empty()).then(|| Upstream {
            scheme: hop.scheme.clone(),
            host: hop.host.clone(),
            port,
            user: hop.account.clone(),
            password: hop.password.clone(),
        })
    }
}

/// A started relay: the browser is pointed at `port`, then [`Relay::follow`] tells it which browser to outlive.
pub struct Relay {
    pub port: u16,
    child: Child,
}

impl Relay {
    pub fn start(upstream: &Upstream, log: std::fs::File) -> io::Result<Relay> {
        let exe = match std::env::var_os(RELAY_EXE_ENV) {
            Some(p) => p.into(),
            None => std::env::current_exe()?,
        };
        let mut cmd = Command::new(exe);
        use std::os::unix::process::CommandExt;
        let mut child = cmd
            .arg(RELAY_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .process_group(0)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        writeln!(stdin, "{}", serde_json::to_string(upstream).map_err(io::Error::other)?)?;
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("piped")).read_line(&mut line)?;
        let port = match line.trim().parse::<u16>() {
            Ok(p) if p != 0 => p,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other(format!("the proxy relay did not start (it said {:?})", line.trim())));
            }
        };
        child.stdin = Some(stdin);
        Ok(Relay { port, child })
    }

    /// Tell the relay the browser's pid; it exits once that process is gone. Reaps the relay in the background.
    pub fn follow(mut self, pid: u32) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = writeln!(stdin, "{pid}");
        }
        let _ = std::thread::Builder::new().name("relay-reap".into()).spawn(move || {
            let _ = self.child.wait();
        });
    }

    /// The browser never came up: stop the relay now.
    pub fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// When `argv[1]` is [`RELAY_ARG`], run the relay and exit; otherwise return at once. Call first thing in `main`.
pub fn run_if_requested() {
    if std::env::args_os().nth(1).is_some_and(|a| a == RELAY_ARG) {
        let code = match relay_main() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("citro proxy relay: {e}");
                1
            }
        };
        std::process::exit(code);
    }
}

fn relay_main() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin());
    let mut line = String::new();
    input.read_line(&mut line)?;
    let upstream: Upstream = serde_json::from_str(line.trim()).map_err(io::Error::other)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    println!("{port}");
    io::stdout().flush()?;

    // the browser's pid arrives once it is started; without one the relay does not linger
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut l = String::new();
        let _ = input.read_line(&mut l);
        let _ = tx.send(l.trim().parse::<i32>().ok());
    });
    std::thread::spawn(move || serve(listener, upstream));
    let pid = rx.recv_timeout(PID_TIMEOUT).ok().flatten().filter(|p| *p > 0);
    let Some(pid) = pid else { return Ok(()) };
    eprintln!("citro proxy relay: 127.0.0.1:{port} for browser {pid}");
    while alive(pid) {
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Accept connections forever (the process ends when the browser does).
pub fn serve(listener: TcpListener, upstream: Upstream) {
    for conn in listener.incoming().flatten() {
        let up = upstream.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(conn, &up) {
                eprintln!("citro proxy relay: {e}");
            }
        });
    }
}

/// Where the browser wants to go.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Ip4(Ipv4Addr),
    Ip6(Ipv6Addr),
    Name(String),
}

fn read_exact<const N: usize>(s: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    s.read_exact(&mut b)?;
    Ok(b)
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// The browser's side: SOCKS5 without authentication, CONNECT only.
fn accept_socks5(c: &mut TcpStream) -> io::Result<(Target, u16)> {
    let [ver, n] = read_exact::<2>(c)?;
    if ver != 5 {
        return Err(bad("not a SOCKS5 client"));
    }
    let mut methods = vec![0u8; n as usize];
    c.read_exact(&mut methods)?;
    if !methods.contains(&0) {
        c.write_all(&[5, 0xff])?;
        return Err(bad("the client offers no no-auth method"));
    }
    c.write_all(&[5, 0])?;
    let [_, cmd, _, atyp] = read_exact::<4>(c)?;
    let target = match atyp {
        1 => Target::Ip4(Ipv4Addr::from(read_exact::<4>(c)?)),
        4 => Target::Ip6(Ipv6Addr::from(read_exact::<16>(c)?)),
        3 => {
            let [len] = read_exact::<1>(c)?;
            let mut name = vec![0u8; len as usize];
            c.read_exact(&mut name)?;
            Target::Name(String::from_utf8(name).map_err(|_| bad("host name is not UTF-8"))?)
        }
        _ => return Err(bad("unknown address type")),
    };
    let port = u16::from_be_bytes(read_exact::<2>(c)?);
    if cmd != 1 {
        reply(c, 7)?;
        return Err(bad("only CONNECT is supported"));
    }
    Ok((target, port))
}

fn reply(c: &mut TcpStream, code: u8) -> io::Result<()> {
    c.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
}

fn connect_upstream(up: &Upstream) -> io::Result<TcpStream> {
    let mut last = io::Error::other("no address");
    for addr in (up.host.as_str(), up.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Open a tunnel to `target:port` through the upstream proxy, logging in.
fn dial(up: &Upstream, target: &Target, port: u16) -> io::Result<TcpStream> {
    let mut s = connect_upstream(up)?;
    s.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    s.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    match up.scheme.as_str() {
        "socks5" => socks5_handshake(&mut s, up, target, port)?,
        "http" => http_connect(&mut s, up, target, port)?,
        other => return Err(bad(format!("the relay cannot speak to a {other} proxy"))),
    }
    s.set_read_timeout(None)?;
    s.set_write_timeout(None)?;
    Ok(s)
}

fn socks5_handshake(s: &mut TcpStream, up: &Upstream, target: &Target, port: u16) -> io::Result<()> {
    s.write_all(&[5, 2, 0, 2])?; // offer: no auth, username/password
    match read_exact::<2>(s)? {
        [5, 0] => {}
        [5, 2] => {
            let (u, p) = (up.user.as_bytes(), up.password.as_bytes());
            if u.len() > 255 || p.len() > 255 {
                return Err(bad("proxy username or password longer than 255 bytes"));
            }
            let mut auth = vec![1, u.len() as u8];
            auth.extend_from_slice(u);
            auth.push(p.len() as u8);
            auth.extend_from_slice(p);
            s.write_all(&auth)?;
            if read_exact::<2>(s)?[1] != 0 {
                return Err(bad("the proxy refused the username or password"));
            }
        }
        _ => return Err(bad("the proxy accepts none of our login methods")),
    }
    let mut req = vec![5, 1, 0];
    match target {
        Target::Ip4(a) => {
            req.push(1);
            req.extend_from_slice(&a.octets());
        }
        Target::Ip6(a) => {
            req.push(4);
            req.extend_from_slice(&a.octets());
        }
        Target::Name(n) => {
            if n.len() > 255 {
                return Err(bad("host name too long"));
            }
            req.push(3);
            req.push(n.len() as u8);
            req.extend_from_slice(n.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req)?;
    let [_, rep, _, atyp] = read_exact::<4>(s)?;
    // skip the bound address
    let skip = match atyp {
        1 => 4,
        4 => 16,
        3 => read_exact::<1>(s)?[0] as usize,
        _ => return Err(bad("bad reply from the proxy")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest)?;
    if rep != 0 {
        return Err(io::Error::other(format!("the proxy could not connect (SOCKS5 reply {rep})")));
    }
    Ok(())
}

fn http_connect(s: &mut TcpStream, up: &Upstream, target: &Target, port: u16) -> io::Result<()> {
    let host = match target {
        Target::Ip4(a) => a.to_string(),
        Target::Ip6(a) => format!("[{a}]"),
        Target::Name(n) => n.clone(),
    };
    let auth = base64(format!("{}:{}", up.user, up.password).as_bytes());
    write!(s, "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nProxy-Authorization: Basic {auth}\r\n\r\n")?;
    // read the response head byte by byte so nothing of the tunnel is consumed
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b)? == 0 || head.len() > 16 * 1024 {
            return Err(bad("the proxy closed the connection"));
        }
        head.push(b[0]);
    }
    let status = String::from_utf8_lossy(&head).split_whitespace().nth(1).unwrap_or("").to_owned();
    if status != "200" {
        return Err(io::Error::other(format!("the proxy answered HTTP {status} to CONNECT")));
    }
    Ok(())
}

fn handle(mut client: TcpStream, up: &Upstream) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let (target, port) = accept_socks5(&mut client)?;
    let upstream = match dial(up, &target, port) {
        Ok(s) => s,
        Err(e) => {
            let _ = reply(&mut client, 5); // connection refused
            return Err(e);
        }
    };
    reply(&mut client, 0)?;
    client.set_read_timeout(None)?;
    pipe(client, upstream)
}

/// Copy both ways until either side closes.
fn pipe(a: TcpStream, b: TcpStream) -> io::Result<()> {
    let (mut a2, mut b2) = (a.try_clone()?, b.try_clone()?);
    let t = std::thread::spawn(move || {
        let _ = io::copy(&mut a2, &mut b2);
        let _ = b2.shutdown(Shutdown::Write);
    });
    let (mut a, mut b) = (a, b);
    let _ = io::copy(&mut b, &mut a);
    let _ = a.shutdown(Shutdown::Write);
    let _ = t.join();
    Ok(())
}

/// Wait until something listens on `127.0.0.1:port` (tests).
#[doc(hidden)]
pub fn wait_listening(port: u16, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny upstream SOCKS5 proxy that demands `user`/`pass` and records the requested target, then echoes.
    fn fake_socks5_upstream() -> (u16, std::sync::mpsc::Receiver<(String, u16)>) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let [_, n] = read_exact::<2>(&mut c).unwrap();
                    let mut m = vec![0; n as usize];
                    c.read_exact(&mut m).unwrap();
                    assert!(m.contains(&2));
                    c.write_all(&[5, 2]).unwrap();
                    let [_, ul] = read_exact::<2>(&mut c).unwrap();
                    let mut u = vec![0; ul as usize];
                    c.read_exact(&mut u).unwrap();
                    let [pl] = read_exact::<1>(&mut c).unwrap();
                    let mut p = vec![0; pl as usize];
                    c.read_exact(&mut p).unwrap();
                    let ok = u == b"user" && p == b"p@ss:word";
                    c.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                    if !ok {
                        return;
                    }
                    let [_, _, _, atyp] = read_exact::<4>(&mut c).unwrap();
                    assert_eq!(atyp, 3, "host names reach the upstream unresolved");
                    let [len] = read_exact::<1>(&mut c).unwrap();
                    let mut name = vec![0; len as usize];
                    c.read_exact(&mut name).unwrap();
                    let port = u16::from_be_bytes(read_exact::<2>(&mut c).unwrap());
                    tx.send((String::from_utf8(name).unwrap(), port)).unwrap();
                    c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
                    let mut buf = [0u8; 5];
                    c.read_exact(&mut buf).unwrap();
                    c.write_all(&buf).unwrap();
                });
            }
        });
        (port, rx)
    }

    fn fake_http_upstream() -> (u16, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    c.read_exact(&mut b).unwrap();
                    head.push(b[0]);
                }
                tx.send(String::from_utf8(head).unwrap()).unwrap();
                c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
                let mut buf = [0u8; 5];
                c.read_exact(&mut buf).unwrap();
                c.write_all(&buf).unwrap();
            }
        });
        (port, rx)
    }

    fn relay_for(up: Upstream) -> u16 {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || serve(l, up));
        port
    }

    /// What a browser does: SOCKS5 without a login, CONNECT by host name, then talk.
    fn browse(relay: u16, host: &str, port: u16) -> Result<[u8; 5], u8> {
        let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, relay)).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        assert_eq!(read_exact::<2>(&mut c).unwrap(), [5, 0]);
        let mut req = vec![5, 1, 0, 3, host.len() as u8];
        req.extend_from_slice(host.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        c.write_all(&req).unwrap();
        let r = read_exact::<10>(&mut c).unwrap();
        if r[1] != 0 {
            return Err(r[1]);
        }
        c.write_all(b"hello").unwrap();
        Ok(read_exact::<5>(&mut c).unwrap())
    }

    fn up(scheme: &str, port: u16, password: &str) -> Upstream {
        Upstream {
            scheme: scheme.into(),
            host: "127.0.0.1".into(),
            port,
            user: "user".into(),
            password: password.into(),
        }
    }

    #[test]
    fn socks5_upstream_gets_the_login_and_the_unresolved_name() {
        let (port, seen) = fake_socks5_upstream();
        let relay = relay_for(up("socks5", port, "p@ss:word"));
        assert_eq!(browse(relay, "iphey.com", 443), Ok(*b"hello"));
        assert_eq!(seen.recv().unwrap(), ("iphey.com".to_owned(), 443));
    }

    #[test]
    fn a_wrong_password_is_a_refused_connection_for_the_browser() {
        let (port, _) = fake_socks5_upstream();
        let relay = relay_for(up("socks5", port, "wrong"));
        assert_eq!(browse(relay, "iphey.com", 443), Err(5));
    }

    #[test]
    fn http_upstream_gets_connect_with_basic_auth() {
        let (port, seen) = fake_http_upstream();
        let relay = relay_for(up("http", port, "p@ss:word"));
        assert_eq!(browse(relay, "example.com", 443), Ok(*b"hello"));
        let head = seen.recv().unwrap();
        assert!(head.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("Proxy-Authorization: Basic {}", base64(b"user:p@ss:word"))), "{head}");
    }

    #[test]
    fn only_logins_the_relay_can_carry_get_one() {
        let hop = |scheme: &str, account: &str| ProxyHop {
            scheme: scheme.into(),
            host: "h".into(),
            port: "1080".into(),
            account: account.into(),
            password: "p".into(),
        };
        assert!(Upstream::for_hop(&hop("socks5", "u")).is_some());
        assert!(Upstream::for_hop(&hop("http", "u")).is_some());
        assert!(Upstream::for_hop(&hop("socks5", "")).is_none(), "no login: the browser connects itself");
        assert!(Upstream::for_hop(&hop("https", "u")).is_none(), "TLS to the proxy is not the relay's");
    }
}
