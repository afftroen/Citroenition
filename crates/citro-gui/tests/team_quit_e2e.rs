//! Quitting the app: a team browser must not outlive it, because nothing else would keep its lock alive. The app
//! closes it, waits until its data is on the hub and hands the lock back, so the next device can open it at once.
//!
//! This is a file of its own on purpose: `citro_sync::shutdown` is a process-wide switch that never goes back.

mod common;

use citro_gui::backend::{Backend, RunState};
use common::{World, headless, wait_until};

#[test]
fn quitting_closes_the_team_browsers_uploads_their_data_and_hands_the_lock_back() {
    let w = World::new();
    let mac = w.device("Anna's Mac", &w.anna);
    let ben = w.device("Ben's laptop", &w.ben);
    mac.make("shop-4");
    mac.make("private-notes"); // never shared: not the hub's business
    let local = mac.id("shop-4");
    mac.app.share_profiles(&[local]).expect("share");
    let id = mac.id("shop-4");
    let private = mac.id("private-notes");
    ben.app.pull_team().expect("pull");

    mac.app.open(&id, &headless()).expect("open the team profile");
    mac.app.open(&private, &headless()).expect("open the private one");
    mac.browse(&id, "Default/Cookies", "session=before-quit");

    let lines = mac.app.shutdown();
    assert_eq!(lines.len(), 1, "only the team browser needed closing: {lines:?}");
    assert!(lines[0].contains("shop-4"), "{lines:?}");
    assert!(!mac.running(&id), "the team browser is closed");
    assert!(mac.running(&private), "a private profile is the user's own business: it is left alone");

    // the lock is back and the data is up: the other device opens it now and finds the cookie
    ben.app.open(&id, &headless()).expect("free the moment quitting returned");
    assert_eq!(ben.read(&id, "Default/Cookies").as_deref(), Some("session=before-quit"));
    // Ben's own browser is closed again at once: the quit switch of `citro-sync` is process-wide and never goes
    // back, and this test process hosts both devices (the app is one process per device, and it exits after it).
    // It is closed properly, though: his data goes up and the lock comes back.
    wait_until("ben's browser is closed and the lock is back", 20, || ben.state("shop-4") == RunState::Closed);
    assert!(!ben.running(&id));
}
