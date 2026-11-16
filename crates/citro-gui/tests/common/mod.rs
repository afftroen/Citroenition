//! What the team end-to-end tests of the app share: a real hub in this process (real HTTP on 127.0.0.1), a few
//! "devices" (each has its own home directory, its own `LocalBackend` and a stand-in kernel: a shell script that
//! announces a listening port the way Chromium does and then idles).
//!
//! Every token in here is made by the hub for the test and dies with the temporary directory.
#![allow(dead_code)]

use citro_core::kernel;
use citro_core::launch;
use citro_core::profile::Profile;
use citro_core::store::{Home, Store};
use citro_gui::backend::local::LocalBackend;
use citro_gui::backend::{Backend, OpenOpts, Query, Row, RunState};
use citro_hub::model::{TeamRole, UserRole};
use citro_hub::server::admin;
use citro_hub::server::run::Hub as HubServer;
use citro_hub::server::store::Config;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

/// The finished-session list of `citro-sync` is per process, and one process hosts every device of a test: the tests
/// of a file take turns (the app itself is one process per home).
pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("citro-gui-it-{tag}-{}-{n}", std::process::id()));
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

/// A hub with one team (`Acme`): anna (admin, owner of the team) and ben (member). The lease is long enough that a
/// busy machine does not lose one by accident and short enough that a lost lease is noticed within a couple of
/// seconds (heartbeats go out every third of it).
pub struct World {
    // dropped first: the hub stops before its directory goes away
    pub hub: Option<HubServer>,
    pub anna: String,
    pub ben: String,
    pub tmp: TempDir,
}

impl World {
    pub fn new() -> World {
        let tmp = TempDir::new("w");
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
        World { hub: Some(hub), anna, ben, tmp }
    }

    pub fn url(&self) -> String {
        self.hub.as_ref().expect("the hub is running").url()
    }

    /// A new machine that has never heard of the hub.
    pub fn fresh_device(&self, name: &str) -> Dev {
        let home = Home { root: self.tmp.0.join(format!("dev-{name}")) };
        fs::create_dir_all(&home.root).expect("home");
        let listener = fake_kernel(&home);
        let app = LocalBackend::new(home.clone());
        Dev { home, app, _listener: listener }
    }

    /// A new machine, connected to the hub as `token`'s user.
    pub fn device(&self, name: &str, token: &str) -> Dev {
        let d = self.fresh_device(name);
        d.app.connect_hub(&self.url(), token, name, "").expect("connect");
        d
    }
}

/// A stand-in kernel in the layout `citro kernel install` leaves: it writes `DevToolsActivePort` for a port that
/// really listens, then idles until it is stopped.
fn fake_kernel(home: &Home) -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("addr").port();
    let body = r#"for a in "$@"; do case "$a" in --user-data-dir=*) ud="${a#--user-data-dir=}";; esac; done
printf '%s\n%s\n' PORT /devtools/browser/test-guid > "$ud/DevToolsActivePort"
sleep 600 &
wait"#
        .replace("PORT", &port.to_string());
    let dir = kernel::kernels_dir(home).join("clearcote-151.0.7922.108");
    fs::create_dir_all(&dir).expect("kernel dir");
    let binary = dir.join("chrome");
    fs::write(&binary, format!("#!/bin/sh\n{body}\n")).expect("kernel");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
    listener
}

pub struct Dev {
    pub home: Home,
    pub app: LocalBackend,
    _listener: TcpListener,
}

impl Drop for Dev {
    /// A failed test must not leave stand-in browsers behind.
    fn drop(&mut self) {
        for p in Store::open(&self.home).map(|s| s.profiles().to_vec()).unwrap_or_default() {
            let _ = launch::stop(&self.home, &p.profile_id);
        }
    }
}

pub fn headless() -> OpenOpts {
    OpenOpts { headless: true, sandbox: false }
}

impl Dev {
    /// A profile made in the app (the way the "New profile" dialog does).
    pub fn make(&self, name: &str) -> Profile {
        self.app
            .save_profile(Profile {
                name: name.into(),
                country: "fr".into(),
                persona_os: "linux".into(),
                password: "account-pass".into(),
                cookie: "sid=top-secret-cookie".into(),
                ..Default::default()
            })
            .expect("create")
    }

    pub fn rows(&self) -> Vec<Row> {
        self.app.list_profiles(&Query::default()).expect("list")
    }

    pub fn row(&self, name: &str) -> Row {
        self.rows().into_iter().find(|r| r.profile.name == name).unwrap_or_else(|| panic!("no profile {name}"))
    }

    pub fn try_row(&self, name: &str) -> Option<Row> {
        self.rows().into_iter().find(|r| r.profile.name == name)
    }

    /// The id of a profile by name (a shared profile gets the hub's id, not the one it was made with).
    pub fn id(&self, name: &str) -> String {
        self.row(name).profile.profile_id
    }

    pub fn state(&self, name: &str) -> RunState {
        self.row(name).run
    }

    pub fn data_file(&self, id: &str, rel: &str) -> PathBuf {
        self.home.profile_dir(id).join("data").join(rel)
    }

    /// What the browser does while it runs: it writes into its data directory.
    pub fn browse(&self, id: &str, rel: &str, content: &str) {
        let path = self.data_file(id, rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, content).expect("write");
    }

    pub fn read(&self, id: &str, rel: &str) -> Option<String> {
        fs::read_to_string(self.data_file(id, rel)).ok()
    }

    pub fn running(&self, id: &str) -> bool {
        launch::running(&self.home, id).is_some()
    }
}

pub fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return;
        }
        thread::sleep(Duration::from_millis(150));
    }
    panic!("timed out waiting for: {what}");
}
