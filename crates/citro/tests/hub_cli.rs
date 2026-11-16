//! The team promise through the real `citro` binary: a real hub in this process, two "machines" (a home directory
//! each) driven only by command lines and by the Local API, and a stand-in kernel (a shell script that announces a
//! listening port the way Chromium does and idles).
//!
//! What is checked: a profile that is open on one device cannot be opened on another one, whoever asks and
//! whichever way (command line or API); closing uploads the data and hands the lock back; the data follows the
//! profile; Ctrl-C closes properly; the team's profiles are not deleted by accident.
//!
//! Every token in here is made by the hub for the test and dies with the temporary directory.

use citro_core::launch;
use citro_core::store::{Home, Store};
use citro_hub::model::{TeamRole, UserRole};
use citro_hub::server::admin;
use citro_hub::server::run::Hub as HubServer;
use citro_hub::server::store::Config;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CITRO: &str = env!("CARGO_BIN_EXE_citro");

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("citro-cli-it-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct World {
    // dropped first: the hub stops before its directory goes away
    hub: HubServer,
    anna: String,
    ben: String,
    tmp: TempDir,
}

impl World {
    fn new() -> World {
        let tmp = TempDir::new();
        let data = tmp.0.join("hub");
        let anna = admin::create_user(&data, "anna", UserRole::Admin, None).expect("first user").token;
        let ben = admin::create_user(&data, "ben", UserRole::Member, None).expect("ben").token;
        let (team, _) = admin::create_team(&data, "Acme", Some("anna")).expect("team");
        admin::add_member(&data, &team.id, "ben", TeamRole::Member).expect("membership");
        let cfg = Config {
            bind: "127.0.0.1:0".parse().expect("address"),
            lease_ttl: Duration::from_secs(6),
            flush_interval: Duration::from_millis(200),
            ..Config::default()
        };
        let hub = HubServer::start(&data, cfg).expect("the hub starts");
        World { hub, anna, ben, tmp }
    }

    /// A machine: its own home, a stand-in kernel, and a login to the hub made through `citro hub login`.
    fn machine(&self, name: &str, token: &str) -> Machine {
        let home = self.tmp.0.join(format!("machine-{}", name.replace(['\'', ' '], "")));
        fs::create_dir_all(&home).expect("home");
        let (kernel, listener) = fake_kernel(&home);
        let m = Machine { home, kernel, name: name.to_owned(), _listener: listener, tmp: self.tmp.0.clone() };
        // the token comes through the environment (the documented way), never through the argument list
        let out = m
            .cmd(&["hub", "login", "--url", &self.hub.url(), "--device-name", name])
            .env("CITRO_HUB_TOKEN", token)
            .capture();
        assert!(out.status.success(), "login: {}", out.stderr);
        m
    }
}

/// A stand-in kernel in the layout `citro kernel install` leaves.
fn fake_kernel(home: &Path) -> (PathBuf, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("addr").port();
    let body = r#"for a in "$@"; do case "$a" in --user-data-dir=*) ud="${a#--user-data-dir=}";; esac; done
printf '%s\n%s\n' PORT /devtools/browser/test-guid > "$ud/DevToolsActivePort"
sleep 600 &
wait"#
        .replace("PORT", &port.to_string());
    let dir = home.join("kernels").join("clearcote-151.0.7922.108");
    fs::create_dir_all(&dir).expect("kernel dir");
    let binary = dir.join("chrome");
    fs::write(&binary, format!("#!/bin/sh\n{body}\n")).expect("kernel");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
    (binary, listener)
}

struct Machine {
    home: PathBuf,
    kernel: PathBuf,
    name: String,
    _listener: TcpListener,
    tmp: PathBuf,
}

struct Output {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

trait Run {
    fn capture(&mut self) -> Output;
}

impl Run for Command {
    fn capture(&mut self) -> Output {
        let o = Command::output(self).expect("run citro");
        Output {
            status: o.status,
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

/// A `citro` that keeps running (an open in the foreground, a server): its output goes to files.
struct Proc {
    child: Child,
    out: PathBuf,
    err: PathBuf,
}

impl Proc {
    fn stdout(&self) -> String {
        fs::read_to_string(&self.out).unwrap_or_default()
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.err).unwrap_or_default()
    }

    fn wait(&mut self, secs: u64) -> ExitStatus {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if let Some(s) = self.child.try_wait().expect("try_wait") {
                return s;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("the process did not end in {secs}s; stdout: {} stderr: {}", self.stdout(), self.stderr());
    }

    fn signal(&self, sig: &str) {
        let s = Command::new("kill").args([&format!("-{sig}"), &self.child.id().to_string()]).status().expect("kill");
        assert!(s.success());
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Machine {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(CITRO);
        c.arg("--home").arg(&self.home).arg("--kernel").arg(&self.kernel).args(args);
        for var in ["CITRO_HUB_URL", "CITRO_HUB_TOKEN", "CITRO_KERNEL", "CITRO_API_KEY", "CITRO_HOME"] {
            c.env_remove(var);
        }
        c.stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).capture()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "citro {args:?} failed: {}{}", o.stdout, o.stderr);
        o
    }

    fn spawn(&self, args: &[&str], tag: &str) -> Proc {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let (out, err) = (self.tmp.join(format!("{tag}-{n}.out")), self.tmp.join(format!("{tag}-{n}.err")));
        let child = self
            .cmd(args)
            .stdout(File::create(&out).expect("out"))
            .stderr(File::create(&err).expect("err"))
            .spawn()
            .expect("spawn citro");
        Proc { child, out, err }
    }

    fn data(&self, id: &str) -> PathBuf {
        self.home.join("profiles").join(id).join("data")
    }

    /// What the browser does while it runs: it writes into its data directory.
    fn browse(&self, id: &str, file: &str, content: &str) {
        let path = self.data(id).join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, content).expect("write");
    }

    fn read(&self, id: &str, file: &str) -> Option<String> {
        fs::read_to_string(self.data(id).join(file)).ok()
    }

    fn running(&self, id: &str) -> bool {
        launch::running(&Home { root: self.home.clone() }, id).is_some()
    }
}

impl Drop for Machine {
    /// A failed test must not leave stand-in browsers behind.
    fn drop(&mut self) {
        let home = Home { root: self.home.clone() };
        for p in Store::open(&home).map(|s| s.profiles().to_vec()).unwrap_or_default() {
            let _ = launch::stop(&home, &p.profile_id);
        }
        let _ = &self.name;
    }
}

fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

/// One HTTP GET against the Local API; the body as JSON.
fn api_get(addr: &str, path: &str) -> Value {
    let mut s = TcpStream::connect(addr).expect("connect to the Local API");
    s.set_read_timeout(Some(Duration::from_secs(120))).expect("timeout");
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").expect("request");
    let mut raw = String::new();
    s.read_to_string(&mut raw).expect("response");
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON ({e}): {raw}"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("port").local_addr().expect("addr").port()
}

/// anna makes a profile, puts it on the hub, and ben's machine pulls it. Returns the hub's id.
fn shared_profile(w: &World, a: &Machine, b: &Machine, name: &str) -> String {
    let id = a.ok(&["new", name]).stdout.trim().to_owned();
    let pushed = a.ok(&["hub", "push", &id]);
    let hub_id = pushed.stdout.trim().to_owned();
    assert_ne!(hub_id, id, "the hub gives the profile its own id");
    assert!(hub_id.starts_with('k') && hub_id.len() == 7, "{hub_id}");
    let pulled = b.ok(&["hub", "pull"]);
    assert!(pulled.stdout.contains(&hub_id), "ben's machine sees it: {}", pulled.stdout);
    let _ = w;
    hub_id
}

#[test]
fn a_profile_is_open_on_one_device_only_and_its_data_follows_it() {
    let w = World::new();
    let a = w.machine("Anna's Mac", &w.anna);
    let b = w.machine("Ben's PC", &w.ben);
    let id = shared_profile(&w, &a, &b, "Shop 1");

    // the hub knows who is who, and the profile is marked as the team's
    let status = a.ok(&["hub", "status"]).stdout;
    assert!(status.contains("anna") && status.contains("Acme") && status.contains("reachable  yes"), "{status}");
    assert!(a.ok(&["list"]).stdout.contains("hub"), "the list marks the team's profiles");
    assert!(a.ok(&["show", &id]).stdout.contains("\"hub\": true"));

    // anna opens it: the command stays in the foreground and keeps the lock
    let mut open_a = a.spawn(&["open", &id], "open-a");
    wait_until("the browser on anna's machine", 30, || a.running(&id));
    assert!(open_a.stderr().contains("belongs to the team hub"), "{}", open_a.stderr());
    a.browse(&id, "Default/Cookies", "session=anna-1");

    // ben sees who has it, and cannot open it: no browser, nothing fetched
    let locks = b.ok(&["hub", "locks"]).stdout;
    assert!(locks.contains(&id) && locks.contains("anna") && locks.contains("Anna's Mac"), "{locks}");
    let refused = b.run(&["open", &id]);
    assert!(!refused.status.success());
    assert!(
        refused.stderr.contains("anna")
            && refused.stderr.contains("Anna's Mac")
            && refused.stderr.contains("another device"),
        "{}",
        refused.stderr
    );
    assert!(!b.running(&id) && !b.data(&id).exists(), "no browser, and nothing was downloaded");

    // the Local API cannot get around it either
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut api_b = b.spawn(&["serve", "--bind", &addr], "serve-b");
    wait_until("the Local API on ben's machine", 30, || TcpStream::connect(&addr).is_ok());
    for path in
        [format!("/api/v1/browser/start?user_id={id}"), format!("/api/v1/browser/start?user_id={id}&open_tabs=1")]
    {
        let reply = api_get(&addr, &path);
        assert_eq!(reply["code"], -1, "{reply}");
        let msg = reply["msg"].as_str().unwrap_or_default();
        assert!(msg.contains("anna") && msg.contains("Anna's Mac"), "{reply}");
        assert!(!b.running(&id));
    }
    // a direct start of the stored profile is refused by the launcher itself
    let home_b = Home { root: b.home.clone() };
    let profile = Store::open(&home_b).expect("store").get(&id).expect("profile").clone();
    let kernel = citro_core::kernel::Kernel::from_path(b.kernel.clone()).expect("kernel");
    let direct = launch::start(&home_b, &kernel, &profile, &launch::LaunchOptions::default());
    assert!(matches!(direct, Err(citro_core::Error::HubBound(_))), "{direct:?}");
    assert!(!b.running(&id));

    // anna closes it from another terminal: data up, lock back, and her open command ends cleanly
    let closed = a.ok(&["close", &id]);
    assert!(closed.stdout.contains("closed") && closed.stdout.contains("handed the profile back"), "{}", closed.stdout);
    let end = open_a.wait(30);
    assert!(end.success(), "stdout: {} stderr: {}", open_a.stdout(), open_a.stderr());
    assert!(!a.running(&id));
    assert!(b.ok(&["hub", "locks"]).stderr.contains("nothing is open"));

    // now ben may have it, and anna's cookies are there when it opens (through the API this time)
    let started = api_get(&addr, &format!("/api/v1/browser/start?user_id={id}"));
    assert_eq!(started["code"], 0, "{started}");
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1"), "the data followed the profile");
    b.browse(&id, "Default/Cookies", "session=anna-1\nsession=ben-2");
    let locked_for_anna = a.run(&["open", &id]);
    assert!(!locked_for_anna.status.success() && locked_for_anna.stderr.contains("ben"), "{}", locked_for_anna.stderr);
    let stopped = api_get(&addr, &format!("/api/v1/browser/stop?user_id={id}"));
    assert_eq!(stopped["code"], 0, "{stopped}");
    assert!(stopped["data"]["hub"].as_str().unwrap_or_default().contains("lock released"), "{stopped}");
    assert!(!b.running(&id));

    // and back on anna's machine: ben's work is there
    let mut again = a.spawn(&["open", &id], "open-a2");
    wait_until("the browser on anna's machine again", 30, || a.running(&id));
    assert_eq!(a.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1\nsession=ben-2"));

    // Ctrl-C (SIGTERM here) closes properly: data uploaded, lock released, no stray browser
    a.browse(&id, "Default/Cookies", "session=anna-1\nsession=ben-2\nsession=anna-3");
    again.signal("TERM");
    let end = again.wait(30);
    assert!(end.success(), "stdout: {} stderr: {}", again.stdout(), again.stderr());
    assert!(again.stderr().contains("lock released"), "{}", again.stderr());
    assert!(!a.running(&id));
    let mut after = b.spawn(&["open", &id], "open-b2");
    wait_until("ben may open it right after", 30, || b.running(&id));
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1\nsession=ben-2\nsession=anna-3"));
    assert!(b.ok(&["close", &id]).stdout.contains("handed the profile back"));
    assert!(after.wait(30).success());

    api_b.signal("TERM");
    assert!(api_b.wait(30).success(), "{}", api_b.stderr());
}

#[test]
fn stopping_the_local_api_closes_the_browsers_it_opened_and_hands_the_locks_back() {
    let w = World::new();
    let a = w.machine("Anna's Mac", &w.anna);
    let b = w.machine("Ben's PC", &w.ben);
    let id = shared_profile(&w, &a, &b, "Shop 2");

    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut api_a = a.spawn(&["serve", "--bind", &addr], "serve-a");
    wait_until("the Local API", 30, || TcpStream::connect(&addr).is_ok());
    let started = api_get(&addr, &format!("/api/v1/browser/start?user_id={id}"));
    assert_eq!(started["code"], 0, "{started}");
    a.browse(&id, "Default/Cookies", "session=from-the-api");
    assert!(b.run(&["open", &id]).stderr.contains("anna"));

    // the server goes away (Ctrl-C): nothing else would keep the lock alive, so the browser is closed properly
    api_a.signal("TERM");
    let end = api_a.wait(60);
    assert!(end.success(), "stdout: {} stderr: {}", api_a.stdout(), api_a.stderr());
    assert!(!a.running(&id), "the browser is closed");
    let mut open_b = b.spawn(&["open", &id], "open-b");
    wait_until("ben may open it", 30, || b.running(&id));
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=from-the-api"));
    assert!(b.ok(&["close", &id]).status.success());
    assert!(open_b.wait(30).success());
}

#[test]
fn the_teams_profiles_are_not_deleted_or_forgotten_by_accident() {
    let w = World::new();
    let a = w.machine("Anna's Mac", &w.anna);
    let b = w.machine("Ben's PC", &w.ben);
    let id = shared_profile(&w, &a, &b, "Shop 3");
    let local = a.ok(&["new", "Only mine"]).stdout.trim().to_owned();

    // a local profile is deleted as before; a team profile needs an explicit `--everywhere`
    let refused = a.run(&["delete", &id]);
    assert!(!refused.status.success() && refused.stderr.contains("whole team"), "{}", refused.stderr);
    assert!(a.ok(&["show", &id]).status.success(), "it is still here");
    assert!(b.ok(&["show", &id]).status.success(), "and on the other machine");
    assert!(a.ok(&["delete", &local]).stdout.contains("deleted"));

    // edits of a team profile reach everybody
    a.ok(&["edit", &id, "--name", "Shop 3 (renamed)", "--remark", "for the winter push"]);
    assert!(a.ok(&["show", &id]).stdout.contains("Shop 3 (renamed)"));
    b.ok(&["hub", "pull"]);
    let shown = b.ok(&["show", &id]).stdout;
    assert!(shown.contains("Shop 3 (renamed)") && shown.contains("for the winter push"), "{shown}");

    // nobody edits a profile that is open
    let mut open_a = a.spawn(&["open", &id], "open-a");
    wait_until("the browser", 30, || a.running(&id));
    let busy = b.run(&["edit", &id, "--name", "hijacked"]);
    assert!(!busy.status.success(), "{}{}", busy.stdout, busy.stderr);
    a.ok(&["close", &id]);
    assert!(open_a.wait(30).success());

    // a data export lists the team's profiles without secrets, like any other
    let csv = b.ok(&["export"]).stdout;
    assert!(csv.contains("Shop 3 (renamed)") && !csv.contains("hijacked"), "{csv}");

    // logging out forgets the hub but keeps the profile, which then cannot be opened; unbinding frees it
    assert!(b.ok(&["hub", "logout"]).stdout.contains("logged out"));
    let locked_out = b.run(&["open", &id]);
    assert!(!locked_out.status.success() && locked_out.stderr.contains("hub login"), "{}", locked_out.stderr);
    assert!(b.ok(&["hub", "unbind", &id]).stdout.contains("local profile"));
    assert!(b.ok(&["delete", &id]).stdout.contains("deleted"), "a local copy may go");
    assert!(a.ok(&["show", &id]).status.success(), "the team's copy is untouched");
    assert!(a.ok(&["delete", &id, "--everywhere"]).stdout.contains("deleted"));
    assert!(!a.run(&["show", &id]).status.success());
}
