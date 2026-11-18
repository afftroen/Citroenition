//! End to end: a real hub in this process (real HTTP on 127.0.0.1), several "devices" (each has its own home
//! directory and device id), and a stand-in kernel (a shell script that announces a listening port the way Chromium
//! does and then idles). What is checked is what the sync layer promises: one profile is open on one device at a
//! time, its data follows the profile from device to device, and a lock that is lost stops the browser.
//!
//! Every token in here is made by the hub for the test and dies with the temporary directory.

use citro_core::kernel::{self, Kernel};
use citro_core::launch::{self, LaunchOptions};
use citro_core::profile::Profile;
use citro_core::store::{self, Home, Store};
use citro_hub::model::{TeamRole, UserRole};
use citro_hub::server::admin;
use citro_hub::server::run::Hub as HubServer;
use citro_hub::server::store::Config;
use citro_sync::{Bound, Error, Hub};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("citro-sync-it-{tag}-{}-{n}", std::process::id()));
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
struct World {
    // dropped first: the hub stops before its directory goes away
    hub: Option<HubServer>,
    cfg: Config,
    data: PathBuf,
    anna: String,
    ben: String,
    tmp: TempDir,
}

impl World {
    fn new() -> World {
        World::on("127.0.0.1:0")
    }

    fn on(bind: &str) -> World {
        let tmp = TempDir::new("w");
        let data = tmp.0.join("hub");
        let anna = admin::create_user(&data, "anna", UserRole::Admin, None).expect("first user").token;
        let ben = admin::create_user(&data, "ben", UserRole::Member, None).expect("ben").token;
        let (team, _) = admin::create_team(&data, "Acme", Some("anna")).expect("team");
        admin::add_member(&data, &team.id, "ben", TeamRole::Member).expect("membership");
        let cfg = Config {
            bind: bind.parse().expect("address"),
            lease_ttl: Duration::from_secs(6),
            flush_interval: Duration::from_millis(200),
            ..Config::default()
        };
        let hub = HubServer::start(&data, cfg.clone()).expect("the hub starts");
        World { hub: Some(hub), cfg, data, anna, ben, tmp }
    }

    fn url(&self) -> String {
        self.hub.as_ref().expect("the hub is running").url()
    }

    /// A new machine, logged in to the hub as `token`'s user.
    fn device(&self, name: &str, token: &str) -> Dev {
        let home = Home { root: self.tmp.0.join(format!("dev-{name}")) };
        fs::create_dir_all(&home.root).expect("home");
        let hub = Hub::log_in(&home, &self.url(), token, Some(name), None).expect("login");
        let (kernel, listener) = fake_kernel(&home);
        Dev { home, hub, kernel, _listener: listener }
    }

    fn stop_hub(&mut self) {
        self.hub.take().expect("the hub is running").stop();
    }

    fn start_hub(&mut self) {
        self.hub = Some(HubServer::start(&self.data, self.cfg.clone()).expect("the hub restarts"));
    }
}

/// A stand-in kernel in the layout `citro kernel install` leaves: it writes `DevToolsActivePort` for a port that
/// really listens, then idles until it is stopped.
fn fake_kernel(home: &Home) -> (Kernel, TcpListener) {
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
    (Kernel::from_path(binary).expect("kernel"), listener)
}

struct Dev {
    home: Home,
    hub: Hub,
    kernel: Kernel,
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

fn opts() -> LaunchOptions {
    LaunchOptions { headless: true, startup_timeout: Duration::from_secs(20), ..LaunchOptions::default() }
}

impl Dev {
    /// A profile made on this device (the way `citro new` does), then published to the team. Returns the hub's id.
    fn publish(&self, name: &str) -> String {
        let local = store::locked(&self.home, |s| s.create(Profile { name: name.to_owned(), ..Default::default() }))
            .expect("create");
        let pushed = self.hub.push(&local.profile_id).expect("push");
        assert!(self.home.hub_bound(&pushed.profile.profile_id), "published = bound to the hub");
        pushed.profile.profile_id
    }

    fn profile(&self, id: &str) -> Profile {
        Store::open(&self.home).expect("store").get(id).expect("the profile is here").clone()
    }

    /// Open through the sync layer: the lock, the data, the browser.
    fn open(&self, id: &str) -> Result<launch::Running, Error> {
        self.hub.open(&self.kernel, &self.profile(id), &opts())
    }

    fn data_dir(&self, id: &str) -> PathBuf {
        self.home.profile_dir(id).join("data")
    }

    /// What the browser does while it runs: it writes into its data directory.
    fn browse(&self, id: &str, file: &str, content: &str) {
        let path = self.data_dir(id).join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, content).expect("write");
    }

    fn read(&self, id: &str, file: &str) -> Option<String> {
        fs::read_to_string(self.data_dir(id).join(file)).ok()
    }

    fn running(&self, id: &str) -> bool {
        launch::running(&self.home, id).is_some()
    }

    /// The process that opened a profile dies and its browser lives on, with nothing watching it: the lock held by
    /// hand, the local copy marked as the newer one (what `open` does before it launches), the browser started
    /// around the supervisor.
    fn open_unsupervised(&self, id: &str) {
        self.hub.client().lock(id).expect("lock by hand");
        let mut bound = Bound::load(&self.home, id).expect("bound");
        bound.dirty = true;
        bound.save(&self.home, id).expect("save");
        let cleared = LaunchOptions { hub_cleared: true, ..opts() };
        launch::start(&self.home, &self.kernel, &self.profile(id), &cleared).expect("start by hand");
        assert!(self.running(id) && !citro_sync::is_syncing(&self.home, id));
    }

    /// The browser is gone and so is whatever watched it.
    fn kill_unsupervised(&self, id: &str) {
        launch::stop(&self.home, id).expect("stop");
        let _ = launch::sync_closed(&self.home);
    }

    fn close(&self, id: &str) -> citro_sync::CloseReport {
        citro_sync::close(&self.home, id, Duration::from_secs(30)).expect("close")
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

fn conflicts(home: &Home, id: &str) -> Vec<PathBuf> {
    let root = home.profile_dir(id);
    let mut v: Vec<PathBuf> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("data.conflict-")))
        .collect();
    v.sort();
    v
}

fn file_in(dir: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(dir.join(rel)).ok()
}

// ---- the promise ------------------------------------------------------------------------------------------------

#[test]
fn a_profile_is_open_on_one_device_only_and_its_data_follows_it() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna); // the same user on another device
    let b = w.device("Ben's laptop", &w.ben); // another user

    // anna makes a profile (with a password the hub must not hand out) and publishes it
    let local = store::locked(&a.home, |s| {
        s.create(Profile { name: "shop-1".into(), password: "hunter2".into(), ..Default::default() })
    })
    .expect("create");
    let id = a.hub.push(&local.profile_id).expect("push").profile.profile_id;
    assert_ne!(id, local.profile_id, "the hub names the profile");

    // everybody else gets it on the next pull, without its secrets
    for d in [&b, &c] {
        let report = d.hub.pull().expect("pull");
        assert_eq!(report.added.len(), 1, "{report:?}");
        assert_eq!(report.added[0].name, "shop-1");
        assert!(d.home.hub_bound(&id));
        assert_ne!(d.profile(&id).password, "hunter2", "the list never carries secrets");
    }

    // anna opens it; from now on nobody else can, whoever they are and however they ask
    let running = a.open(&id).expect("open on the first device");
    assert!(a.running(&id) && citro_sync::is_syncing(&a.home, &id));
    for (who, d) in [("ben", &b), ("anna on another device", &c)] {
        let err = d.open(&id).expect_err(who);
        let by = err.locked_by().unwrap_or_else(|| panic!("{who}: {err}"));
        assert_eq!(by.user, "anna", "{who}");
        assert_eq!(by.device, "Anna's Mac", "{who}");
        assert!(!d.running(&id), "{who}: no browser was started");
        assert!(!d.data_dir(&id).exists(), "{who}: nothing was fetched either");
        // the free function (what the CLI and the Local API call) goes through the same lock
        let err = citro_sync::open(&d.home, &d.kernel, &d.profile(&id), &opts()).expect_err(who);
        assert!(err.locked_by().is_some(), "{who}: {err}");
        // and going around the sync layer is not possible: the launcher itself refuses a hub profile
        let around = launch::start(&d.home, &d.kernel, &d.profile(&id), &opts()).expect_err(who);
        assert!(matches!(around, citro_core::Error::HubBound(_)), "{who}: {around}");
        assert!(!d.running(&id), "{who}");
    }
    // the lock list says who has what, and an edit has to wait for the close
    let rows = b.hub.locks().expect("locks");
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].user.as_str(), rows[0].device.as_str(), rows[0].mine), ("anna", "Anna's Mac", false));
    assert!(a.hub.locks().expect("locks")[0].mine);
    assert!(
        citro_sync::edit(&b.home, &id, |_, p| {
            p.name = "renamed".into();
            Ok(())
        })
        .is_err(),
        "no edits while it is open"
    );
    assert_eq!(running.profile_id, id);

    // the browser works: a cookie, and a cache that must not travel
    a.browse(&id, "Default/Cookies", "session=anna-1");
    a.browse(&id, "Default/Cache/blob", "x".repeat(4096).as_str());
    let closed = a.close(&id);
    assert!(closed.stopped);
    let out = closed.outcome.expect("a hub profile reports its close");
    assert!(out.is_clean(), "{}", out.summary());
    let v1 = out.uploaded.expect("the data went up");
    assert!(!a.running(&id) && !citro_sync::is_syncing(&a.home, &id));
    assert!(b.hub.locks().expect("locks").is_empty(), "the lock is back");
    b.hub.pull().expect("pull");
    assert!(b.profile(&id).last_close_time > 0, "the hub stamped the close");

    // now it is ben's: the cookie is there, the cache is not
    b.open(&id).expect("open on the second device");
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1"));
    assert!(b.read(&id, "Default/Cache/blob").is_none(), "caches stay home");
    assert!(a.open(&id).is_err(), "and anna is the one refused now");
    b.browse(&id, "Default/Cookies", "session=anna-1\nsession=ben-2");
    let out = b.close(&id).outcome.expect("outcome");
    assert!(out.is_clean(), "{}", out.summary());
    assert!(out.uploaded.expect("went up") > v1, "a newer version");

    // and back on anna's first device: ben's work is there
    a.open(&id).expect("open again");
    assert_eq!(a.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1\nsession=ben-2"));
    assert!(a.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn a_lost_lock_stops_the_browser_and_what_it_did_meanwhile_is_kept_not_merged() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna);
    let id = a.publish("shop-2");
    c.hub.pull().expect("pull");

    // a team owner breaks the lock while the browser runs, and the other device takes the profile at once
    a.open(&id).expect("open");
    a.browse(&id, "Default/Cookies", "session=from-a-after-the-lock-was-lost");
    assert!(c.hub.force_unlock(&id).expect("force unlock"), "there was a lock to break");
    c.open(&id).expect("the other device may have it now");

    // the first device notices on its next heartbeat and stops
    wait_until("the browser on the first device stops", 20, || !a.running(&id));
    let out = citro_sync::wait(&a.home, &id, Duration::from_secs(30)).expect("the supervisor reports");
    assert!(out.lost.is_some(), "{}", out.summary());
    assert!(out.error.is_some(), "somebody else has it: {}", out.summary());
    let kept = out.conflict.expect("what it did was kept aside");
    assert_eq!(file_in(&kept, "Default/Cookies").as_deref(), Some("session=from-a-after-the-lock-was-lost"));
    assert_eq!(conflicts(&a.home, &id), [kept], "exactly one copy, named after the conflict");
    assert!(!a.data_dir(&id).exists(), "the next open starts from the hub's copy");

    // the device that has it now never saw that cookie, and closes normally
    assert!(c.read(&id, "Default/Cookies").is_none());
    assert!(c.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn a_lost_lock_nobody_took_is_handed_back_properly() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna);
    let id = a.publish("shop-3");
    c.hub.pull().expect("pull");

    a.open(&id).expect("open");
    a.browse(&id, "Default/Cookies", "session=a");
    assert!(c.hub.force_unlock(&id).expect("force unlock"));

    wait_until("the browser stops", 20, || !a.running(&id));
    let out = citro_sync::wait(&a.home, &id, Duration::from_secs(30)).expect("the supervisor reports");
    assert!(out.lost.is_some(), "the owner was told: {}", out.summary());
    assert!(out.released, "the lock was taken again and handed back: {}", out.summary());
    assert!(out.uploaded.is_some(), "nobody else touched the data, so it goes up: {}", out.summary());
    assert!(out.conflict.is_none() && out.error.is_none(), "{}", out.summary());
    assert!(c.hub.locks().expect("locks").is_empty());

    // and the other device sees it
    c.open(&id).expect("open");
    assert_eq!(c.read(&id, "Default/Cookies").as_deref(), Some("session=a"));
    assert!(c.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn a_browser_whose_session_died_is_taken_back_under_the_lock() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna);
    let id = a.publish("shop-4");
    c.hub.pull().expect("pull");

    // the process that opened the profile died, the browser lives on (this test plays that process)
    a.open_unsupervised(&id);
    a.browse(&id, "Default/Cookies", "session=survivor");

    // the next process of this device finds it and puts it back under the lock
    assert!(a.hub.adopt().is_empty(), "nothing to complain about");
    assert!(citro_sync::is_syncing(&a.home, &id), "supervised again");
    assert!(c.open(&id).expect_err("it is locked again").locked_by().is_some());
    let out = a.close(&id).outcome.expect("outcome");
    assert!(out.is_clean(), "{}", out.summary());
    assert!(out.uploaded.is_some());
    c.open(&id).expect("free now");
    assert_eq!(c.read(&id, "Default/Cookies").as_deref(), Some("session=survivor"));
    assert!(c.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn an_orphaned_browser_is_stopped_when_somebody_else_has_the_profile_now() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna);
    let id = a.publish("shop-5");
    c.hub.pull().expect("pull");

    a.open_unsupervised(&id);
    a.browse(&id, "Default/Cookies", "session=orphan");
    // the lock went to somebody else while nobody watched
    assert!(a.hub.force_unlock(&id).expect("force"));
    c.open(&id).expect("the other device has it");

    let problems = a.hub.adopt();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].error.as_deref().unwrap_or_default().contains("Anna's PC"), "{:?}", problems[0]);
    assert!(!a.running(&id), "the orphan was stopped");
    let kept = problems[0].conflict.clone().expect("kept aside");
    assert_eq!(file_in(&kept, "Default/Cookies").as_deref(), Some("session=orphan"));
    assert!(c.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn data_that_never_went_up_is_uploaded_by_recover() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let b = w.device("Ben's laptop", &w.ben);
    let id = a.publish("shop-6");
    b.hub.pull().expect("pull");

    // worked in, and the process died before the upload; the browser is gone too
    a.open_unsupervised(&id);
    a.browse(&id, "Default/Cookies", "session=unsaved");
    let err = citro_sync::recover(&a.home, &id).expect_err("not while the browser runs");
    assert!(err.to_string().contains("close it first"), "{err}");
    a.kill_unsupervised(&id);

    let out = citro_sync::recover(&a.home, &id).expect("recover");
    assert!(out.is_clean() && out.uploaded.is_some(), "{}", out.summary());
    assert!(citro_sync::recover(&a.home, &id).expect("again").notes.iter().any(|n| n.contains("nothing")));
    b.open(&id).expect("open");
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=unsaved"));
    assert!(b.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn a_late_recover_never_overwrites_what_the_team_did_meanwhile() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let c = w.device("Anna's PC", &w.anna);
    let id = a.publish("shop-10");
    c.hub.pull().expect("pull");

    a.open_unsupervised(&id);
    a.browse(&id, "Default/Cookies", "session=a-unsaved");
    a.kill_unsupervised(&id);
    // the dead session's lock is broken and the profile moves on
    assert!(c.hub.force_unlock(&id).expect("force"));
    c.open(&id).expect("open on the other device");
    c.browse(&id, "Default/Cookies", "session=c-newer");
    assert!(c.close(&id).outcome.expect("outcome").is_clean());

    // the late recovery is refused by the hub's version check: nothing of the newer copy is lost
    let out = citro_sync::recover(&a.home, &id).expect("recover runs");
    assert!(out.uploaded.is_none() && out.error.is_some(), "{}", out.summary());
    let kept = out.conflict.expect("what this device has is kept aside");
    assert_eq!(file_in(&kept, "Default/Cookies").as_deref(), Some("session=a-unsaved"));
    a.open(&id).expect("open");
    assert_eq!(a.read(&id, "Default/Cookies").as_deref(), Some("session=c-newer"), "the team's copy is the live one");
    assert!(a.close(&id).outcome.expect("outcome").is_clean());
}

#[test]
fn a_hub_that_goes_away_at_the_close_gets_the_data_when_it_is_back() {
    // a fixed port, so the hub can come back where the clients look for it
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("port");
        l.local_addr().expect("addr").port()
    };
    let mut w = World::on(&format!("127.0.0.1:{port}"));
    let a = w.device("Anna's Mac", &w.anna);
    let id = a.publish("shop-7");
    a.open(&id).expect("open");
    a.browse(&id, "Default/Cookies", "session=offline");

    w.stop_hub();
    // `close` stops the browser at once and then waits for the upload, which keeps trying while the hub is away
    let report = thread::scope(|scope| {
        let closer = scope.spawn(|| citro_sync::close(&a.home, &id, Duration::from_secs(90)));
        wait_until("the browser is stopped", 20, || !a.running(&id));
        thread::sleep(Duration::from_secs(3));
        assert!(citro_sync::is_syncing(&a.home, &id), "still trying while the hub is away");
        w.start_hub();
        closer.join().expect("close thread").expect("close")
    });
    assert!(report.stopped);
    let out = report.outcome.expect("the supervisor finished once the hub was back");
    assert!(out.uploaded.is_some(), "{}", out.summary());
    assert!(out.released, "{}", out.summary());
    let b = w.device("Ben's laptop", &w.ben);
    b.hub.pull().expect("pull");
    b.open(&id).expect("open");
    assert_eq!(b.read(&id, "Default/Cookies").as_deref(), Some("session=offline"));
    assert!(b.close(&id).outcome.expect("outcome").is_clean());
}

// ---- the rest of the surface ------------------------------------------------------------------------------------

#[test]
fn edits_and_deletes_go_through_to_the_team() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let b = w.device("Ben's laptop", &w.ben);
    let id = a.publish("shop-8");
    b.hub.pull().expect("pull");

    let edited = citro_sync::edit(&a.home, &id, |_, p| {
        p.name = "shop-8 renamed".into();
        Ok(())
    })
    .expect("edit");
    assert_eq!(edited.name, "shop-8 renamed");
    let report = b.hub.pull().expect("pull");
    assert_eq!(report.updated.len(), 1, "{report:?}");
    assert_eq!(b.profile(&id).name, "shop-8 renamed");
    assert!(b.home.hub_bound(&id));

    // a profile of the team is not deleted by accident
    let err = citro_sync::delete(&a.home, &id, false).expect_err("needs an explicit yes");
    assert!(matches!(err, Error::TeamProfile(_)), "{err}");
    assert!(b.hub.pull().expect("pull").gone.is_empty());
    citro_sync::delete(&a.home, &id, true).expect("delete everywhere");
    let report = b.hub.pull().expect("pull");
    assert_eq!(report.gone.len(), 1, "the others learn that it is gone: {report:?}");
    // it stays on ben's device until he frees it
    assert!(b.home.hub_bound(&id));
    citro_sync::unbind(&b.home, &id).expect("unbind");
    assert!(!b.home.hub_bound(&id));
}

#[test]
fn logging_out_keeps_the_profiles_but_forgets_the_hub() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let id = a.publish("shop-9");
    assert!(citro_sync::load_login(&a.home).expect("login file").is_some());
    assert!(citro_sync::log_out(&a.home).expect("log out"));
    assert!(citro_sync::load_login(&a.home).expect("login file").is_none());
    assert!(Hub::connect(&a.home).expect("connect").is_none());
    assert!(a.home.hub_bound(&id), "still bound: it is a team profile until it is freed");
    // and while logged out it cannot be opened at all
    let err = citro_sync::open(&a.home, &a.kernel, &a.profile(&id), &opts()).expect_err("no hub, no lock, no browser");
    assert!(matches!(err, Error::NotLoggedIn), "{err}");
    assert!(!a.running(&id));
}

/// Everything is a team profile unless it says otherwise; a group decides who in the team may use its profiles,
/// and the hub, not the app, makes it stick.
#[test]
fn profiles_are_shared_by_themselves_and_a_groups_rules_hold_on_every_device() {
    use citro_core::team::GroupAccess;
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna); // anna owns the team
    let b = w.device("Ben's laptop", &w.ben); // ben is a member

    // anna's local profiles: one normal, one in a VIP group, one she keeps for herself, one in a private group
    let (vip, private) = store::locked(&a.home, |s| {
        let vip = s.create_group("VIP", "")?;
        let private = s.create_group("Private", "")?;
        Ok((vip.group_id, private.group_id))
    })
    .expect("groups");
    store::locked(&a.home, |s| {
        for mut x in s.groups().to_vec() {
            if x.group_id == vip {
                x.access = GroupAccess { see: TeamRole::Admin, open: TeamRole::Admin, people: vec![] };
                x.tabs = vec!["https://ads.example/".into()];
            }
            if x.group_id == private {
                x.local_only = true;
            }
            s.update_group(x)?;
        }
        Ok(())
    })
    .expect("rules");
    for (name, group, local_only) in [
        ("normal", "0", false),
        ("big spender", vip.as_str(), false),
        ("mine", "0", true),
        ("secret", private.as_str(), false),
    ] {
        store::locked(&a.home, |s| {
            s.create(Profile { name: name.into(), group_id: group.into(), local_only, ..Default::default() })
        })
        .expect("create");
    }

    // nobody pressed Share: one round of the background sync puts every team profile on the hub
    let shared: Vec<String> = a
        .hub
        .share_new()
        .into_iter()
        .map(|(e, r)| {
            r.expect("shared");
            e.name
        })
        .collect();
    assert_eq!(shared.len(), 2, "{shared:?}");
    assert!(shared.contains(&"normal".to_owned()) && shared.contains(&"big spender".to_owned()));
    assert!(a.hub.share_new().is_empty(), "once shared, nothing is shared twice");

    // the VIP group went up with its rules and its start page
    let on_hub = a.hub.client().groups(&a.hub.login().team_id).expect("groups");
    let g = on_hub.iter().find(|g| g.name == "VIP").expect("VIP is on the hub");
    assert_eq!(g.access.see, TeamRole::Admin);
    assert_eq!(g.settings.tabs, ["https://ads.example/"]);
    assert!(on_hub.iter().all(|g| g.name != "Private"), "a local-only group stays here");

    // ben, a member, gets the normal profile and nothing of the VIP group
    let r = b.hub.pull().expect("pull");
    let names: Vec<&str> = r.added.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["normal"], "{r:?}");
    assert!(Store::open(&b.home).unwrap().groups().iter().all(|g| g.group_name != "VIP"));
    let vip_id = Store::open(&a.home)
        .unwrap()
        .profiles()
        .iter()
        .find(|p| p.name == "big spender")
        .map(|p| p.profile_id.clone())
        .unwrap();
    // even asking for it by id gets nothing
    assert!(b.hub.client().profile(&vip_id, None).is_err());
    assert!(b.hub.client().lock(&vip_id).is_err());

    // anna makes ben one of the chosen people: now he gets it, and its group's start page comes along
    let people = a.hub.people().expect("people");
    let ben_id = people.members.iter().find(|m| m.user_name == "ben").map(|m| m.user_id.clone()).unwrap();
    store::locked(&a.home, |s| {
        let mut g = s.groups().iter().find(|g| g.group_name == "VIP").cloned().unwrap();
        g.access.people = vec![ben_id.clone()];
        s.update_group(g)
    })
    .expect("allow ben");
    let vip_local =
        Store::open(&a.home).unwrap().groups().iter().find(|g| g.group_name == "VIP").unwrap().group_id.clone();
    a.hub.push_group(&vip_local).expect("rules go up");
    let r = b.hub.pull().expect("pull");
    assert!(r.added.iter().any(|e| e.name == "big spender"), "{r:?}");
    let ben_vip = Store::open(&b.home).unwrap().groups().iter().find(|g| g.group_name == "VIP").cloned().expect("VIP");
    assert_eq!(ben_vip.tabs, ["https://ads.example/"], "the group's settings came along");
    assert_eq!(ben_vip.access.people, [ben_id]);

    // and ben, a member, cannot change who may use a group
    store::locked(&b.home, |s| {
        let mut g = s.groups().iter().find(|g| g.group_name == "VIP").cloned().unwrap();
        g.access = GroupAccess::default();
        s.update_group(g)
    })
    .expect("local change");
    let ben_vip_id =
        Store::open(&b.home).unwrap().groups().iter().find(|g| g.group_name == "VIP").unwrap().group_id.clone();
    assert!(b.hub.push_group(&ben_vip_id).is_err(), "only owners and admins decide who may use a group");
}

/// Inviting from the app: the owner makes an account in the team and gets its token once; the token logs a new
/// device in at once; a new token replaces it.
#[test]
fn an_owner_invites_someone_and_can_give_them_a_new_token() {
    let w = World::new();
    let a = w.device("Anna's Mac", &w.anna);
    let team = a.hub.login().team_id.clone();
    let (m, token) = a.hub.client().invite(&team, "cara", TeamRole::Member).expect("invite");
    assert_eq!((m.user_name.as_str(), m.role), ("cara", TeamRole::Member));

    // cara fills in the hub address and the token: she is in the team
    let c = w.device("Cara's laptop", &token);
    assert_eq!(c.hub.login().team_id, team);
    a.publish("shop-1");
    assert_eq!(c.hub.pull().expect("pull").added.len(), 1, "she gets the team's profiles");

    // ben, a member, may not invite
    let b = w.device("Ben's laptop", &w.ben);
    assert!(b.hub.client().invite(&team, "dan", TeamRole::Member).is_err());

    // she lost her token: a new one, and the old one stops working
    let fresh = a.hub.client().new_member_token(&team, &m.user_id).expect("new token");
    assert_ne!(fresh, token);
    let home = Home { root: w.tmp.0.join("dev-cara-old") };
    fs::create_dir_all(&home.root).unwrap();
    assert!(Hub::log_in(&home, &w.url(), &token, Some("old"), None).is_err(), "the old token is dead");
    let home2 = Home { root: w.tmp.0.join("dev-cara-new") };
    fs::create_dir_all(&home2.root).unwrap();
    assert!(Hub::log_in(&home2, &w.url(), &fresh, Some("new"), None).is_ok());
}
