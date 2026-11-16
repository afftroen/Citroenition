//! Test support, shared by the unit tests (`#[path]` in `lib.rs`) and the integration tests (`mod common;`):
//! a throw-away OpenPGP key, honest and hostile archives, and a tiny HTTP server.
//!
//! Everything here is offline and deterministic enough to run in parallel: servers bind to port 0 of 127.0.0.1.
#![allow(dead_code)]

use pgp::composed::{ArmorOptions, DetachedSignature, KeyType, SecretKeyParamsBuilder, SignedPublicKey, SignedSecretKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::ser::Serialize;
use pgp::types::{KeyDetails, Password};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------------------------------------------------------
// OpenPGP
// ---------------------------------------------------------------------------------------------------------------

/// A freshly generated signing key that nothing trusts, unless a test hands its public half to the code under test.
pub struct TestKey {
    pub secret: SignedSecretKey,
    pub public: SignedPublicKey,
}

impl TestKey {
    pub fn generate() -> TestKey {
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Citro Test <test@invalid>".into());
        let secret = params.build().unwrap().generate(rand::thread_rng()).unwrap();
        let public = SignedPublicKey::from(secret.clone());
        TestKey { secret, public }
    }

    pub fn fingerprint(&self) -> String {
        self.public.fingerprint().to_string()
    }

    /// ASCII-armored detached signature over `data` (SHA-256), like `gpg --armor --detach-sign`.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.sign_with(data, HashAlgorithm::Sha256)
    }

    pub fn sign_with(&self, data: &[u8], hash: HashAlgorithm) -> Vec<u8> {
        self.detached(data, hash).to_armored_bytes(ArmorOptions::default()).unwrap()
    }

    /// The same, as raw packets (what `gpg --detach-sign` writes to a `.sig` file).
    pub fn sign_binary(&self, data: &[u8]) -> Vec<u8> {
        self.detached(data, HashAlgorithm::Sha256).to_bytes().unwrap()
    }

    fn detached(&self, data: &[u8], hash: HashAlgorithm) -> DetachedSignature {
        DetachedSignature::sign_binary_data(
            rand::thread_rng(),
            &self.secret.primary_key,
            &Password::empty(),
            hash,
            data,
        )
        .unwrap()
    }
}

/// Flip the lowest bit of the byte at `index`.
pub fn flip(bytes: &[u8], index: usize) -> Vec<u8> {
    let mut v = bytes.to_vec();
    v[index] ^= 1;
    v
}

/// The index of a byte in the middle of an armored signature's base64 body: flipping it changes the signature.
pub fn armor_body_index(armored: &[u8]) -> usize {
    let text = std::str::from_utf8(armored).unwrap();
    let start = text.find("\n\n").expect("armor headers end with a blank line") + 2;
    let end = text[start..].find("\n=").expect("armor has a CRC line") + start;
    start + (end - start) / 2
}

// ---------------------------------------------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------------------------------------------

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// `sha256sum` output for the given files.
pub fn sums_text(files: &[(&str, &[u8])]) -> Vec<u8> {
    files.iter().map(|(name, data)| format!("{}  {name}\n", sha256_hex(data))).collect::<String>().into_bytes()
}

// ---------------------------------------------------------------------------------------------------------------
// tar / xz / zip
// ---------------------------------------------------------------------------------------------------------------

/// One tar member. The writer below does no validation at all, so hostile archives are as easy as honest ones.
#[derive(Clone)]
pub struct Entry {
    pub name: Vec<u8>,
    /// tar type flag: `0` file, `1` hard link, `2` symlink, `3` char device, `4` block device, `5` dir, `6` fifo
    pub kind: u8,
    pub mode: u32,
    pub link: Vec<u8>,
    pub data: Vec<u8>,
    pub declared_size: Option<u64>,
}

impl Entry {
    fn new(name: &str, kind: u8, mode: u32) -> Entry {
        Entry { name: name.as_bytes().to_vec(), kind, mode, link: Vec::new(), data: Vec::new(), declared_size: None }
    }
    pub fn file(name: &str, data: &[u8]) -> Entry {
        Entry { data: data.to_vec(), ..Entry::new(name, b'0', 0o644) }
    }
    pub fn exec(name: &str, data: &[u8]) -> Entry {
        Entry { data: data.to_vec(), ..Entry::new(name, b'0', 0o755) }
    }
    pub fn dir(name: &str) -> Entry {
        Entry::new(name, b'5', 0o755)
    }
    pub fn symlink(name: &str, target: &str) -> Entry {
        Entry { link: target.as_bytes().to_vec(), ..Entry::new(name, b'2', 0o777) }
    }
    pub fn hardlink(name: &str, target: &str) -> Entry {
        Entry { link: target.as_bytes().to_vec(), ..Entry::new(name, b'1', 0o644) }
    }
    pub fn special(name: &str, kind: u8) -> Entry {
        Entry::new(name, kind, 0o666)
    }
    pub fn mode(mut self, mode: u32) -> Entry {
        self.mode = mode;
        self
    }
    pub fn name_bytes(mut self, name: &[u8]) -> Entry {
        self.name = name.to_vec();
        self
    }
    /// What the header claims, whatever the data really is.
    pub fn declared_size(mut self, size: u64) -> Entry {
        self.declared_size = Some(size);
        self
    }
}

fn octal(field: &mut [u8], value: u64) {
    let digits = format!("{value:0width$o}", width = field.len() - 1);
    assert!(digits.len() < field.len(), "{value} does not fit in {} octal digits", field.len() - 1);
    field[..digits.len()].copy_from_slice(digits.as_bytes());
    field[digits.len()] = 0;
}

/// The 512-byte ustar header of `e`.
pub fn tar_header(e: &Entry) -> [u8; 512] {
    let mut h = [0u8; 512];
    assert!(e.name.len() <= 100 && e.link.len() <= 100, "test names must fit a plain ustar header");
    h[..e.name.len()].copy_from_slice(&e.name);
    octal(&mut h[100..108], u64::from(e.mode));
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], e.declared_size.unwrap_or(e.data.len() as u64));
    octal(&mut h[136..148], 0);
    h[148..156].fill(b' ');
    h[156] = e.kind;
    h[157..157 + e.link.len()].copy_from_slice(&e.link);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..154].copy_from_slice(format!("{sum:06o}").as_bytes());
    h[154] = 0;
    h[155] = b' ';
    h
}

pub fn tar(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(&tar_header(e));
        out.extend_from_slice(&e.data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

pub fn xz(data: &[u8]) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap()
}

pub fn tar_xz(entries: &[Entry]) -> Vec<u8> {
    xz(&tar(entries))
}

/// A tar stream made of `count` copies of one header (and nothing else): `count` entries cost no memory.
pub struct RepeatedHeaders {
    header: [u8; 512],
    left: u64,
    offset: usize,
    trailer: usize,
}

impl RepeatedHeaders {
    pub fn new(entry: &Entry, count: u64) -> RepeatedHeaders {
        RepeatedHeaders { header: tar_header(entry), left: count, offset: 0, trailer: 1024 }
    }
}

impl Read for RepeatedHeaders {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            if self.left > 0 {
                let take = (512 - self.offset).min(buf.len() - n);
                buf[n..n + take].copy_from_slice(&self.header[self.offset..self.offset + take]);
                n += take;
                self.offset += take;
                if self.offset == 512 {
                    self.offset = 0;
                    self.left -= 1;
                }
            } else if self.trailer > 0 {
                let take = self.trailer.min(buf.len() - n);
                buf[n..n + take].fill(0);
                n += take;
                self.trailer -= take;
            } else {
                break;
            }
        }
        Ok(n)
    }
}

/// A zip member for [`zip`].
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub mode: Option<u32>,
    pub kind: ZipKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ZipKind {
    File,
    Dir,
    Symlink,
}

impl ZipEntry {
    pub fn file(name: &str, data: &[u8]) -> ZipEntry {
        ZipEntry { name: name.to_owned(), data: data.to_vec(), mode: Some(0o644), kind: ZipKind::File }
    }
    pub fn exec(name: &str, data: &[u8]) -> ZipEntry {
        ZipEntry { mode: Some(0o755), ..ZipEntry::file(name, data) }
    }
    pub fn dir(name: &str) -> ZipEntry {
        ZipEntry { name: name.to_owned(), data: Vec::new(), mode: Some(0o755), kind: ZipKind::Dir }
    }
    pub fn symlink(name: &str, target: &str) -> ZipEntry {
        ZipEntry { data: target.as_bytes().to_vec(), kind: ZipKind::Symlink, ..ZipEntry::file(name, b"") }
    }
    pub fn no_mode(mut self) -> ZipEntry {
        self.mode = None;
        self
    }
}

pub fn zip(entries: &[ZipEntry]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for e in entries {
        let mut options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(mode) = e.mode {
            options = options.unix_permissions(mode);
        }
        match e.kind {
            ZipKind::File => {
                w.start_file(e.name.as_str(), options).unwrap();
                w.write_all(&e.data).unwrap();
            }
            ZipKind::Dir => w.add_directory(e.name.as_str(), options).unwrap(),
            ZipKind::Symlink => w.add_symlink(e.name.as_str(), String::from_utf8_lossy(&e.data), options).unwrap(),
        }
    }
    w.finish().unwrap().into_inner()
}

/// What a real kernel archive looks like: flat, entries directly at the root, `./` in front of every name.
pub fn kernel_entries(binary: &str) -> Vec<Entry> {
    vec![
        Entry::dir("./"),
        Entry::exec(&format!("./{binary}"), b"#!/bin/sh\necho 151.0.7922.108\n"),
        Entry::dir("./fonts/"),
        Entry::file("./fonts/a.ttf", b"font"),
        Entry::dir("./locales/"),
        Entry::file("./locales/en-US.pak", b"pak"),
    ]
}

// ---------------------------------------------------------------------------------------------------------------
// A mirror: <base>/SHA256SUMS.txt, <base>/SHA256SUMS.txt.asc, <base>/<archive>
// ---------------------------------------------------------------------------------------------------------------

/// The three files of a synthetic release. Fields are public so tests can damage them before serving.
#[derive(Clone)]
pub struct Mirror {
    pub archive_name: String,
    pub archive: Vec<u8>,
    pub sums: Vec<u8>,
    pub sig: Vec<u8>,
}

impl Mirror {
    /// `archive` published as `archive_name` and listed (with its true digest, next to an unrelated file) in a
    /// checksum list signed by `key`.
    pub fn signed(key: &TestKey, archive_name: &str, archive: Vec<u8>) -> Mirror {
        let sums = sums_text(&[(archive_name, &archive), ("chrome", b"unrelated")]);
        let sig = key.sign(&sums);
        Mirror { archive_name: archive_name.to_owned(), archive, sums, sig }
    }

    pub fn routes(&self) -> Vec<(String, Body)> {
        vec![
            ("/SHA256SUMS.txt".to_owned(), Body::Ok(self.sums.clone())),
            ("/SHA256SUMS.txt.asc".to_owned(), Body::Ok(self.sig.clone())),
            (format!("/{}", self.archive_name), Body::Ok(self.archive.clone())),
        ]
    }

    pub fn archive_path(&self) -> String {
        format!("/{}", self.archive_name)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------------------------------------------------------

/// What a route answers.
#[derive(Clone)]
pub enum Body {
    /// `200` with exactly these bytes.
    Ok(Vec<u8>),
    /// `200` that announces `announce` bytes, sends `bytes`, then drops the connection (a cut-off download).
    Short { bytes: Vec<u8>, announce: usize },
    /// An empty answer with this status.
    Status(u16),
}

/// A request as the server saw it; header names are lowercased.
#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// A one-purpose HTTP/1.1 server on 127.0.0.1:<free port>, one thread per connection, `Connection: close`.
/// Dropping it stops the accept loop.
pub struct Server {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(routes: Vec<(String, Body)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (routes, requests, stop) = (Arc::clone(&routes), Arc::clone(&requests), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (routes, requests) = (Arc::clone(&routes), Arc::clone(&requests));
                    std::thread::spawn(move || serve(stream, &routes, &requests));
                }
            })
        };
        Server { base: format!("http://{addr}"), addr, requests, stop, thread: Some(thread) }
    }

    pub fn mirror(mirror: &Mirror) -> Server {
        Server::start(mirror.routes())
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// Request targets (path and query) in arrival order.
    pub fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.path).collect()
    }

    pub fn hits(&self, path: &str) -> usize {
        self.paths().iter().filter(|p| p.as_str() == path).count()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr); // wake the accept loop
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, routes: &[(String, Body)], requests: &Mutex<Vec<Request>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
        if head.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines.next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_owned();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    requests.lock().unwrap().push(Request { path: path.clone(), headers });

    let route = routes.iter().find(|(p, _)| *p == path).map(|(_, b)| b);
    let (status, announce, bytes): (&str, usize, &[u8]) = match route {
        Some(Body::Ok(bytes)) => ("200 OK", bytes.len(), bytes),
        Some(Body::Short { bytes, announce }) => ("200 OK", *announce, bytes),
        Some(Body::Status(code)) => (status_line(*code), 0, &[]),
        None => ("404 Not Found", 0, &[]),
    };
    let reply = format!("HTTP/1.1 {status}\r\nContent-Length: {announce}\r\nConnection: close\r\n\r\n");
    if stream.write_all(reply.as_bytes()).is_err() {
        return;
    }
    for piece in bytes.chunks(64 * 1024) {
        if stream.write_all(piece).is_err() {
            return;
        }
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn status_line(code: u16) -> &'static str {
    match code {
        403 => "403 Forbidden",
        404 => "404 Not Found",
        429 => "429 Too Many Requests",
        500 => "500 Internal Server Error",
        _ => "418 I'm a teapot",
    }
}
