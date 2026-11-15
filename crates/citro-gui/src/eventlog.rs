//! The "Citroenition" log: what the app did (opened, closed, created, deleted, exported ...), with a level and a
//! timestamp. Kept in memory for the Logs page and, for the local backend, appended to a small file so it survives a
//! restart. It never records secrets: callers pass profile names and ids, never passwords or proxy URLs.

use crate::backend::{Level, LogLine, LogQuery};
use crate::timefmt::now_ts;
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

const CAP: usize = 4000;
/// When the file grows past this, it is cut down to its newest `KEEP_BYTES` (on the next start).
const MAX_FILE_BYTES: u64 = 1 << 20;
const KEEP_BYTES: usize = 256 << 10;

#[derive(Debug, Default)]
pub struct EventLog {
    lines: Mutex<VecDeque<LogLine>>,
    file: Option<PathBuf>,
}

fn level_from(s: &str) -> Option<Level> {
    Level::ALL.into_iter().find(|l| l.label().eq_ignore_ascii_case(s))
}

/// `ts<TAB>LEVEL<TAB>profile<TAB>message`
fn parse_line(line: &str) -> Option<LogLine> {
    let mut it = line.splitn(4, '\t');
    let ts = it.next()?.parse().ok()?;
    let level = level_from(it.next()?)?;
    let profile = it.next()?;
    let message = it.next()?.to_owned();
    Some(LogLine { ts, level, profile_id: Some(profile.to_owned()).filter(|p| !p.is_empty()), message })
}

fn format_line(l: &LogLine) -> String {
    let msg: String = l.message.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    format!("{}\t{}\t{}\t{}\n", l.ts, l.level.label(), l.profile_id.as_deref().unwrap_or(""), msg)
}

impl EventLog {
    pub fn in_memory() -> EventLog {
        EventLog::default()
    }

    /// Loads the newest part of `path` (if it exists) and appends new events to it.
    pub fn with_file(path: PathBuf) -> EventLog {
        let mut lines = VecDeque::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut text: &str = &text;
            if text.len() as u64 > MAX_FILE_BYTES {
                let mut from = text.len() - KEEP_BYTES;
                while !text.is_char_boundary(from) {
                    from += 1;
                }
                // start at a line boundary
                let nl = text[from..].find('\n').map_or(text.len(), |i| from + i + 1);
                text = &text[nl..];
                let _ = std::fs::write(&path, text);
            }
            for l in text.lines().filter_map(parse_line) {
                if lines.len() == CAP {
                    lines.pop_front();
                }
                lines.push_back(l);
            }
        }
        EventLog { lines: Mutex::new(lines), file: Some(path) }
    }

    pub fn push(&self, level: Level, profile_id: Option<&str>, message: impl Into<String>) {
        self.push_at(now_ts(), level, profile_id, message);
    }

    pub fn push_at(&self, ts: i64, level: Level, profile_id: Option<&str>, message: impl Into<String>) {
        let line = LogLine { ts, level, profile_id: profile_id.map(str::to_owned), message: message.into() };
        if let Some(path) = &self.file {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).mode(0o600).open(path) {
                let _ = f.write_all(format_line(&line).as_bytes());
            }
        }
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == CAP {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn info(&self, profile_id: Option<&str>, message: impl Into<String>) {
        self.push(Level::Info, profile_id, message);
    }

    pub fn warn(&self, profile_id: Option<&str>, message: impl Into<String>) {
        self.push(Level::Warn, profile_id, message);
    }

    pub fn error(&self, profile_id: Option<&str>, message: impl Into<String>) {
        self.push(Level::Error, profile_id, message);
    }

    /// Newest `q.limit` matching lines, oldest first.
    pub fn query(&self, q: &LogQuery) -> Vec<LogLine> {
        let needle = q.text.trim().to_lowercase();
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<LogLine> = lines
            .iter()
            .rev()
            .filter(|l| l.level >= q.level_min)
            .filter(|l| q.profile_id.as_deref().is_none_or(|p| l.profile_id.as_deref() == Some(p)))
            .filter(|l| {
                needle.is_empty()
                    || l.message.to_lowercase().contains(&needle)
                    || l.profile_id.as_deref().is_some_and(|p| p.to_lowercase().contains(&needle))
            })
            .take(if q.limit == 0 { usize::MAX } else { q.limit })
            .cloned()
            .collect();
        out.reverse();
        out
    }

    pub fn len(&self) -> usize {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(level: Level, text: &str, limit: usize) -> LogQuery {
        LogQuery { level_min: level, text: text.into(), limit, ..Default::default() }
    }

    #[test]
    fn filters_by_level_text_and_profile_newest_last() {
        let log = EventLog::in_memory();
        log.push_at(1, Level::Info, Some("k1"), "opened Amélie");
        log.push_at(2, Level::Warn, None, "proxy slow");
        log.push_at(3, Level::Error, Some("k2"), "open failed: Boom");
        log.push_at(4, Level::Info, Some("k1"), "closed Amélie");
        assert_eq!(log.query(&q(Level::Info, "", 0)).len(), 4);
        assert_eq!(log.query(&q(Level::Warn, "", 0)).iter().map(|l| l.ts).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(log.query(&q(Level::Trace, "AMÉLIE", 0)).len(), 2);
        assert_eq!(log.query(&q(Level::Info, "", 2)).iter().map(|l| l.ts).collect::<Vec<_>>(), vec![3, 4]);
        let by_profile = LogQuery { profile_id: Some("k1".into()), limit: 10, ..Default::default() };
        assert_eq!(log.query(&by_profile).len(), 2);
        // searching by profile id works too
        assert_eq!(log.query(&q(Level::Info, "k2", 0)).len(), 1);
    }

    #[test]
    fn capacity_is_bounded() {
        let log = EventLog::in_memory();
        for i in 0..(CAP + 50) {
            log.push_at(i as i64, Level::Info, None, format!("line {i}"));
        }
        assert_eq!(log.len(), CAP);
        assert_eq!(log.query(&q(Level::Info, "", 1))[0].message, format!("line {}", CAP + 49));
    }

    #[test]
    fn survives_a_restart_and_keeps_one_line_per_event() {
        let dir = std::env::temp_dir().join(format!("citro-gui-eventlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("gui-events.log");
        {
            let log = EventLog::with_file(path.clone());
            log.push_at(10, Level::Info, Some("k9"), "multi\nline\tmessage");
            log.push_at(11, Level::Error, None, "second");
        }
        let log = EventLog::with_file(path.clone());
        let lines = log.query(&q(Level::Trace, "", 0));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "multi line message");
        assert_eq!(lines[0].profile_id.as_deref(), Some("k9"));
        assert_eq!((lines[1].level, lines[1].profile_id.clone()), (Level::Error, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_lines_in_the_file_are_skipped() {
        assert!(parse_line("not a log line").is_none());
        assert!(parse_line("5\tNOPE\t\tmsg").is_none());
        assert_eq!(parse_line("5\tWARN\t\tmsg").unwrap().level, Level::Warn);
    }
}
