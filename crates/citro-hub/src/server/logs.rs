//! Event logs as JSON lines, one file per team (`<data>/logs/<team_id>.jsonl`) with size-capped rotation.
//!
//! The newest file is `<team>.jsonl`; rotation shifts it to `.1`, then `.2`, ... and drops the oldest. Queries read
//! the files backwards, so "the last 200 lines" never reads more than it needs.

use super::fsio::{create_private_dir, private_options};
use crate::model::{LogKind, LogLevel, LogRecord};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The pseudo team that collects events no team owns (failed logins, startup); only server admins may read it.
pub const SERVER_LOG: &str = "_server";

#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    pub kind: Option<LogKind>,
    pub profile_id: Option<String>,
    pub levels: Vec<LogLevel>,
    pub since: Option<i64>,
    pub limit: usize,
}

impl LogQuery {
    fn matches(&self, r: &LogRecord) -> bool {
        self.kind.is_none_or(|k| k == r.kind)
            && self.profile_id.as_deref().is_none_or(|p| r.profile_id.as_deref() == Some(p))
            && (self.levels.is_empty() || self.levels.contains(&r.level))
            && self.since.is_none_or(|s| r.ts >= s)
    }
}

#[derive(Debug)]
pub struct LogSink {
    dir: PathBuf,
    max_bytes: u64,
    files: usize,
    write: Mutex<()>,
}

fn safe_name(team_id: &str) -> io::Result<&str> {
    let ok = !team_id.is_empty()
        && team_id.len() <= 64
        && team_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok { Ok(team_id) } else { Err(io::Error::new(io::ErrorKind::InvalidInput, "bad log name")) }
}

/// Call `f` for every non-empty line of the file, last line first, until it returns `false`.
fn for_each_line_rev(path: &Path, mut f: impl FnMut(&[u8]) -> bool) -> io::Result<()> {
    const CHUNK: u64 = 64 * 1024;
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut pos = file.metadata()?.len();
    // The start of a line whose beginning is still further back in the file.
    let mut carry: Vec<u8> = Vec::new();
    while pos > 0 {
        let start = pos.saturating_sub(CHUNK);
        let mut buf = vec![0u8; (pos - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        pos = start;
        buf.extend_from_slice(&carry);
        let mut end = buf.len();
        while let Some(nl) = buf[..end].iter().rposition(|&b| b == b'\n') {
            let line = &buf[nl + 1..end];
            if !line.is_empty() && !f(line) {
                return Ok(());
            }
            end = nl;
        }
        carry = buf[..end].to_vec();
    }
    if !carry.is_empty() {
        f(&carry);
    }
    Ok(())
}

impl LogSink {
    pub fn new(dir: PathBuf, max_bytes: u64, files: usize) -> LogSink {
        LogSink { dir, max_bytes: max_bytes.max(1024), files: files.max(1), write: Mutex::new(()) }
    }

    fn path(&self, team_id: &str, generation: usize) -> io::Result<PathBuf> {
        let name = safe_name(team_id)?;
        Ok(if generation == 0 {
            self.dir.join(format!("{name}.jsonl"))
        } else {
            self.dir.join(format!("{name}.jsonl.{generation}"))
        })
    }

    fn rotate(&self, team_id: &str) -> io::Result<()> {
        if self.files <= 1 {
            return match fs::remove_file(self.path(team_id, 0)?) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        for generation in (0..self.files - 1).rev() {
            match fs::rename(self.path(team_id, generation)?, self.path(team_id, generation + 1)?) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Append records to a team's log, rotating first when the file would grow past the cap.
    pub fn append(&self, team_id: &str, records: &[LogRecord]) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut chunk = String::new();
        for r in records {
            chunk.push_str(&serde_json::to_string(r).map_err(|e| io::Error::other(e.to_string()))?);
            chunk.push('\n');
        }
        let _guard = self.write.lock().unwrap_or_else(|p| p.into_inner());
        create_private_dir(&self.dir)?;
        let path = self.path(team_id, 0)?;
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len > 0 && len + chunk.len() as u64 > self.max_bytes {
            self.rotate(team_id)?;
        }
        let mut f = private_options().create(true).append(true).open(&path)?;
        f.write_all(chunk.as_bytes())
    }

    /// Newest first (by event time; arrival order breaks ties), at most `q.limit` records.
    pub fn query(&self, team_id: &str, q: &LogQuery) -> io::Result<Vec<LogRecord>> {
        let mut out: Vec<LogRecord> = Vec::new();
        let limit = q.limit.max(1);
        let _guard = self.write.lock().unwrap_or_else(|p| p.into_inner());
        for generation in 0..self.files {
            let path = self.path(team_id, generation)?;
            for_each_line_rev(&path, |line| {
                if let Ok(rec) = serde_json::from_slice::<LogRecord>(line) {
                    if q.matches(&rec) {
                        out.push(rec);
                    }
                }
                out.len() < limit
            })?;
            if out.len() >= limit {
                break;
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.ts));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("citro-hub-logs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn rec(ts: i64, kind: LogKind, level: LogLevel, pid: Option<&str>, msg: &str) -> LogRecord {
        LogRecord {
            ts,
            kind,
            level,
            team_id: "t1".into(),
            user: "ann".into(),
            device_id: "d".into(),
            profile_id: pid.map(str::to_owned),
            message: msg.into(),
        }
    }

    #[test]
    fn query_is_newest_first_and_filters() {
        let d = tmp("query");
        let sink = LogSink::new(d.clone(), 1 << 20, 3);
        let all = [
            rec(10, LogKind::Citro, LogLevel::Info, Some("k1"), "a"),
            rec(20, LogKind::Browser, LogLevel::Warn, Some("k1"), "b\nmultiline \"quoted\" ünï"),
            rec(30, LogKind::Browser, LogLevel::Error, Some("k2"), "c"),
            rec(40, LogKind::Citro, LogLevel::Debug, None, "d"),
        ];
        sink.append("t1", &all[..2]).unwrap();
        sink.append("t1", &all[2..]).unwrap();
        let msgs = |q: &LogQuery| sink.query("t1", q).unwrap().into_iter().map(|r| r.message).collect::<Vec<_>>();
        assert_eq!(msgs(&LogQuery { limit: 10, ..Default::default() }), ["d", "c", "b\nmultiline \"quoted\" ünï", "a"]);
        assert_eq!(msgs(&LogQuery { limit: 2, ..Default::default() }), ["d", "c"]);
        assert_eq!(msgs(&LogQuery { limit: 10, kind: Some(LogKind::Browser), ..Default::default() }).len(), 2);
        assert_eq!(msgs(&LogQuery { limit: 10, profile_id: Some("k1".into()), ..Default::default() }).len(), 2);
        assert_eq!(msgs(&LogQuery { limit: 10, since: Some(30), ..Default::default() }), ["d", "c"]);
        assert_eq!(
            msgs(&LogQuery { limit: 10, levels: vec![LogLevel::Warn, LogLevel::Error], ..Default::default() }),
            ["c", "b\nmultiline \"quoted\" ünï"]
        );
        assert!(sink.query("other", &LogQuery { limit: 5, ..Default::default() }).unwrap().is_empty());
        assert!(sink.query("../etc", &LogQuery { limit: 5, ..Default::default() }).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rotation_keeps_the_newest_files_and_queries_span_them() {
        let d = tmp("rotate");
        let sink = LogSink::new(d.clone(), 1024, 3);
        for i in 0..60 {
            sink.append("t1", &[rec(i, LogKind::Citro, LogLevel::Info, None, &format!("line {i:03} {}", "x".repeat(80)))])
                .unwrap();
        }
        let names: Vec<_> = {
            let mut v: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        };
        assert_eq!(names, ["t1.jsonl", "t1.jsonl.1", "t1.jsonl.2"], "never more than 3 files");
        for n in &names {
            assert!(fs::metadata(d.join(n)).unwrap().len() <= 1024, "{n} exceeds the cap");
        }
        let got = sink.query("t1", &LogQuery { limit: 1000, ..Default::default() }).unwrap();
        assert_eq!(got[0].message[..8], *"line 059", "newest first");
        // consecutive, newest first, spanning generations; the oldest lines were dropped
        assert!(got.len() > 10 && got.len() < 60);
        for w in got.windows(2) {
            assert_eq!(w[0].ts, w[1].ts + 1);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(d.join("t1.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reverse_reader_handles_lines_across_chunk_borders() {
        let d = tmp("rev");
        fs::create_dir_all(&d).unwrap();
        let p = d.join("f.txt");
        let lines: Vec<String> = (0..2000).map(|i| format!("{i}:{}", "y".repeat(i % 97))).collect();
        fs::write(&p, lines.join("\n") + "\n").unwrap();
        let mut seen = Vec::new();
        for_each_line_rev(&p, |l| {
            seen.push(String::from_utf8(l.to_vec()).unwrap());
            true
        })
        .unwrap();
        seen.reverse();
        assert_eq!(seen, lines);
        // a missing trailing newline and early exit
        fs::write(&p, "one\ntwo\nthree").unwrap();
        let mut first = None;
        for_each_line_rev(&p, |l| {
            first = Some(l.to_vec());
            false
        })
        .unwrap();
        assert_eq!(first.unwrap(), b"three");
        let _ = fs::remove_dir_all(&d);
    }
}
