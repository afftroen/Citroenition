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
        let left = until.checked_duration_since(now)?;
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
