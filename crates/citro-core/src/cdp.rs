//! A minimal client for the browser's DevTools endpoint (one WebSocket, request/response by id). Only what the
//! launcher needs, so no async runtime or WebSocket crate: RFC 6455 text frames over a blocking `TcpStream` to
//! `127.0.0.1`. No `Origin` header is sent: Chromium refuses DevTools connections from origins it was not told about.

use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

pub struct Cdp {
    stream: BufReader<TcpStream>,
    next_id: u64,
}

fn bad(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

impl Cdp {
    /// Connect to `ws://127.0.0.1:<port>/<path>`.
    pub fn connect(url: &str) -> io::Result<Cdp> {
        let rest = url.strip_prefix("ws://").ok_or_else(|| bad("not a ws:// URL"))?;
        let (hostport, path) = rest.split_once('/').map_or((rest, "/".to_owned()), |(h, p)| (h, format!("/{p}")));
        let tcp = TcpStream::connect(hostport)?;
        tcp.set_read_timeout(Some(TIMEOUT))?;
        tcp.set_write_timeout(Some(TIMEOUT))?;
        let mut stream = BufReader::new(tcp);
        let key = crate::kernel_env::base64(&std::process::id().to_be_bytes().repeat(4));
        write!(
            stream.get_mut(),
            "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut status = String::new();
        stream.read_line(&mut status)?;
        if !status.contains(" 101 ") {
            return Err(bad(format!("DevTools refused the connection: {}", status.trim())));
        }
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
        }
        Ok(Cdp { stream, next_id: 1 })
    }

    fn send(&mut self, text: &str) -> io::Result<()> {
        let data = text.as_bytes();
        let mut frame = vec![0x81u8]; // FIN + text
        match data.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n <= u16::MAX as usize => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        // a client must mask; the key does not need to be secret here
        let mask = (self.next_id as u32 ^ 0x5a17_c0de).to_be_bytes();
        frame.extend_from_slice(&mask);
        frame.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.get_mut().write_all(&frame)
    }

    /// The next complete text message (fragments joined; pings answered; other frames skipped).
    fn recv(&mut self) -> io::Result<String> {
        let mut msg = Vec::new();
        loop {
            let mut h = [0u8; 2];
            self.stream.read_exact(&mut h)?;
            let (fin, op) = (h[0] & 0x80 != 0, h[0] & 0x0f);
            let mut len = u64::from(h[1] & 0x7f);
            if len == 126 {
                let mut b = [0u8; 2];
                self.stream.read_exact(&mut b)?;
                len = u64::from(u16::from_be_bytes(b));
            } else if len == 127 {
                let mut b = [0u8; 8];
                self.stream.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if len > 64 << 20 {
                return Err(bad("DevTools message too large"));
            }
            let mut payload = vec![0u8; len as usize];
            self.stream.read_exact(&mut payload)?;
            match op {
                0x8 => return Err(bad("DevTools closed the connection")),
                0x9 => {
                    // pong with the same payload (masked, as a client)
                    let mut f = vec![0x8a, 0x80 | payload.len() as u8, 0, 0, 0, 0];
                    f.extend_from_slice(&payload);
                    self.stream.get_mut().write_all(&f)?;
                }
                0x0..=0x2 => {
                    msg.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(msg).map_err(|_| bad("DevTools sent invalid UTF-8"));
                    }
                }
                _ => {}
            }
        }
    }

    /// Call `method` and return its `result`, or the error DevTools gave.
    pub fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "id": id, "method": method, "params": params }).to_string())?;
        loop {
            let v: Value = serde_json::from_str(&self.recv()?).map_err(|e| bad(e.to_string()))?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(e) = v.get("error") {
                    return Err(io::Error::other(
                        e.get("message").and_then(Value::as_str).unwrap_or("error").to_owned(),
                    ));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot fake DevTools server: completes the handshake, unmasks one request, answers it (fragmented).
    #[test]
    fn a_call_round_trips_through_a_websocket() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(!head.to_ascii_lowercase().contains("origin:"), "no Origin header");
            r.get_mut()
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .unwrap();
            let mut h = [0u8; 2];
            r.read_exact(&mut h).unwrap();
            let len = (h[1] & 0x7f) as usize;
            let mut mask = [0u8; 4];
            r.read_exact(&mut mask).unwrap();
            let mut p = vec![0u8; len];
            r.read_exact(&mut p).unwrap();
            let text: String = p.iter().enumerate().map(|(i, b)| (b ^ mask[i % 4]) as char).collect();
            let req: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(req["method"], "Storage.setCookies");
            // an unrelated event first, then the answer in two fragments
            let event = br#"{"method":"Target.targetCreated","params":{}}"#;
            let mut f = vec![0x81, event.len() as u8];
            f.extend_from_slice(event);
            let answer = format!(r#"{{"id":{},"result":{{"ok":true}}}}"#, req["id"]);
            let (a, b) = answer.as_bytes().split_at(5);
            f.extend_from_slice(&[0x01, a.len() as u8]);
            f.extend_from_slice(a);
            f.extend_from_slice(&[0x80, b.len() as u8]);
            f.extend_from_slice(b);
            r.get_mut().write_all(&f).unwrap();
        });
        let mut c = Cdp::connect(&format!("ws://127.0.0.1:{port}/devtools/browser/x")).unwrap();
        let r = c.call("Storage.setCookies", json!({"cookies": []})).unwrap();
        assert_eq!(r["ok"], true);
        server.join().unwrap();
    }
}
