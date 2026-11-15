//! Small helpers that need no UI: default export paths, byte formatting, path expansion.

use crate::timefmt::DateTime;
use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// `~/citro-profiles-20261115-1403.csv`. `home = None` keeps the literal `~`, which [`expand_tilde`] resolves at
/// write time.
pub fn default_csv_path(home: Option<&Path>, date: &DateTime) -> String {
    default_export_path(home, &format!("citro-profiles-{}.csv", stamp(date)))
}

/// `~/citro-browser-log-20261115-1403.txt`
pub fn default_log_path(home: Option<&Path>, kind: &str, date: &DateTime) -> String {
    default_export_path(home, &format!("citro-{}-log-{}.txt", kind.to_ascii_lowercase(), stamp(date)))
}

/// `20261115-1403`
pub fn stamp(d: &DateTime) -> String {
    format!("{:04}{:02}{:02}-{:02}{:02}", d.year, d.month, d.day, d.hour, d.minute)
}

fn default_export_path(home: Option<&Path>, file: &str) -> String {
    match home {
        Some(h) => h.join(file).display().to_string(),
        None => format!("~/{file}"),
    }
}

/// Resolve a leading `~` / `~/` against `home`.
pub fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    let p = path.trim();
    match (p, home) {
        ("~", Some(h)) => h.to_path_buf(),
        (_, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        _ => PathBuf::from(p),
    }
}

/// Show `/home/me/x` as `~/x`.
pub fn tildify(path: &str, home: Option<&Path>) -> String {
    match home.and_then(|h| h.to_str()).filter(|h| !h.is_empty()) {
        Some(h) if path == h => "~".to_owned(),
        Some(h) if path.starts_with(&format!("{h}/")) => format!("~{}", &path[h.len()..]),
        _ => path.to_owned(),
    }
}

/// `118.4 MB`, `812 KB`, `0 B`
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 { format!("{v:.0} {}", UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

/// Plain-text single-line preview of a free-text field (first line only).
pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Letters+digits initial for avatars.
pub fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".to_owned())
}

/// Hides the password of every `scheme://user:password@host` in a line of free text (kernel logs can echo a proxy
/// URL). `user:***@` is kept so the line stays readable.
pub fn mask_secrets_in_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        // the authority ends at the first `/`, `?`, `#` or whitespace
        let end = tail.find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace()).unwrap_or(tail.len());
        let (authority, after) = tail.split_at(end);
        match authority.rfind('@') {
            Some(at) => {
                let (userinfo, host) = authority.split_at(at);
                match userinfo.split_once(':') {
                    Some((user, _)) => {
                        out.push_str(user);
                        out.push_str(":***");
                    }
                    None => out.push_str("***"),
                }
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> DateTime {
        DateTime::from_unix(crate::timefmt::days_from_civil(2026, 11, 15) * 86_400 + 40_000, 0)
    }

    #[test]
    fn export_paths_default_to_the_home_directory_with_a_timestamp() {
        // 2026-11-15 11:06:40 UTC
        assert_eq!(default_csv_path(Some(Path::new("/home/ada")), &d()), "/home/ada/citro-profiles-20261115-1106.csv");
        assert_eq!(default_csv_path(None, &d()), "~/citro-profiles-20261115-1106.csv");
        assert_eq!(
            default_log_path(Some(Path::new("/home/ada")), "Browser", &d()),
            "/home/ada/citro-browser-log-20261115-1106.txt"
        );
    }

    #[test]
    fn tilde_round_trip() {
        let home = Path::new("/home/ada");
        assert_eq!(expand_tilde("~/Downloads/x.csv", Some(home)), PathBuf::from("/home/ada/Downloads/x.csv"));
        assert_eq!(expand_tilde("  ~  ", Some(home)), PathBuf::from("/home/ada"));
        assert_eq!(expand_tilde("/tmp/x.csv", Some(home)), PathBuf::from("/tmp/x.csv"));
        assert_eq!(expand_tilde("~/x", None), PathBuf::from("~/x"));
        assert_eq!(tildify("/home/ada/.local/share/citroenition", Some(home)), "~/.local/share/citroenition");
        assert_eq!(tildify("/home/adam/x", Some(home)), "/home/adam/x");
        assert_eq!(tildify("/home/ada", Some(home)), "~");
    }

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(118_400_000), "113 MB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn urls_in_log_lines_lose_their_passwords() {
        assert_eq!(
            mask_secrets_in_text(
                "via socks5://bob:p%40ss:word@203.0.113.9:1080/x failed, retry http://example.com/a@b"
            ),
            "via socks5://bob:***@203.0.113.9:1080/x failed, retry http://example.com/a@b"
        );
        assert_eq!(mask_secrets_in_text("https://tok3n@host.example"), "https://***@host.example");
        assert_eq!(mask_secrets_in_text("no url here: a:b@c"), "no url here: a:b@c");
        assert_eq!(mask_secrets_in_text(""), "");
        let twice = mask_secrets_in_text("a ftp://u:p1@h1 b ftp://v:p2@h2");
        assert_eq!(twice, "a ftp://u:***@h1 b ftp://v:***@h2");
    }

    #[test]
    fn small_text_helpers() {
        assert_eq!(first_line("  hello\nworld"), "hello");
        assert_eq!(first_line(""), "");
        assert_eq!(initial("élodie"), "É");
        assert_eq!(initial("  --"), "?");
    }
}
