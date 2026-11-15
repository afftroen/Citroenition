//! Small helpers that need no UI: default export paths, byte formatting, path expansion.

use crate::timefmt::DateTime;
use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// `~/Downloads/citroenition-profiles-2026-11-15.csv`. `home = None` keeps the literal `~`, which
/// [`expand_tilde`] resolves at write time.
pub fn default_csv_path(home: Option<&Path>, date: &DateTime) -> String {
    default_export_path(home, &format!("citroenition-profiles-{}.csv", date.date()))
}

/// `~/Downloads/citroenition-browser-log-2026-11-15.txt`
pub fn default_log_path(home: Option<&Path>, kind: &str, date: &DateTime) -> String {
    default_export_path(home, &format!("citroenition-{}-log-{}.txt", kind.to_ascii_lowercase(), date.date()))
}

fn default_export_path(home: Option<&Path>, file: &str) -> String {
    match home {
        Some(h) => h.join("Downloads").join(file).display().to_string(),
        None => format!("~/Downloads/{file}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> DateTime {
        DateTime::from_unix(crate::timefmt::days_from_civil(2026, 11, 15) * 86_400 + 40_000, 0)
    }

    #[test]
    fn csv_path_defaults_to_downloads_with_date() {
        assert_eq!(
            default_csv_path(Some(Path::new("/home/ada")), &d()),
            "/home/ada/Downloads/citroenition-profiles-2026-11-15.csv"
        );
        assert_eq!(default_csv_path(None, &d()), "~/Downloads/citroenition-profiles-2026-11-15.csv");
        assert_eq!(
            default_log_path(Some(Path::new("/home/ada")), "Browser", &d()),
            "/home/ada/Downloads/citroenition-browser-log-2026-11-15.txt"
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
    fn small_text_helpers() {
        assert_eq!(first_line("  hello\nworld"), "hello");
        assert_eq!(first_line(""), "");
        assert_eq!(initial("élodie"), "É");
        assert_eq!(initial("  --"), "?");
    }
}
