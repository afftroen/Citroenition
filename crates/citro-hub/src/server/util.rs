//! Small helpers: clocks, randomness, hex, hashing, constant-time compares, input validation.

use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn now_secs() -> i64 {
    now_ms() / 1000
}

/// Fill a buffer from the OS RNG. A failing OS RNG is not recoverable, so this panics.
pub fn random_bytes(buf: &mut [u8]) {
    getrandom::fill(buf).expect("the operating system RNG failed");
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// `len` random characters out of `[a-z0-9]` (rejection sampling, so no modulo bias).
pub fn random_alnum(len: usize) -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 32];
    while out.len() < len {
        random_bytes(&mut buf);
        for &b in &buf {
            if b < 252 && out.len() < len {
                out.push(ALPHABET[(b % 36) as usize] as char);
            }
        }
    }
    out
}

/// A fresh random id like `u7k2m9q1x`.
pub fn random_id(prefix: &str, len: usize) -> String {
    format!("{prefix}{}", random_alnum(len))
}

/// `citro_` + 32 random bytes as hex.
pub fn new_token() -> String {
    let mut b = [0u8; 32];
    random_bytes(&mut b);
    format!("citro_{}", hex(&b))
}

/// A lease token: `ls_` + 24 random bytes as hex.
pub fn new_lease() -> String {
    let mut b = [0u8; 24];
    random_bytes(&mut b);
    format!("ls_{}", hex(&b))
}

/// What the server stores of a token: the lower-case hex SHA-256 of the whole `citro_...` string.
pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Constant-time equality (the length is not secret).
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// A user name: 1-64 characters out of letters, digits, `.`, `_`, `-`, `@`.
pub fn valid_user_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

/// Free text such as a team / group / device name: trimmed, 1..=`max` characters, no control characters.
pub fn clean_text(s: &str, max: usize) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > max || t.chars().any(char::is_control) {
        return None;
    }
    Some(t.to_owned())
}

/// Replace every `citro_<hex>` / `ls_<hex>` token-looking run so that a secret pasted into a log line by accident
/// never lands on disk.
pub fn scrub_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for prefix in ["citro_", "ls_"] {
            if let Some(tail) = rest.strip_prefix(prefix) {
                let run = tail.bytes().take_while(u8::is_ascii_hexdigit).count();
                if run >= 32 {
                    out.push_str(prefix);
                    out.push_str("****");
                    rest = &tail[run..];
                    continue 'outer;
                }
            }
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            out.push(c);
        }
        rest = chars.as_str();
    }
    out
}

/// `2026-11-15T18:43:00Z`; empty for `0` (never) and negative values.
pub fn iso_utc(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_utc(0), "");
        assert_eq!(iso_utc(1), "1970-01-01T00:00:01Z");
        assert_eq!(iso_utc(1_794_768_180), "2026-11-15T18:43:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn tokens_have_the_documented_shape() {
        let t = new_token();
        assert!(t.starts_with("citro_") && t.len() == 6 + 64);
        assert!(t[6..].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(t, new_token());
        assert_eq!(token_hash(&t).len(), 64);
        assert!(ct_eq(&t, &t.clone()) && !ct_eq(&t, "citro_x"));
        let id = random_id("k", 6);
        assert!(id.len() == 7 && id[1..].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    }

    #[test]
    fn names_and_text_are_validated() {
        assert!(valid_user_name("alice.b-2@corp") && !valid_user_name("") && !valid_user_name("a b"));
        assert!(!valid_user_name(&"x".repeat(65)));
        assert_eq!(clean_text("  Team A ", 20).as_deref(), Some("Team A"));
        assert!(clean_text("bad\nname", 20).is_none() && clean_text("   ", 20).is_none());
        assert!(clean_text(&"é".repeat(21), 20).is_none() && clean_text(&"é".repeat(20), 20).is_some());
    }

    #[test]
    fn token_lookalikes_are_scrubbed() {
        let t = new_token();
        let l = new_lease();
        let s = scrub_secrets(&format!("token {t} and lease {l}, but citro_short stays"));
        assert!(!s.contains(&t[6..]) && !s.contains(&l[3..]));
        assert!(s.contains("citro_****") && s.contains("ls_****") && s.contains("citro_short"));
        assert_eq!(scrub_secrets("plain ünïcode"), "plain ünïcode");
    }
}
