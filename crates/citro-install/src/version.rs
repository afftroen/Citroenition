//! Kernel versions (`151.0.7922.108`) and their ordering.

use std::cmp::Ordering;

/// At most this many dot-separated parts (Chromium uses four).
const MAX_PARTS: usize = 6;
/// Each part is at most this many digits (keeps every part inside `u64` and every directory name short).
const MAX_DIGITS: usize = 9;

/// `1..=6` dot-separated parts of `1..=9` ASCII digits each. This is what may end up in a directory name.
pub fn is_valid_version(v: &str) -> bool {
    let mut parts = 0;
    for part in v.split('.') {
        parts += 1;
        if part.is_empty() || part.len() > MAX_DIGITS || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    (1..=MAX_PARTS).contains(&parts)
}

/// The numeric sort key of a version: `151.0.7922.108` -> `[151, 0, 7922, 108]`.
pub(crate) fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Numeric, component-wise comparison (`151.0.7922.108 > 150.9.1 > 99.0.1`, `10.0 > 9.0`).
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    version_key(a).cmp(&version_key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validity() {
        for ok in ["151.0.7922.108", "150.9.1", "99.0.1", "7", "1.2.3.4.5.6", "000.1"] {
            assert!(is_valid_version(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "1.",
            ".1",
            "1..2",
            "1.2.3.4.5.6.7",
            "v151.0.1",
            "151.0.1-linux-x64",
            "../1",
            "1/2",
            "1.2 ",
            "1.\u{663}",
            "1234567890.1",
            "-1",
        ] {
            assert!(!is_valid_version(bad), "{bad:?}");
        }
    }

    #[test]
    fn numeric_not_lexicographic() {
        assert_eq!(compare_versions("151.0.7922.108", "150.9.1"), Ordering::Greater);
        assert_eq!(compare_versions("150.9.1", "99.0.1"), Ordering::Greater);
        assert_eq!(compare_versions("10.0", "9.0"), Ordering::Greater);
        assert_eq!(compare_versions("151.0.7922.108", "151.0.7922.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Less);
    }
}
