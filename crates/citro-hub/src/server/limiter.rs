//! Per-IP brake on failed logins. In memory only: a restart forgets it, which is fine for a brute-force brake.
//!
//! `limit` failures within `window` block the address for `block`; while it is blocked every request from it is
//! refused with 429 before the token is even looked at. A limit of 0 turns the brake off.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// More addresses than this and the table is pruned (an attacker with many addresses must not grow it forever).
const MAX_TRACKED: usize = 4096;

#[derive(Debug)]
struct Entry {
    window_start: Instant,
    fails: u32,
    blocked_until: Option<Instant>,
}

#[derive(Debug)]
pub struct AuthLimiter {
    limit: u32,
    window: Duration,
    block: Duration,
    entries: Mutex<HashMap<IpAddr, Entry>>,
}

impl AuthLimiter {
    pub fn new(limit: u32, window: Duration, block: Duration) -> AuthLimiter {
        AuthLimiter { limit, window, block, entries: Mutex::new(HashMap::new()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// How many seconds (rounded up) the address stays blocked, or `None` when it may talk to us.
    pub fn blocked(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        if self.limit == 0 {
            return None;
        }
        let until = self.map().get(&ip)?.blocked_until?;
        // exactly at `until` the block is over (`left` is zero), not "blocked for 0 more seconds"
        let left = until.checked_duration_since(now).filter(|d| !d.is_zero())?;
        Some(left.as_secs() + u64::from(left.subsec_nanos() > 0))
    }

    /// Count a failed login; returns `true` when this one tipped the address over the limit.
    pub fn fail(&self, ip: IpAddr, now: Instant) -> bool {
        if self.limit == 0 {
            return false;
        }
        let mut map = self.map();
        if map.len() >= MAX_TRACKED {
            let window = self.window;
            map.retain(|_, e| e.blocked_until.is_some_and(|u| u > now) || now.duration_since(e.window_start) <= window);
            if map.len() >= MAX_TRACKED {
                // Still full of live entries: forget the oldest half rather than refuse to count.
                let mut starts: Vec<Instant> = map.values().map(|e| e.window_start).collect();
                starts.sort();
                let cut = starts[starts.len() / 2];
                map.retain(|_, e| e.window_start > cut || e.blocked_until.is_some_and(|u| u > now));
            }
        }
        let e = map.entry(ip).or_insert(Entry { window_start: now, fails: 0, blocked_until: None });
        if now.duration_since(e.window_start) > self.window {
            e.window_start = now;
            e.fails = 0;
        }
        e.fails += 1;
        if e.fails >= self.limit {
            e.blocked_until = Some(now + self.block);
            e.fails = 0;
            e.window_start = now;
            return true;
        }
        false
    }

    /// Number of addresses currently tracked (tests).
    pub fn tracked(&self) -> usize {
        self.map().len()
    }
}

/// The most `(address, user)` pairs tracked at once. Only authenticated requests are counted, so this is bounded by
/// users x addresses in practice; the cap is a safety net.
const MAX_BUCKETS: usize = 8192;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Request rate limit per `(address, user)`: a token bucket holding `per_min` tokens (so a burst of that many
/// requests is fine) that refills at `per_min / 60` tokens per second. A rate of 0 turns it off.
///
/// It runs after authentication, so the key contains a *valid* user: a stolen or runaway client cannot starve the
/// others (they have their own bucket), and an attacker guessing tokens is stopped by [`AuthLimiter`] instead.
#[derive(Debug)]
pub struct RateLimiter {
    per_min: u32,
    buckets: Mutex<HashMap<(IpAddr, String), Bucket>>,
}

impl RateLimiter {
    pub fn new(per_min: u32) -> RateLimiter {
        RateLimiter { per_min, buckets: Mutex::new(HashMap::new()) }
    }

    /// Take one token. `Err(seconds)` (rounded up, at least 1) says how long to wait for the next one.
    pub fn check(&self, ip: IpAddr, user_id: &str, now: Instant) -> Result<(), u64> {
        if self.per_min == 0 {
            return Ok(());
        }
        let cap = f64::from(self.per_min);
        let rate = cap / 60.0;
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= MAX_BUCKETS {
            // A bucket that is full again carries no information: forget it.
            map.retain(|_, b| b.tokens + now.saturating_duration_since(b.last).as_secs_f64() * rate < cap);
        }
        let b = map.entry((ip, user_id.to_owned())).or_insert(Bucket { tokens: cap, last: now });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate).min(cap);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - b.tokens) / rate).ceil().max(1.0) as u64)
        }
    }

    /// Number of pairs currently tracked (tests).
    pub fn tracked(&self) -> usize {
        self.buckets.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn a_burst_up_to_the_rate_passes_then_it_waits() {
        let l = RateLimiter::new(60);
        let t0 = Instant::now();
        for i in 0..60 {
            assert!(l.check(ip(1), "u1", t0).is_ok(), "request {i}");
        }
        assert_eq!(l.check(ip(1), "u1", t0), Err(1), "the 61st within the same instant waits one second");
        assert!(l.check(ip(1), "u1", t0 + Duration::from_millis(1100)).is_ok(), "one token came back");
        assert!(l.check(ip(1), "u1", t0 + Duration::from_millis(1100)).is_err());
        // an idle minute refills the bucket, but never beyond its capacity
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..60 {
            assert!(l.check(ip(1), "u1", later).is_ok());
        }
        assert!(l.check(ip(1), "u1", later).is_err());
    }

    #[test]
    fn every_address_and_user_pair_has_its_own_bucket() {
        let l = RateLimiter::new(2);
        let t0 = Instant::now();
        assert!(l.check(ip(1), "u1", t0).is_ok() && l.check(ip(1), "u1", t0).is_ok());
        assert!(l.check(ip(1), "u1", t0).is_err());
        assert!(l.check(ip(2), "u1", t0).is_ok(), "same user, other address");
        assert!(l.check(ip(1), "u2", t0).is_ok(), "other user, same address");
        assert_eq!(l.tracked(), 3);
    }

    #[test]
    fn zero_turns_it_off_and_the_wait_is_never_zero() {
        let off = RateLimiter::new(0);
        let t0 = Instant::now();
        for _ in 0..10_000 {
            assert!(off.check(ip(1), "u1", t0).is_ok());
        }
        assert_eq!(off.tracked(), 0);
        let slow = RateLimiter::new(1);
        assert!(slow.check(ip(1), "u1", t0).is_ok());
        assert_eq!(slow.check(ip(1), "u1", t0), Err(60), "one request per minute: wait the whole minute");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn blocks_after_the_limit_and_releases_after_the_block_time() {
        let l = AuthLimiter::new(3, Duration::from_secs(60), Duration::from_secs(30));
        let t0 = Instant::now();
        assert_eq!(l.blocked(ip(1), t0), None);
        assert!(!l.fail(ip(1), t0));
        assert!(!l.fail(ip(1), t0));
        assert_eq!(l.blocked(ip(1), t0), None, "two failures are still fine");
        assert!(l.fail(ip(1), t0), "the third blocks");
        assert_eq!(l.blocked(ip(1), t0), Some(30));
        assert_eq!(l.blocked(ip(1), t0 + Duration::from_millis(29_500)), Some(1), "rounded up");
        assert_eq!(l.blocked(ip(1), t0 + Duration::from_secs(30)), None);
        assert_eq!(l.blocked(ip(2), t0), None, "other addresses are not affected");
        // after the block a new round of failures is needed
        let t1 = t0 + Duration::from_secs(31);
        assert!(!l.fail(ip(1), t1));
    }

    #[test]
    fn failures_outside_the_window_do_not_add_up() {
        let l = AuthLimiter::new(3, Duration::from_secs(10), Duration::from_secs(30));
        let t0 = Instant::now();
        assert!(!l.fail(ip(1), t0));
        assert!(!l.fail(ip(1), t0 + Duration::from_secs(1)));
        // the window has passed: the counter starts over
        assert!(!l.fail(ip(1), t0 + Duration::from_secs(20)));
        assert!(!l.fail(ip(1), t0 + Duration::from_secs(21)));
        assert!(l.fail(ip(1), t0 + Duration::from_secs(22)));
    }

    #[test]
    fn a_limit_of_zero_turns_the_brake_off() {
        let l = AuthLimiter::new(0, Duration::from_secs(10), Duration::from_secs(30));
        let t0 = Instant::now();
        for _ in 0..100 {
            assert!(!l.fail(ip(1), t0));
        }
        assert_eq!(l.blocked(ip(1), t0), None);
        assert_eq!(l.tracked(), 0);
    }

    #[test]
    fn the_table_cannot_grow_without_bound() {
        let l = AuthLimiter::new(5, Duration::from_secs(60), Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..(MAX_TRACKED as u32 * 2) {
            let a = IpAddr::from(i.to_be_bytes());
            l.fail(a, t0 + Duration::from_millis(u64::from(i)));
        }
        assert!(l.tracked() <= MAX_TRACKED, "tracked {}", l.tracked());
    }
}
