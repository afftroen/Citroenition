//! The desktop app's backend against a real hub: what a person does in the app (connect, share, open, close,
//! delete) and what the app must keep true while they do it. The sync layer has its own tests; these check that the
//! app is wired to it, so the window, the command line and the Local API all obey the same lock.

mod common;

use citro_gui::backend::{Backend, BackendError, Level, OpenError, RunState};
use common::{World, headless, serial, wait_until};

#[test]
fn one_profile_one_device_and_the_data_follows_it() {
    let _turn = serial();
    let w = World::new();
    let anna = w.fresh_device("Anna's Mac");
    let ben = w.device("Ben's laptop", &w.ben);

    // connecting says who you are and which team this device works with
    let status = anna.app.connect_hub(&w.url(), &w.anna, "Anna's Mac", "").expect("connect");
    assert!(status.connected && status.user == "anna" && status.team == "Acme", "{status:?}");
    assert_eq!(status.device_name, "Anna's Mac");
    assert_eq!(anna.app.teams().expect("teams")[0].name, "Acme");

    // anna makes a profile and shares it: it belongs to the team from then on
    anna.make("shop-1");
    assert!(anna.row("shop-1").team.is_none(), "private until it is shared");
    let local_id = anna.id("shop-1");
    let said = anna.app.share_profiles(&[local_id]).expect("share");
    assert!(said.contains("Acme"), "{said}");
    let id = anna.id("shop-1");
    assert!(anna.row("shop-1").team.is_some(), "now it is a team profile");

    // ben gets it on the next pull, without its secrets
    assert!(ben.try_row("shop-1").is_none());
    let said = ben.app.pull_team().expect("pull");
    assert!(said.contains("1 new"), "{said}");
    let theirs = ben.row("shop-1");
    assert!(theirs.team.is_some() && theirs.run == RunState::Closed, "{theirs:?}");
    assert_eq!(theirs.profile.profile_id, id);
    assert_ne!(ben.app.profile(&id).expect("record").password, "account-pass", "the hub never hands secrets out");
    assert_ne!(ben.app.profile(&id).expect("record").cookie, "sid=top-secret-cookie");

    // anna opens it: nobody else can, whoever they are
    let run = anna.app.open(&id, &headless()).expect("open on the first device");
    assert!(run.pid > 0 && anna.running(&id));
    assert!(matches!(anna.state("shop-1"), RunState::Open { .. }));
    match ben.app.open(&id, &headless()) {
        Err(OpenError::LockedElsewhere { user, device, .. }) => {
            assert_eq!((user.as_str(), device.as_str()), ("anna", "Anna's Mac"));
        }
        other => panic!("ben must be refused, got {other:?}"),
    }
    assert!(!ben.running(&id), "no browser was started on the second device");
    assert!(!ben.data_file(&id, "").exists(), "its data was not fetched either");

    // the table says so too (the lock table refreshes in the background: the list never waits for the network)
    wait_until(
        "ben's table shows who has it",
        15,
        || matches!(ben.state("shop-1"), RunState::LockedElsewhere { ref user, ref device, .. } if user == "anna" && device == "Anna's Mac"),
    );

    // nothing can change a profile somebody has open, and a team profile is not deleted by accident
    assert!(ben.app.set_remark(&id, "mine now").is_err(), "no edits while it is open elsewhere");
    match ben.app.delete_profiles(std::slice::from_ref(&id)) {
        Err(BackendError::Invalid(m)) => assert!(m.contains("team hub"), "{m}"),
        other => panic!("a plain delete must refuse a team profile, got {other:?}"),
    }
    assert!(ben.try_row("shop-1").is_some());

    // the browser works: a cookie, and a cache that must not travel
    anna.browse(&id, "Default/Cookies", "session=anna-1");
    anna.browse(&id, "Default/Cache/blob", &"x".repeat(4096));
    anna.app.close(&id).expect("close: the data is on the hub and the lock is back when this returns");
    assert!(!anna.running(&id));
    assert!(matches!(anna.state("shop-1"), RunState::Closed), "{:?}", anna.state("shop-1"));

    // so ben can open it the moment close returns, and anna's cookie is there, her cache is not
    ben.app.open(&id, &headless()).expect("open on the second device");
    assert_eq!(ben.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1"));
    assert!(ben.read(&id, "Default/Cache/blob").is_none(), "caches stay home");
    assert!(
        matches!(anna.app.open(&id, &headless()), Err(OpenError::LockedElsewhere { .. })),
        "and anna is the one refused now"
    );
    ben.browse(&id, "Default/Cookies", "session=anna-1\nsession=ben-2");
    ben.app.close(&id).expect("close on the second device");

    // back on anna's: ben's work came with it
    anna.app.open(&id, &headless()).expect("open again");
    assert_eq!(anna.read(&id, "Default/Cookies").as_deref(), Some("session=anna-1\nsession=ben-2"));
    anna.app.close(&id).expect("close");

    // delete for everybody: it leaves the hub too, and the other device is told on its next pull
    anna.app.delete_profiles_everywhere(std::slice::from_ref(&id)).expect("delete for everyone");
    assert!(anna.try_row("shop-1").is_none());
    let said = ben.app.pull_team().expect("pull");
    assert!(said.contains("gone from the hub"), "{said}");
}

#[test]
fn a_lost_lock_stops_the_browser_and_is_reported_once() {
    let _turn = serial();
    let w = World::new();
    let mac = w.device("Anna's Mac", &w.anna);
    let pc = w.device("Anna's PC", &w.anna);
    mac.make("shop-2");
    let id = {
        let local = mac.id("shop-2");
        mac.app.share_profiles(&[local]).expect("share");
        mac.id("shop-2")
    };
    pc.app.pull_team().expect("pull");

    mac.app.open(&id, &headless()).expect("open");
    // the other device is the one that never lists in this test: the finished sessions are per process
    assert!(pc.app.force_unlock(&id).is_ok(), "an admin may break a lock");

    wait_until("the browser is stopped because it lost its lock", 25, || !mac.running(&id));
    let mut notices = Vec::new();
    wait_until("the app has something to say about it", 25, || {
        notices.extend(mac.app.take_notices());
        !notices.is_empty()
    });
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_ne!(notices[0].level, Level::Info, "{notices:?}");
    assert!(notices[0].text.contains("shop-2"), "{notices:?}");
    assert!(mac.app.take_notices().is_empty(), "shown once");
    assert!(
        matches!(mac.state("shop-2"), RunState::Closed | RunState::LockedElsewhere { .. }),
        "{:?}",
        mac.state("shop-2")
    );
}

#[test]
fn a_disconnected_device_keeps_the_profiles_but_cannot_open_them() {
    let _turn = serial();
    let w = World::new();
    let mac = w.device("Anna's Mac", &w.anna);
    mac.make("shop-3");
    let local = mac.id("shop-3");
    mac.app.share_profiles(&[local]).expect("share");
    let id = mac.id("shop-3");

    let status = mac.app.disconnect_hub().expect("disconnect");
    assert!(!status.connected, "{status:?}");
    assert!(!mac.app.hub_status().connected);
    assert!(mac.try_row("shop-3").is_some(), "the profile stays");
    match mac.app.open(&id, &headless()) {
        Err(OpenError::Other(m)) => assert!(m.contains("not connected") && m.contains("Teams page"), "{m}"),
        other => panic!("a team profile must not open without the hub, got {other:?}"),
    }
    assert!(!mac.running(&id));

    // connecting again gives it back
    mac.app.connect_hub(&w.url(), &w.anna, "Anna's Mac", "").expect("reconnect");
    mac.app.open(&id, &headless()).expect("open");
    mac.app.close(&id).expect("close");
}

#[test]
fn a_wrong_token_is_refused_in_plain_words_and_never_echoed() {
    let _turn = serial();
    let w = World::new();
    let d = w.fresh_device("Nobody's PC");
    let bad = "citro_this-is-not-a-token";
    let err = d.app.connect_hub(&w.url(), bad, "Nobody's PC", "").expect_err("refused");
    let text = err.to_string();
    assert!(!text.contains(bad), "the token must not come back in a message: {text}");
    assert!(!d.app.hub_status().connected);
    assert!(d.app.hub_status().url.is_empty());
}
