//! End-to-end tests: a real hub in this process, listening on `127.0.0.1:0`, driven over real HTTP by the typed
//! [`Client`] (one per device) and, where the wire format itself is the point, by raw requests.

mod common;

use citro_core::profile::Profile;
use citro_hub::client::{Client, Device, Error, LeaseKeeper, Pulled, UploadOptions};
use citro_hub::model::{LogEntry, LogFilter, LogKind, LogLevel, MASK, ProfileFilter, TeamRole, UserRole};
use citro_hub::server::admin;
use citro_hub::server::store::Config;
use common::*;
use serde_json::json;
use std::sync::{Barrier, mpsc};
use std::time::{Duration, Instant};

fn refused<T>(r: Result<T, Error>) -> Error {
    r.err().expect("the hub should have refused this")
}

/// Assert an error of the given shape; evaluates to the error.
macro_rules! assert_err {
    ($result:expr, $pattern:pat) => {{
        let e = refused($result);
        assert!(matches!(e, $pattern), "unexpected error: {e}");
        e
    }};
}

fn expect_locked(err: Error, holder: &str, device: &str) {
    match err {
        Error::Locked { holder: h, device: d, since, expires_at } => {
            assert_eq!((h.as_str(), d.as_str()), (holder, device));
            assert!(since > 0 && expires_at >= since, "the answer says since when and until when");
        }
        other => panic!("expected `locked` by {holder} on {device}, got: {other}"),
    }
}

fn named(name: &str) -> Profile {
    Profile { name: name.to_owned(), ..Default::default() }
}

// ---- the lock ---------------------------------------------------------------------------------------------------

#[test]
fn a_locked_profile_is_closed_to_every_other_device_and_user() {
    let w = World::new();
    let pid = w.profile("Shop");
    let a = w.ben();
    let b = w.cara();
    let a2 = a.with_device(Device::new("dev-ben-2", "Ben's phone")); // same user, same token, other device
    let owner = w.anna();

    let lease = a.lock(&pid).expect("A gets the lock");
    assert_eq!(lease.profile_id, pid);
    assert!(!lease.lease.is_empty() && lease.ttl_secs >= 1);

    // everybody in the team sees who has it open, and where (never the lease)
    let lock = b.profile(&pid, None).unwrap().lock.expect("the lock is shown");
    assert_eq!(
        (lock.user.as_str(), lock.device_id.as_str(), lock.device_name.as_str()),
        ("ben", "dev-ben", "Ben's laptop")
    );

    for who in [&b, &a2, &owner] {
        let name = who.device().name.clone();
        let ctx = |what: &str| format!("{what} by {name}");
        expect_locked(refused(who.lock(&pid)), "ben", "Ben's laptop");
        expect_locked(refused(who.download_data(&pid, "")), "ben", "Ben's laptop"); // reading is refused too
        expect_locked(refused(who.upload_data(&pid, "", b"stolen", &UploadOptions::default())), "ben", "Ben's laptop");
        expect_locked(refused(who.heartbeat(&pid, "")), "ben", "Ben's laptop");
        expect_locked(refused(who.unlock(&pid, "", true)), "ben", "Ben's laptop");
        expect_locked(refused(who.patch_profile(&pid, &json!({ "remark": "hijacked" }), None)), "ben", "Ben's laptop");
        // a made-up lease is no better, and the answer says who really holds it
        match refused(who.upload_data(&pid, "ls_guess", b"stolen", &UploadOptions::default())) {
            Error::LeaseLost { holder: Some(h), .. } => assert!(h.contains("ben"), "{}", ctx("holder named")),
            other => panic!("{}: {other}", ctx("made-up lease")),
        }
        assert_err!(who.heartbeat(&pid, "ls_guess"), Error::LeaseLost { .. });
        assert_err!(who.unlock(&pid, "ls_guess", true), Error::LeaseLost { .. });
    }
    expect_locked(refused(owner.delete_profile(&pid)), "ben", "Ben's laptop");

    // nothing of that got through: no data, no edit, and A is still the holder
    let seen = a.profile(&pid, Some(&lease.lease)).unwrap();
    assert_eq!((seen.data.version, seen.profile.remark.as_str()), (0, ""));
    a.heartbeat(&pid, &lease.lease).expect("A keeps its lease");
    assert_eq!(a.lock(&pid).map(|l| l.profile_id).unwrap_or_default(), pid, "the same device may re-lock");
}

#[test]
fn a_restarted_client_gets_its_own_lock_back_with_a_fresh_lease() {
    let w = World::new();
    let pid = w.profile("Shop");
    let a = w.ben();
    let first = a.lock(&pid).unwrap();
    let second = a.lock(&pid).expect("same user + same device id = the same holder, after a crash");
    assert_ne!(first.lease, second.lease);
    assert_err!(a.heartbeat(&pid, &first.lease), Error::LeaseLost { .. });
    a.heartbeat(&pid, &second.lease).unwrap();
    expect_locked(refused(w.cara().lock(&pid)), "ben", "Ben's laptop");
}

#[test]
fn data_survives_a_handover_and_arrives_with_the_same_sha256() {
    let w = World::new();
    let pid = w.profile("Shop");
    let (a, b) = (w.ben(), w.cara());

    let lease = a.lock(&pid).unwrap();
    assert!(a.download_data(&pid, &lease.lease).unwrap().is_none(), "nothing is stored yet");
    let v1 = blob(200_000, 1);
    let sha = sha256_hex(&v1);
    let meta =
        a.upload_data(&pid, &lease.lease, &v1, &UploadOptions { only_if_new: true, ..Default::default() }).unwrap();
    assert_eq!((meta.version, meta.size, meta.sha256.as_str()), (1, 200_000, sha.as_str()));
    let beat = a.heartbeat(&pid, &lease.lease).unwrap();
    assert!(beat.expires_at >= lease.expires_at);
    a.unlock(&pid, &lease.lease, true).unwrap();

    // bookkeeping: free again, last opened and closed by ben
    let p = b.profile(&pid, None).unwrap();
    assert!(p.lock.is_none());
    assert_eq!((p.last_open_by.as_str(), p.last_close_by.as_str()), ("ben", "ben"));
    assert!(p.profile.last_open_time > 0 && p.profile.last_close_time >= p.profile.last_open_time);
    assert_eq!((p.data.version, p.data.sha256.as_str()), (1, sha.as_str()));

    // B opens it and gets exactly what A left
    let l2 = b.lock(&pid).unwrap();
    let dl = b.download_data(&pid, &l2.lease).unwrap().expect("v1");
    assert_eq!((dl.version, dl.sha256.as_str()), (1, sha.as_str()));
    assert_eq!(sha256_hex(&dl.data), sha, "the SHA-256 of the received bytes matches");
    assert_eq!(dl.data, v1);
    assert_eq!(b.download_data_if_changed(&pid, &l2.lease, &sha).unwrap(), Pulled::Unchanged { version: 1 });

    // A is out: its old lease is dead and the profile names the new holder
    match refused(a.heartbeat(&pid, &lease.lease)) {
        Error::LeaseLost { holder: Some(h), .. } => assert!(h.contains("cara")),
        other => panic!("{other}"),
    }
    expect_locked(refused(a.lock(&pid)), "cara", "Cara's desktop");

    // B writes v2 and gives the profile back without having closed the browser (an aborted open)
    let v2 = blob(1000, 2);
    assert_eq!(b.upload_data(&pid, &l2.lease, &v2, &UploadOptions::default()).unwrap().version, 2);
    b.unlock(&pid, &l2.lease, false).unwrap();
    let p = a.profile(&pid, None).unwrap();
    assert_eq!((p.last_open_by.as_str(), p.last_close_by.as_str()), ("cara", "ben"));
}

#[test]
fn a_lease_that_is_not_renewed_frees_the_profile_by_itself() {
    let w = World::new();
    let pid = w.profile("Shop");
    let (a, b) = (w.ben(), w.cara());
    let lease = a.lock_with_ttl(&pid, Duration::from_millis(500)).unwrap();
    assert_eq!(lease.ttl_ms, 500);
    expect_locked(refused(b.lock(&pid)), "ben", "Ben's laptop");

    std::thread::sleep(Duration::from_millis(900));
    assert!(b.profile(&pid, None).unwrap().lock.is_none(), "an expired lock is not shown any more");
    b.lock(&pid).expect("nobody unlocked and nobody asked: it is simply free");

    // the old holder finds out the hard way
    assert_err!(a.heartbeat(&pid, &lease.lease), Error::LeaseLost { .. });
    assert_err!(a.upload_data(&pid, &lease.lease, b"late", &UploadOptions::default()), Error::LeaseLost { .. });
    assert_err!(a.unlock(&pid, &lease.lease, true), Error::LeaseLost { .. });

    // and the log says it was a takeover
    let log = w.anna().logs(Some(&w.team), &LogFilter { profile_id: Some(pid.clone()), ..Default::default() }).unwrap();
    assert!(log.iter().any(|r| r.message.contains("took over the expired lock of ben")), "{log:?}");
}

#[test]
fn force_unlock_breaks_the_lease_and_leaves_an_audit_line() {
    let w = World::new();
    let pid = w.profile("Shop");
    let (a, admin_client, other) = (w.ben(), w.anna(), w.cara());
    let lease = a.lock(&pid).unwrap();

    assert_err!(other.force_unlock(&pid), Error::Forbidden(_));
    assert_err!(a.force_unlock(&pid), Error::Forbidden(_)); // not even the holder
    a.heartbeat(&pid, &lease.lease).expect("a refused force-unlock changes nothing");

    assert!(admin_client.force_unlock(&pid).unwrap(), "there was a live lock to break");
    // the old lease fails everywhere, at once
    assert_err!(a.heartbeat(&pid, &lease.lease), Error::LeaseLost { .. });
    assert_err!(a.upload_data(&pid, &lease.lease, b"zombie", &UploadOptions::default()), Error::LeaseLost { .. });
    assert_err!(a.unlock(&pid, &lease.lease, true), Error::LeaseLost { .. });
    assert!(!admin_client.force_unlock(&pid).unwrap(), "nothing left to break the second time");

    other.lock(&pid).expect("the profile is free");
    let log =
        admin_client.logs(Some(&w.team), &LogFilter { profile_id: Some(pid.clone()), ..Default::default() }).unwrap();
    let audit = log
        .iter()
        .find(|r| r.message.contains("force-unlock by anna") && r.message.contains("broken"))
        .expect("an audit line");
    assert_eq!(audit.level, LogLevel::Warn);
    assert!(audit.message.contains("ben on Ben's laptop"), "{}", audit.message);
    let noop = log.iter().find(|r| r.message.contains("was not locked")).expect("the second attempt is on record too");
    assert_eq!(noop.level, LogLevel::Info, "breaking nothing is not worth a warning");
}

#[test]
fn viewers_may_look_but_not_open_and_outsiders_see_nothing() {
    let w = World::new();
    let pid = w.profile("Shop");
    let v = w.vera();

    assert_err!(v.lock(&pid), Error::Forbidden(_));
    assert_err!(v.download_data(&pid, ""), Error::Forbidden(_));
    assert_err!(v.upload_data(&pid, "", b"x", &UploadOptions::default()), Error::Forbidden(_));
    assert_err!(v.create_profile(&w.team, &named("nope")), Error::Forbidden(_));
    assert_err!(v.patch_profile(&pid, &json!({ "remark": "x" }), None), Error::Forbidden(_));
    assert_err!(v.force_unlock(&pid), Error::Forbidden(_));
    assert!(v.profile(&pid, None).unwrap().lock.is_none(), "the viewer's attempts locked nothing");
    assert_eq!(v.profiles(&w.team, &ProfileFilter::default()).unwrap().total, 1, "but looking is fine");
    assert!(v.export_csv(&w.team, None).is_ok());
    assert!(v.logs(Some(&w.team), &LogFilter::default()).is_ok());

    // somebody who is in no team gets "not found" for everything: the team does not even exist for them
    let carl = admin::create_user(&w.data, "carl", UserRole::Member, None).unwrap().token;
    let c = w.client(&carl, "dev-carl", "Carl");
    assert!(c.teams().unwrap().is_empty());
    assert_err!(c.profile(&pid, None), Error::NotFound(_));
    assert_err!(c.lock(&pid), Error::NotFound(_));
    assert_err!(c.profiles(&w.team, &ProfileFilter::default()), Error::NotFound(_));
    assert_err!(c.export_csv(&w.team, None), Error::NotFound(_));
    assert_err!(c.logs(Some(&w.team), &LogFilter::default()), Error::NotFound(_));
    assert_err!(c.create_profile(&w.team, &named("x")), Error::NotFound(_));
}

#[test]
fn wrong_missing_and_malformed_tokens_are_401() {
    let w = World::new();
    let bad = format!("citro_{}", "0".repeat(64));
    let c = w.client(&bad, "dev-x", "X");
    assert_err!(c.me(), Error::Unauthorized(_));
    assert_err!(c.teams(), Error::Unauthorized(_));
    assert_err!(c.lock("kanything"), Error::Unauthorized(_));

    let url = format!("{}/v1/me", w.url());
    let wrong = format!("Bearer {bad}");
    let cases: [&[(&str, &str)]; 4] =
        [&[], &[("Authorization", "Basic YWJjOmRlZg==")], &[("Authorization", "Bearer")], &[("Authorization", &wrong)]];
    for headers in cases {
        let r = raw("GET", &url, None, headers, None);
        assert_eq!(r.status, 401, "{headers:?}");
        assert_eq!(r.json()["error"]["code"], "unauthorized");
        assert!(!r.text().contains(&bad[6..]), "the answer never echoes the offered token");
    }
    assert_eq!(raw("GET", &format!("{}/healthz", w.url()), None, &[], None).status, 200, "healthz needs no token");
    assert_eq!(w.ben().me().unwrap().user.name, "ben", "the real token still works");
}

#[test]
fn locks_survive_a_restart_until_they_expire() {
    let mut w = World::new();
    let pid = w.profile("Shop");
    let lease = w.ben().lock_with_ttl(&pid, Duration::from_millis(2500)).unwrap();

    w.restart();
    let (a, b) = (w.ben(), w.cara());
    expect_locked(refused(b.lock(&pid)), "ben", "Ben's laptop");
    assert!(b.profile(&pid, None).unwrap().lock.is_some());
    a.heartbeat(&pid, &lease.lease).expect("the lease of the old process is honoured by the new one");

    std::thread::sleep(Duration::from_millis(2900));
    let l2 = b.lock(&pid).expect("expired meanwhile: free");
    assert_err!(a.heartbeat(&pid, &lease.lease), Error::LeaseLost { .. });

    // a lock with the default lifetime and a pushed blob survive a second restart as well
    let data = blob(5000, 7);
    b.upload_data(&pid, &l2.lease, &data, &UploadOptions::default()).unwrap();
    w.restart();
    let (a, b) = (w.ben(), w.cara());
    expect_locked(refused(a.lock(&pid)), "cara", "Cara's desktop");
    assert_err!(a.download_data(&pid, &lease.lease), Error::LeaseLost { .. });
    let again = b.download_data(&pid, &l2.lease).unwrap().expect("the data is still there");
    assert_eq!(again.data, data);
    b.unlock(&pid, &l2.lease, true).unwrap();
    w.restart();
    w.ben().lock(&pid).expect("the unlock was persisted too");
}

#[test]
fn concurrent_acquire_has_exactly_one_winner() {
    const THREADS: usize = 16;
    let w = World::new();
    for round in 0..5 {
        let pid = w.profile(&format!("race-{round}"));
        let clients: Vec<Client> = (0..THREADS)
            .map(|i| {
                let token = if i % 2 == 0 { &w.ben } else { &w.cara }; // two users, many devices
                w.client(token, &format!("dev-{i}"), &format!("Device {i}"))
            })
            .collect();
        let barrier = Barrier::new(THREADS);
        let results: Vec<Result<_, Error>> = std::thread::scope(|s| {
            let handles: Vec<_> = clients
                .iter()
                .map(|c| {
                    let (barrier, pid) = (&barrier, &pid);
                    s.spawn(move || {
                        barrier.wait();
                        c.lock(pid)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect()
        });
        let winners: Vec<usize> = results.iter().enumerate().filter(|(_, r)| r.is_ok()).map(|(i, _)| i).collect();
        assert_eq!(winners.len(), 1, "round {round}: winners {winners:?}");
        for (i, r) in results.iter().enumerate().filter(|(i, _)| *i != winners[0]) {
            assert!(matches!(r, Err(Error::Locked { .. })), "round {round}, thread {i}: not `locked`");
        }
        // everybody agrees on who won
        let lock = w.anna().profile(&pid, None).unwrap().lock.expect("locked");
        assert_eq!(lock.device_id, format!("dev-{}", winners[0]));
        if let Err(Error::Locked { holder, device, .. }) = &results[(winners[0] + 1) % THREADS] {
            assert_eq!((holder.as_str(), device.as_str()), (lock.user.as_str(), lock.device_name.as_str()));
        }
    }
}

#[test]
fn the_lease_keeper_heartbeats_and_reports_a_lost_lease() {
    let w = World::new();
    let pid = w.profile("Shop");
    let (a, b, admin_client) = (w.ben(), w.cara(), w.anna());

    let lease = a.lock_with_ttl(&pid, Duration::from_millis(600)).unwrap();
    let keeper = LeaseKeeper::start(&a, &pid, &lease);
    std::thread::sleep(Duration::from_millis(1900)); // three lifetimes
    assert!(keeper.is_held(), "{:?}", keeper.loss());
    assert!(keeper.heartbeats() >= 3, "heartbeats: {}", keeper.heartbeats());
    expect_locked(refused(b.lock(&pid)), "ben", "Ben's laptop"); // still ours, long after the first ttl

    assert!(admin_client.force_unlock(&pid).unwrap());
    let loss = keeper.wait_lost(Duration::from_secs(5)).expect("the keeper notices the loss");
    assert!(loss.confirmed_by_hub, "{loss:?}");
    assert!(!keeper.is_held());
    assert!(!loss.reason.contains(&lease.lease), "the reason never carries the secret");
    keeper.stop();

    // the callback flavour, and a clean release that stamps "last closed"
    let l2 = b.lock_with_ttl(&pid, Duration::from_millis(600)).unwrap();
    let (tx, rx) = mpsc::channel();
    let keeper = LeaseKeeper::start_with(&b, &pid, &l2, move |loss| {
        let _ = tx.send(loss.confirmed_by_hub);
    });
    assert!(admin_client.force_unlock(&pid).unwrap());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
    drop(keeper);

    let l3 = a.lock_with_ttl(&pid, Duration::from_millis(600)).unwrap();
    expect_locked(refused(b.lock_and_keep(&pid)), "ben", "Ben's laptop"); // no keeper for somebody else's lock
    let keeper = LeaseKeeper::start(&a, &pid, &l3);
    std::thread::sleep(Duration::from_millis(700));
    keeper.release(true).expect("released with the lease in hand");
    let p = b.profile(&pid, None).unwrap();
    assert!(p.lock.is_none());
    assert_eq!(p.last_close_by, "ben");
    b.lock(&pid).expect("free for the next one");
}

// ---- the data ---------------------------------------------------------------------------------------------------

#[test]
fn data_versions_are_monotonic_conditional_capped_and_pruned() {
    let cfg = Config { max_blob_bytes: 64 * 1024, keep_versions: 2, ..test_config() };
    let w = World::with(cfg);
    let pid = w.profile("Shop");
    let a = w.ben();
    let lease = a.lock(&pid).unwrap();
    assert_eq!(lease.max_data_bytes, 64 * 1024, "the lease tells the client the size limit");

    let blobs: Vec<Vec<u8>> = (1..=5u64).map(|i| blob(1000 * i as usize, i)).collect();
    for (i, data) in blobs.iter().take(4).enumerate() {
        let meta = a.upload_data(&pid, &lease.lease, data, &UploadOptions::default()).unwrap();
        assert_eq!(meta.version, i as u64 + 1, "versions count up by one");
    }

    // optimistic concurrency: every condition that does not hold refuses the push and stores nothing
    let cur = sha256_hex(&blobs[3]);
    match refused(a.upload_data(
        &pid,
        &lease.lease,
        &blobs[4],
        &UploadOptions { base_version: Some(2), ..Default::default() },
    )) {
        Error::Conflict { current_version, .. } => assert_eq!(current_version, Some(4)),
        other => panic!("{other}"),
    }
    let wrong_hash = UploadOptions { if_match: Some(sha256_hex(b"something else")), ..Default::default() };
    assert_err!(a.upload_data(&pid, &lease.lease, &blobs[4], &wrong_hash), Error::Conflict { .. });
    let first_push = UploadOptions { only_if_new: true, ..Default::default() };
    assert_err!(a.upload_data(&pid, &lease.lease, &blobs[4], &first_push), Error::Conflict { .. });
    assert_eq!(a.profile(&pid, Some(&lease.lease)).unwrap().data.version, 4);
    let right = UploadOptions { base_version: Some(4), if_match: Some(cur), ..Default::default() };
    assert_eq!(a.upload_data(&pid, &lease.lease, &blobs[4], &right).unwrap().version, 5);

    // retention: the newest two stay, older ones are gone
    let versions = a.data_versions(&pid).unwrap();
    assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), [5, 4]);
    assert!(versions[0].current && !versions[1].current);
    assert_eq!(a.download_data_version(&pid, &lease.lease, 4).unwrap().data, blobs[3]);
    assert_err!(a.download_data_version(&pid, &lease.lease, 3), Error::NotFound(_));
    assert_err!(a.download_data_version(&pid, &lease.lease, 9), Error::NotFound(_));

    // the size cap
    assert_err!(
        a.upload_data(&pid, &lease.lease, &blob(64 * 1024 + 1, 9), &UploadOptions::default()),
        Error::TooLarge(_)
    );
    assert_eq!(a.profile(&pid, Some(&lease.lease)).unwrap().data.version, 5, "a refused push stores nothing");
    assert_eq!(a.upload_data(&pid, &lease.lease, &blob(64 * 1024, 9), &UploadOptions::default()).unwrap().version, 6);

    // files in and out, verified against the hash on the way
    let dir = w.tmp.path();
    let (src, dst) = (dir.join("archive.bin"), dir.join("pulled.bin"));
    std::fs::write(&src, blob(30_000, 11)).unwrap();
    let meta = a.upload_data_file(&pid, &lease.lease, &src, &UploadOptions::default()).unwrap();
    assert_eq!(meta.version, 7);
    let fetched = a.download_data_to_file(&pid, &lease.lease, &dst).unwrap().expect("data");
    assert_eq!((fetched.version, fetched.size, fetched.sha256.as_str()), (7, 30_000, meta.sha256.as_str()));
    assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&src).unwrap());
    assert!(
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains(".part"))
    );
}

#[test]
fn the_raw_http_contract_works_with_plain_curl_style_requests() {
    let w = World::new();
    let pid = w.profile("Shop");
    let url = w.url();
    let json_h = [("Content-Type", "application/json")];
    let lock = |token: &str, device: &str, name: &str| {
        let body = json!({ "device_id": device, "device_name": name }).to_string();
        raw("POST", &format!("{url}/v1/profiles/{pid}/lock"), Some(token), &json_h, Some(body.as_bytes()))
    };

    // lock #1 wins; lock #2 uses the SAME token from another device and is refused, naming the holder
    let r1 = lock(&w.ben, "dev-a", "Ben's laptop");
    assert_eq!(r1.status, 200, "{}", r1.text().len());
    let lease = r1.json()["lease"].as_str().expect("lease").to_owned();
    let r2 = lock(&w.ben, "dev-b", "Ben's phone");
    assert_eq!(r2.status, 409);
    let e = r2.json();
    assert_eq!(e["error"]["code"], "locked");
    assert_eq!(
        (e["error"]["holder"]["user"].as_str(), e["error"]["holder"]["device_name"].as_str()),
        (Some("ben"), Some("Ben's laptop"))
    );
    assert!(!r2.text().contains(&lease), "an error never carries the lease");
    assert!(e["error"]["holder"].get("device_id").is_none());
    assert_eq!(r2.header("cache-control"), Some("no-store"));
    assert_eq!(r2.header("x-content-type-options"), Some("nosniff"));

    // the data endpoint: no lease = 409, wrong lease = 403, right lease = stored
    let data = blob(4096, 3);
    let sha = sha256_hex(&data);
    let data_url = format!("{url}/v1/profiles/{pid}/data");
    let put = |token: &str, lease: Option<&str>, extra: &[(&str, &str)]| {
        let mut h: Vec<(&str, &str)> = vec![("Content-Type", "application/octet-stream")];
        if let Some(l) = lease {
            h.push(("X-Citro-Lease", l));
        }
        h.extend_from_slice(extra);
        raw("PUT", &data_url, Some(token), &h, Some(&data))
    };
    assert_eq!(put(&w.ben, None, &[]).status, 409);
    assert_eq!(put(&w.ben, Some("ls_wrong"), &[]).status, 403);
    assert_eq!(put(&w.cara, Some(&lease), &[]).status, 403, "the lease is bound to its user");
    let damaged = put(&w.ben, Some(&lease), &[("X-Content-SHA256", &"0".repeat(64))]);
    assert!((400..500).contains(&damaged.status), "a declared hash that does not match is refused");
    assert_eq!(w.ben().profile(&pid, Some(&lease)).unwrap().data.version, 0, "nothing was stored");
    let ok = put(&w.ben, Some(&lease), &[("X-Content-SHA256", &sha)]);
    assert_eq!(ok.status, 200);
    assert_eq!((ok.json()["version"].as_u64(), ok.json()["sha256"].as_str()), (Some(1), Some(sha.as_str())));

    // reading: refused without the lease, then ETag / X-Content-SHA256 / version headers and conditional GET
    assert_eq!(raw("GET", &data_url, Some(&w.cara), &[], None).status, 409);
    let get = raw("GET", &data_url, Some(&w.ben), &[("X-Citro-Lease", &lease)], None);
    assert_eq!(get.status, 200);
    assert_eq!(get.body, data);
    assert_eq!(get.header("etag"), Some(format!("\"{sha}\"").as_str()));
    assert_eq!(get.header("x-content-sha256"), Some(sha.as_str()));
    assert_eq!(get.header("x-citro-data-version"), Some("1"));
    let etag = format!("\"{sha}\"");
    let cached = raw("GET", &data_url, Some(&w.ben), &[("X-Citro-Lease", &lease), ("If-None-Match", &etag)], None);
    assert_eq!((cached.status, cached.body.len()), (304, 0));

    // optimistic concurrency by If-Match
    let stale = put(&w.ben, Some(&lease), &[("If-Match", "\"deadbeef\"")]);
    assert_eq!(stale.status, 412);
    assert_eq!(stale.json()["error"]["code"], "stale");
    assert_eq!(put(&w.ben, Some(&lease), &[("If-Match", &etag)]).status, 200);

    // a few shape checks: unknown endpoint, wrong method
    assert_eq!(raw("GET", &format!("{url}/v1/nothing-here"), Some(&w.ben), &[], None).status, 404);
    assert_eq!(raw("DELETE", &format!("{url}/v1/me"), Some(&w.ben), &[], None).status, 405);
    let body = json!({ "closed": true }).to_string();
    let unlock = raw(
        "POST",
        &format!("{url}/v1/profiles/{pid}/unlock"),
        Some(&w.ben),
        &[("X-Citro-Lease", &lease), json_h[0]],
        Some(body.as_bytes()),
    );
    assert_eq!(unlock.status, 200);
    assert_eq!(lock(&w.ben, "dev-b", "Ben's phone").status, 200, "now the other device may have it");
}

// ---- logs -------------------------------------------------------------------------------------------------------

#[test]
fn logs_are_scrubbed_filtered_and_rotated_on_disk() {
    let cfg = Config { log_max_bytes: 4096, log_files: 3, ..test_config() };
    let w = World::with(cfg);
    let pid = w.profile("Shop");
    let a = w.ben();
    let fake_token = format!("citro_{}", "ab".repeat(32));

    let pushed = a
        .push_logs(&[
            LogEntry::now(
                LogKind::Browser,
                LogLevel::Warn,
                Some(&pid),
                format!("renderer crashed after Authorization: Bearer {fake_token}"),
            ),
            LogEntry::now(
                LogKind::Browser,
                LogLevel::Error,
                Some(&pid),
                "gpu process lost, password=hunter2 proxy=http://user:s3cret@10.0.0.1:8080/",
            ),
            LogEntry::now(LogKind::Citro, LogLevel::Info, None, "profile list refreshed"),
            LogEntry::now(LogKind::Citro, LogLevel::Info, None, "x".repeat(100_000)),
        ])
        .unwrap();
    assert_eq!(pushed, 4);

    let all = a.logs(Some("acme"), &LogFilter::default()).unwrap(); // the team by (case-insensitive) name
    let text = serde_json::to_string(&all).unwrap();
    for secret in [&fake_token[6..], "hunter2", "s3cret"] {
        assert!(!text.contains(secret), "a secret survived scrubbing");
    }
    assert!(all.iter().all(|r| r.message.chars().count() <= 4001), "messages are capped");
    assert!(all.iter().all(|r| r.team_id == w.team));
    assert!(all.windows(2).all(|p| p[0].ts >= p[1].ts), "newest first");

    // a client that does not scrub (plain curl): the hub does it
    let body = json!({
        "device_id": "dev-curl",
        "entries": [{ "ts": now_secs(), "kind": "browser", "level": "info", "profile_id": pid,
                      "message": "raw client: password=rawsecret, Bearer rawbearer, https://u:rawpw@h.example/" }]
    })
    .to_string();
    let posted = raw(
        "POST",
        &format!("{}/v1/logs", w.url()),
        Some(&w.ben),
        &[("Content-Type", "application/json")],
        Some(body.as_bytes()),
    );
    assert_eq!(posted.status, 200, "{}", posted.text());
    let all = a.logs(Some("acme"), &LogFilter::default()).unwrap();
    let curl_line = all.iter().find(|r| r.device_id == "dev-curl").expect("the raw line is stored");
    assert_eq!(curl_line.message, "raw client: password=********, Bearer ********, https://u:********@h.example/");
    let dump = serde_json::to_string(&all).unwrap();
    assert!(["rawsecret", "rawbearer", "rawpw"].iter().all(|s| !dump.contains(s)), "the hub scrubs on its own");

    // filters: kind, level, profile, since, limit
    let kind = |k| LogFilter { kind: Some(k), ..Default::default() };
    let browser = a.logs(Some(&w.team), &kind(LogKind::Browser)).unwrap();
    assert_eq!(browser.len(), 3);
    let messages: Vec<&str> = browser.iter().map(|r| r.message.as_str()).collect();
    assert!(messages.contains(&"renderer crashed after Authorization: Bearer ********"), "{messages:?}");
    assert!(
        messages.contains(&"gpu process lost, password=******** proxy=http://user:********@10.0.0.1:8080/"),
        "{messages:?}"
    );
    assert!(browser.iter().all(|r| r.kind == LogKind::Browser && r.user == "ben"));
    assert_eq!(browser.iter().filter(|r| r.device_id == "dev-ben").count(), 2, "lines carry the device they came from");
    let errors = a
        .logs(
            Some(&w.team),
            &LogFilter { level: Some(LogLevel::Error), kind: Some(LogKind::Browser), ..Default::default() },
        )
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].profile_id.as_deref(), Some(pid.as_str()));
    let citro = a.logs(Some(&w.team), &kind(LogKind::Citro)).unwrap();
    assert!(
        citro.iter().any(|r| r.message == "profile list refreshed")
            && citro.iter().any(|r| r.message.contains("created"))
    );
    let of_profile = a.logs(Some(&w.team), &LogFilter { profile_id: Some(pid.clone()), ..Default::default() }).unwrap();
    assert!(of_profile.len() >= 3 && of_profile.iter().all(|r| r.profile_id.as_deref() == Some(pid.as_str())));
    assert!(
        a.logs(Some(&w.team), &LogFilter { since: Some(now_secs() + 3600), ..Default::default() }).unwrap().is_empty()
    );
    assert_eq!(a.logs(Some(&w.team), &LogFilter { limit: Some(1), ..Default::default() }).unwrap().len(), 1);
    // only the team's own people read them
    assert_err!(
        w.client(&admin::create_user(&w.data, "dan", UserRole::Member, None).unwrap().token, "d", "D")
            .logs(Some(&w.team), &LogFilter::default()),
        Error::NotFound(_)
    );

    // rotation: a flood of lines never grows the log beyond `log_files` files of about `log_max_bytes`
    let flood: Vec<LogEntry> = (0..300)
        .map(|i| {
            LogEntry::now(
                LogKind::Browser,
                LogLevel::Info,
                Some(&pid),
                format!("line {i}: tab navigated to https://example.test/page/{i}"),
            )
        })
        .collect();
    a.push_logs(&flood).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(w.data.join("logs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&w.team))
        .collect();
    files.sort();
    assert!(files.len() > 1 && files.len() <= 3, "rotated, and never more than 3 files: {files:?}");
    for f in &files {
        let len = std::fs::metadata(w.data.join("logs").join(f)).unwrap().len();
        assert!(len < 4096 + 600, "{f} is {len} bytes");
    }
    let newest = a
        .logs(Some(&w.team), &LogFilter { limit: Some(5), kind: Some(LogKind::Browser), ..Default::default() })
        .unwrap();
    assert!(newest.iter().all(|r| r.message.starts_with("line ")), "queries read the newest file first");
}

// ---- CSV --------------------------------------------------------------------------------------------------------

const HEADER: [&str; 24] = [
    "no",
    "id",
    "name",
    "group",
    "remark",
    "persona_os",
    "country",
    "proxy_type",
    "proxy_host",
    "proxy_port",
    "proxy_user",
    "language",
    "timezone",
    "tabs",
    "account_username",
    "created_at",
    "created_by",
    "last_open_at",
    "last_open_by",
    "last_close_at",
    "last_close_by",
    "status",
    "locked_by",
    "locked_device",
];

#[test]
fn the_csv_export_is_rfc_4180_and_python_can_read_it() {
    let w = World::new();
    let (anna, ben, vera) = (w.anna(), w.ben(), w.vera());
    let group = anna.create_group(&w.team, "Sales EU", "the european shops").unwrap();
    let nasty: Profile = serde_json::from_value(json!({
        "name": "Shop, \"main\"\nsecond line", "remark": "=HYPERLINK(\"http://evil.example\")", "username": "bob@example.com",
        "password": "pw-secret", "fakey": "2FA-SECRET", "cookie": "COOKIE-SECRET", "group_id": group.id,
        "tabs": ["https://a.example/", "https://u:tabsecret@b.example/x"], "country": "DE", "persona_os": "windows",
        "user_proxy_config": { "proxy_type": "socks5", "proxy_host": "1.2.3.4", "proxy_port": 1080, "proxy_user": "pu", "proxy_password": "px-secret" },
        "fingerprint_config": { "language": ["de-DE", "en-US"], "timezone": "Europe/Berlin" }
    }))
    .unwrap();
    let p1 = anna.create_profile(&w.team, &nasty).unwrap().profile.profile_id;
    anna.create_profile(
        &w.team,
        &Profile { name: "Plain".into(), remark: "second, ungrouped".into(), ..Default::default() },
    )
    .unwrap();
    let lease = ben.lock(&p1).unwrap();
    ben.unlock(&p1, &lease.lease, true).unwrap();
    let lease = ben.lock(&p1).unwrap(); // open right now

    let bytes = vera.export_csv(&w.team, None).expect("a viewer may export");
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "UTF-8 BOM, so that Excel opens it right");
    let text = String::from_utf8(bytes.clone()).expect("UTF-8");
    assert!(text.ends_with("\r\n") && !text.ends_with("\r\n\r\n"));
    for secret in [
        "pw-secret",
        "2FA-SECRET",
        "COOKIE-SECRET",
        "px-secret",
        "tabsecret",
        lease.lease.as_str(),
        w.ben.as_str(),
        w.anna.as_str(),
    ] {
        assert!(!text.contains(secret), "a secret leaked into the CSV");
    }

    let rows = python_csv_rows(w.tmp.path(), &bytes);
    assert_eq!(rows[0], HEADER, "the header is the documented contract");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.len() == 24), "every row has 24 cells");
    let row = rows.iter().find(|r| r[1] == p1).expect("the nasty profile");
    let col = |name: &str| HEADER.iter().position(|h| *h == name).unwrap();
    assert_eq!(row[col("name")], "Shop, \"main\"\nsecond line", "commas, quotes and newlines survive");
    assert_eq!(row[col("remark")], "'=HYPERLINK(\"http://evil.example\")", "formulas are neutralised");
    assert_eq!(row[col("group")], "Sales EU");
    assert_eq!((row[col("persona_os")].as_str(), row[col("country")].as_str()), ("windows", "DE"));
    assert_eq!(
        (
            row[col("proxy_type")].as_str(),
            row[col("proxy_host")].as_str(),
            row[col("proxy_port")].as_str(),
            row[col("proxy_user")].as_str()
        ),
        ("socks5", "1.2.3.4", "1080", "pu")
    );
    assert_eq!((row[col("language")].as_str(), row[col("timezone")].as_str()), ("de-DE,en-US", "Europe/Berlin"));
    assert_eq!(row[col("tabs")], format!("https://a.example/ | https://u:{MASK}@b.example/x"));
    assert_eq!(row[col("account_username")], "bob@example.com");
    assert_eq!(
        (row[col("created_by")].as_str(), row[col("last_open_by")].as_str(), row[col("last_close_by")].as_str()),
        ("anna", "ben", "ben")
    );
    assert_eq!(
        (row[col("status")].as_str(), row[col("locked_by")].as_str(), row[col("locked_device")].as_str()),
        ("open", "ben", "Ben's laptop")
    );
    for name in ["created_at", "last_open_at", "last_close_at"] {
        let v = &row[col(name)];
        assert!(v.len() == 20 && v.ends_with('Z') && v.as_bytes()[10] == b'T', "{name} = {v:?} is ISO-8601 UTC");
    }
    let plain = rows.iter().find(|r| r[col("name")] == "Plain").unwrap();
    assert_eq!(
        (plain[col("status")].as_str(), plain[col("last_open_at")].as_str(), plain[col("locked_by")].as_str()),
        ("closed", "", "")
    );

    // one group, by name or by id; an unknown group is an error, not an empty file
    let by_name = python_csv_rows(w.tmp.path(), &vera.export_csv(&w.team, Some("Sales EU")).unwrap());
    assert_eq!(by_name.len(), 2);
    assert_eq!(by_name[1][1], p1);
    assert_eq!(vera.export_csv(&w.team, Some(&group.id)).unwrap(), vera.export_csv(&w.team, Some("Sales EU")).unwrap());
    assert_err!(vera.export_csv(&w.team, Some("No such group")), Error::Invalid(_));
    // and over the wire: the file name and type
    let r = raw("GET", &format!("{}/v1/teams/{}/profiles.csv", w.url(), w.team), Some(&w.vera), &[], None);
    assert_eq!(r.header("content-type"), Some("text/csv; charset=utf-8"));
    assert!(
        r.header("content-disposition")
            .is_some_and(|d| d.contains("attachment") && d.contains("-profiles-") && d.contains(".csv"))
    );
    let to_file = w.tmp.path().join("team.csv");
    vera.export_csv_to(&w.team, None, &to_file).unwrap();
    assert_eq!(std::fs::read(to_file).unwrap(), bytes);
}

// ---- brakes -----------------------------------------------------------------------------------------------------

#[test]
fn the_rate_limit_is_per_user_and_a_login_flood_blocks_the_address() {
    let cfg = Config { rate_limit_per_min: 5, auth_fail_limit: 3, ..test_config() };
    let w = World::with(cfg);
    let (ben, cara) = (w.ben(), w.cara());
    for _ in 0..5 {
        ben.me().unwrap();
    }
    match refused(ben.me()) {
        Error::RateLimited { retry_after } => assert!(retry_after.is_some_and(|d| d.as_secs() >= 1)),
        other => panic!("{other}"),
    }
    cara.me().expect("every user has a bucket of their own");

    // a flood of wrong tokens: three 401s, then the address is blocked, even for the right token
    let bad = format!("citro_{}", "1".repeat(64));
    let intruder = w.client(&bad, "dev-x", "X");
    for _ in 0..3 {
        assert_err!(intruder.me(), Error::Unauthorized(_));
    }
    assert_err!(w.anna().me(), Error::RateLimited { .. });
    let r = raw("GET", &format!("{}/v1/me", w.url()), Some(&w.anna), &[], None);
    assert_eq!(r.status, 429);
    assert!(r.header("retry-after").is_some_and(|v| v.parse::<u64>().is_ok_and(|s| s >= 1)));

    // the failures are on record, the offered token is not
    let log = std::fs::read_to_string(w.data.join("logs").join("_server.jsonl")).unwrap();
    assert!(log.contains("authentication failed from 127.0.0.1"), "{log}");
    assert!(log.contains("blocked"), "the third failure says the address is blocked");
    assert!(!log.contains(&bad[6..]) && !log.contains(&w.anna[6..]) && !log.contains(&w.ben[6..]));
}

// ---- offline administration against a running hub ---------------------------------------------------------------

#[test]
fn users_can_be_created_rotated_and_revoked_while_the_hub_runs() {
    let w = World::new();
    let pid = w.profile("Shop");
    let (anna, cara) = (w.anna(), w.cara());

    // created offline, accepted by the running hub on the next request
    let created = admin::create_user(&w.data, "dora", UserRole::Member, None).unwrap();
    let dora = w.client(&created.token, "dev-dora", "Dora's laptop");
    assert_eq!(dora.me().unwrap().user.name, "dora");
    assert!(dora.teams().unwrap().is_empty(), "no team yet: she sees nothing");
    anna.add_member(&w.team, "dora", TeamRole::Member).unwrap();
    assert_eq!(dora.teams().unwrap().len(), 1);
    let lease = dora.lock(&pid).unwrap();

    // a rotated token: the old one is dead at once, the new one works, her lock is hers still
    let (_, fresh) = admin::rotate_token(&w.data, "dora").unwrap();
    assert_err!(dora.me(), Error::Unauthorized(_));
    let dora2 = w.client(&fresh, "dev-dora", "Dora's laptop");
    dora2.heartbeat(&pid, &lease.lease).expect("the lease belongs to the user, not to the token");

    // revoked: dead, and her locks are released
    admin::revoke_user(&w.data, "dora").unwrap();
    assert_err!(dora2.me(), Error::Unauthorized(_));
    cara.lock(&pid).expect("a revoked user's locks are released");
    assert!(admin::revoke_user(&w.data, "anna").is_err(), "the last active admin cannot be revoked");
    assert!(admin::rotate_token(&w.data, "dora").is_err(), "revocation is final");

    // the user list (admins only) knows the state, and has no secrets in it
    assert_err!(w.ben().admin_users(), Error::Forbidden(_));
    let users = anna.admin_users().unwrap();
    assert!(users.iter().any(|u| u.name == "dora" && u.disabled));
    let dump = serde_json::to_string(&users).unwrap();
    assert!(!dump.contains(&created.token[6..]) && !dump.contains(&fresh[6..]) && !dump.contains("hash"));
    let log = anna.admin_logs(&LogFilter::default()).unwrap();
    assert!(log.iter().all(|r| !r.message.contains(&fresh[6..])));
}

#[test]
fn memberships_are_not_written_offline_while_a_hub_serves() {
    let w = World::new();
    // Teams and members belong to the running server: an offline write would be lost at its next save, so the
    // admin commands refuse instead of pretending (a plain user is fine, the server adopts users from disk).
    for refused in [
        admin::create_user(&w.data, "eve", UserRole::Member, Some(("Acme", TeamRole::Member))).map(|_| ()),
        admin::create_team(&w.data, "Other", Some("anna")).map(|_| ()),
        admin::add_member(&w.data, &w.team, "ben", TeamRole::Viewer).map(|_| ()),
    ] {
        let text = refused.expect_err("must be refused while a hub serves this directory").to_string();
        assert!(text.contains("is serving") && text.contains("POST /v1/teams"), "{text}");
    }
    assert!(w.anna().admin_users().unwrap().iter().all(|u| u.name != "eve"), "a refused command changes nothing");
    assert_eq!(w.ben().teams().unwrap()[0].role, TeamRole::Member, "ben's role is untouched");
    // the API is the way while serving
    admin::create_user(&w.data, "eve", UserRole::Member, None).unwrap();
    w.anna().add_member(&w.team, "eve", TeamRole::Viewer).unwrap();
}

// ---- teams, groups, remarks -------------------------------------------------------------------------------------

#[test]
fn several_teams_groups_remarks_and_bookkeeping() {
    let w = World::new();
    let (anna, ben, cara) = (w.anna(), w.ben(), w.cara());

    let beta = anna.create_team("Beta").unwrap();
    assert_eq!(beta.role, TeamRole::Owner);
    assert_err!(ben.create_team("Nope"), Error::Forbidden(_));
    anna.add_member(&beta.team.id, "ben", TeamRole::Admin).unwrap();
    let mine = ben.me().unwrap().teams;
    let role_in = |name: &str| mine.iter().find(|t| t.team.name == name).map(|t| t.role);
    assert_eq!(
        (role_in("Acme"), role_in("Beta")),
        (Some(TeamRole::Member), Some(TeamRole::Admin)),
        "one user, two teams, two roles"
    );
    assert_eq!(cara.teams().unwrap().len(), 1);

    // groups and remarks in Beta
    let shops = ben.create_group(&beta.team.id, "Shops", "all the shops").unwrap();
    let p = ben
        .create_profile(
            &beta.team.id,
            &Profile {
                name: "B1".into(),
                remark: "first remark".into(),
                group_id: shops.id.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    let pid = p.profile.profile_id.clone();
    assert_eq!(
        (p.created_by.as_str(), p.team_id.as_str(), p.profile.group_id.as_str()),
        ("ben", beta.team.id.as_str(), shops.id.as_str())
    );
    ben.create_profile(&beta.team.id, &named("B2")).unwrap();

    let by_group =
        ben.profiles(&beta.team.id, &ProfileFilter { group: Some("Shops".into()), ..Default::default() }).unwrap();
    assert_eq!((by_group.total, by_group.profiles[0].profile.name.as_str()), (1, "B1"));
    let ungrouped =
        ben.profiles(&beta.team.id, &ProfileFilter { group: Some("0".into()), ..Default::default() }).unwrap();
    assert_eq!((ungrouped.total, ungrouped.profiles[0].profile.name.as_str()), (1, "B2"));
    let by_remark =
        ben.profiles(&beta.team.id, &ProfileFilter { q: Some("FIRST".into()), ..Default::default() }).unwrap();
    assert_eq!(by_remark.total, 1, "search is case-insensitive and covers remarks");
    let paged =
        ben.profiles(&beta.team.id, &ProfileFilter { limit: Some(1), offset: Some(1), ..Default::default() }).unwrap();
    assert_eq!((paged.total, paged.profiles.len()), (2, 1));

    // remarks can be edited while nobody has the profile open; serial numbers count per team
    let edited = ben.patch_profile(&pid, &json!({ "remark": "second remark" }), None).unwrap();
    assert_eq!(edited.profile.remark, "second remark");
    assert_eq!(p.profile.serial_number, 1);
    assert_eq!(w.anna().create_profile(&w.team, &named("A1")).unwrap().profile.serial_number, 1);

    // teams are walls: Cara is not in Beta
    assert_err!(cara.profile(&pid, None), Error::NotFound(_));
    assert_err!(cara.lock(&pid), Error::NotFound(_));
    assert_err!(cara.export_csv(&beta.team.id, None), Error::NotFound(_));
    assert_err!(cara.members(&beta.team.id), Error::NotFound(_));

    // deleting a group does not delete its profiles
    assert_eq!(ben.delete_group(&shops.id).unwrap(), 1);
    assert_eq!(ben.profile(&pid, None).unwrap().profile.group_id, "0");
    // deleting profiles needs an owner/admin of the team, and an unlocked profile
    assert_err!(cara.delete_profile(&pid), Error::NotFound(_));
    let lease = ben.lock(&pid).unwrap();
    expect_locked(refused(ben.delete_profile(&pid)), "ben", "Ben's laptop");
    ben.unlock(&pid, &lease.lease, true).unwrap();
    ben.delete_profile(&pid).unwrap();
    assert_err!(ben.profile(&pid, None), Error::NotFound(_));
    let left = ben.bulk_delete(&beta.team.id, &["knope".to_owned()]).unwrap();
    assert!(left.deleted.is_empty() && left.skipped.len() == 1);
}

// ---- the client in odd places -----------------------------------------------------------------------------------

#[test]
fn a_hub_that_is_down_is_a_network_error_and_the_lock_stays_where_it_was() {
    let mut w = World::new();
    let pid = w.profile("Shop");
    let a = w.ben();
    let lease = a.lock(&pid).unwrap();
    let started = Instant::now();
    w.hub.take().unwrap().stop();
    let e = assert_err!(a.heartbeat(&pid, &lease.lease), Error::Network(_));
    assert!(e.is_transient());
    assert!(started.elapsed() < Duration::from_secs(10));
    // the hub comes back (new port); the lease is still good
    w.hub = Some(citro_hub::server::run::Hub::start(&w.data, w.cfg.clone()).unwrap());
    let a = w.ben();
    a.heartbeat(&pid, &lease.lease).expect("the lease outlived the outage");
}
