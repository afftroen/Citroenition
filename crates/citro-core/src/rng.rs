//! A tiny deterministic PRNG (splitmix64).
//!
//! Fingerprint values are derived from the profile id, so a profile keeps the same
//! canvas / audio / client-rect identity across launches without storing them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn from_seed(seed: u64) -> Self {
        Self(seed)
    }

    /// Seed from a string (FNV-1a 64).
    pub fn from_str_seed(s: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(h)
    }

    /// Seed from the clock, the pid and a process-wide counter.
    pub fn from_entropy() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        let c = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut r = Self(t ^ (u64::from(std::process::id()) << 32) ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `lo..hi` (`hi` exclusive; modulo bias is irrelevant here).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(lo < hi);
        lo + self.next_u64() % (hi - lo)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = self.range(0, items.len() as u64) as usize;
        &items[i]
    }

    pub fn token(&mut self, len: usize, alphabet: &[u8]) -> String {
        (0..len).map(|_| *self.pick(alphabet) as char).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::from_str_seed("k1j2a52d");
        let mut b = Rng::from_str_seed("k1j2a52d");
        assert_eq!((a.next_u64(), a.next_u64()), (b.next_u64(), b.next_u64()));
        assert_ne!(Rng::from_str_seed("x").next_u64(), Rng::from_str_seed("y").next_u64());
    }

    #[test]
    fn range_and_token() {
        let mut r = Rng::from_entropy();
        for _ in 0..1000 {
            let v = r.range(5, 9);
            assert!((5..9).contains(&v));
        }
        assert_eq!(r.token(7, b"abc").len(), 7);
    }
}
