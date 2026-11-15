//! Tiny date/time helpers (no chrono): civil-date math, the local UTC offset and the "3 h ago" strings.
//! Everything takes the offset as a parameter so it is deterministic and testable.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Days since 1970-01-01 -> (year, month 1..=12, day 1..=31). Howard Hinnant's `civil_from_days`.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTime {
    pub fn from_unix(ts: i64, offset_secs: i32) -> DateTime {
        let t = ts + i64::from(offset_secs);
        let (year, month, day) = civil_from_days(t.div_euclid(86_400));
        let sod = t.rem_euclid(86_400) as u32;
        DateTime { year, month, day, hour: sod / 3600, minute: sod % 3600 / 60, second: sod % 60 }
    }

    /// `2026-11-15`
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `14:03`
    pub fn hm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `2026-11-15 14:03:22`
    pub fn date_time(&self) -> String {
        format!("{} {:02}:{:02}:{:02}", self.date(), self.hour, self.minute, self.second)
    }
}

fn day_index(ts: i64, offset_secs: i32) -> i64 {
    (ts + i64::from(offset_secs)).div_euclid(86_400)
}

/// `UTC` / `UTC+01:00` / `UTC-05:30`
pub fn tz_label(offset_secs: i32) -> String {
    if offset_secs == 0 {
        return "UTC".to_owned();
    }
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let a = offset_secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

/// `"never"` for `0`, otherwise `2026-11-15 14:03:22 (UTC+01:00)` - the tooltip next to a relative time.
pub fn absolute(ts: i64, offset_secs: i32) -> String {
    if ts <= 0 {
        return "never".to_owned();
    }
    format!("{} ({})", DateTime::from_unix(ts, offset_secs).date_time(), tz_label(offset_secs))
}

/// `never`, `just now`, `7 min ago`, `3 h ago`, `yesterday`, `5 days ago`, then the plain date.
pub fn relative(now: i64, ts: i64, offset_secs: i32) -> String {
    if ts <= 0 {
        return "never".to_owned();
    }
    let diff = now - ts;
    if diff < 60 {
        return "just now".to_owned();
    }
    if diff < 3600 {
        return format!("{} min ago", diff / 60);
    }
    if diff < 86_400 {
        return format!("{} h ago", diff / 3600);
    }
    let days = day_index(now, offset_secs) - day_index(ts, offset_secs);
    match days {
        ..=1 => "yesterday".to_owned(),
        2..=13 => format!("{days} days ago"),
        _ => DateTime::from_unix(ts, offset_secs).date(),
    }
}

/// For "open on X by Y since ...": `today 14:32`, `yesterday 22:10` or `2026-11-10 22:10`.
pub fn since(now: i64, ts: i64, offset_secs: i32) -> String {
    if ts <= 0 {
        return "an unknown time".to_owned();
    }
    let dt = DateTime::from_unix(ts, offset_secs);
    match day_index(now, offset_secs) - day_index(ts, offset_secs) {
        ..=0 => format!("today {}", dt.hm()),
        1 => format!("yesterday {}", dt.hm()),
        _ => format!("{} {}", dt.date(), dt.hm()),
    }
}

/// `45 s`, `12 min`, `2 h 14 min`, `3 d 4 h`
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        _ => format!("{} d {} h", s / 86_400, s % 86_400 / 3600),
    }
}

/// `+0100` / `-0530` (the output of `date +%z`) -> seconds east of UTC.
pub fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if rest.len() != 4 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = rest[..2].parse().ok()?;
    let m: i32 = rest[2..].parse().ok()?;
    (h <= 23 && m <= 59).then_some(sign * (h * 3600 + m * 60))
}

/// The machine's current UTC offset. Pure Rust has no safe way to read the C library's zone, so ask `date`
/// (honours `TZ` and `/etc/localtime`); UTC when that fails.
pub fn detect_local_offset() -> i32 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_offset(&s))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-15 13:03:22 UTC (a Sunday).
    fn t() -> i64 {
        days_from_civil(2026, 11, 15) * 86_400 + 13 * 3600 + 3 * 60 + 22
    }

    #[test]
    fn civil_round_trip() {
        for d in [-1000, -1, 0, 1, 59, 60, 365, 11_016, 20_000, 30_000, 47_482] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d, "{y}-{m}-{dd}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 11, 15), 20_772);
    }

    #[test]
    fn date_time_formatting_with_offset() {
        assert_eq!(DateTime::from_unix(t(), 0).date_time(), "2026-11-15 13:03:22");
        let dt = DateTime::from_unix(t(), 3600);
        assert_eq!((dt.date(), dt.hm()), ("2026-11-15".to_owned(), "14:03".to_owned()));
        // an offset that crosses midnight
        assert_eq!(DateTime::from_unix(t() + 10 * 3600, 3600).date_time(), "2026-11-16 00:03:22");
        assert_eq!(DateTime::from_unix(-1, 0).date_time(), "1969-12-31 23:59:59");
    }

    #[test]
    fn relative_strings() {
        let (now, off) = (t(), 3600);
        assert_eq!(relative(now, 0, off), "never");
        assert_eq!(relative(now, now - 10, off), "just now");
        assert_eq!(relative(now, now + 500, off), "just now"); // clock skew never prints a negative age
        assert_eq!(relative(now, now - 60, off), "1 min ago");
        assert_eq!(relative(now, now - 59 * 60, off), "59 min ago");
        assert_eq!(relative(now, now - 3 * 3600 - 20, off), "3 h ago");
        assert_eq!(relative(now, now - 23 * 3600 - 3000, off), "23 h ago");
        assert_eq!(relative(now, now - 86_400, off), "yesterday");
        assert_eq!(relative(now, now - 30 * 3600, off), "yesterday");
        assert_eq!(relative(now, now - 3 * 86_400, off), "3 days ago");
        assert_eq!(relative(now, now - 13 * 86_400, off), "13 days ago");
        assert_eq!(relative(now, now - 20 * 86_400, off), "2026-10-26");
    }

    #[test]
    fn relative_respects_local_midnight() {
        // 00:21 local (+01:00) on the 16th.
        let now = days_from_civil(2026, 11, 15) * 86_400 + 23 * 3600 + 21 * 60;
        assert_eq!(relative(now, now - 40 * 60, 3600), "40 min ago");
        assert_eq!(relative(now, now - (24 * 3600 + 11 * 60), 3600), "yesterday");
        assert_eq!(relative(now, now - 48 * 3600, 3600), "2 days ago");
    }

    #[test]
    fn absolute_and_since() {
        let now = t();
        assert_eq!(absolute(0, 0), "never");
        assert_eq!(absolute(now, 3600), "2026-11-15 14:03:22 (UTC+01:00)");
        assert_eq!(since(now, now - 600, 0), "today 12:53");
        assert_eq!(since(now, now - 86_400, 0), "yesterday 13:03");
        assert_eq!(since(now, now - 5 * 86_400, 0), "2026-11-10 13:03");
        assert_eq!(since(now, 0, 0), "an unknown time");
    }

    #[test]
    fn tz_labels_and_offsets() {
        assert_eq!(tz_label(0), "UTC");
        assert_eq!(tz_label(-19_800), "UTC-05:30");
        assert_eq!(parse_offset("+0100\n"), Some(3600));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        assert_eq!(parse_offset("+0545"), Some(20_700));
        for bad in ["", "0100", "+01", "+2460", "UTC", "+01:00"] {
            assert_eq!(parse_offset(bad), None, "{bad}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(duration(5), "5 s");
        assert_eq!(duration(720), "12 min");
        assert_eq!(duration(2 * 3600 + 14 * 60), "2 h 14 min");
        assert_eq!(duration(3 * 86_400 + 4 * 3600), "3 d 4 h");
        assert_eq!(duration(-4), "0 s");
    }
}
