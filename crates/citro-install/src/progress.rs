//! A ready-made progress display for command-line front ends.

use crate::Progress;
use std::io::{IsTerminal, Write};

/// Renders [`Progress`] on stderr as a single line that keeps updating on a terminal, and as plain lines (one per
/// phase, and one per 10 % of the download) when stderr is a pipe or a log file.
///
/// ```no_run
/// # use citro_install::{InstallOptions, ProgressLine, install};
/// # fn main() -> citro_install::Result<()> {
/// let mut line = ProgressLine::new();
/// let result = install("/tmp/kernels".as_ref(), &InstallOptions::default(), &mut |p| line.update(p));
/// line.finish();
/// println!("{}", result?.binary.display());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ProgressLine {
    tty: bool,
    line_open: bool,
    last_decile: Option<u64>,
    downloaded: bool,
}

impl Default for ProgressLine {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressLine {
    /// A display on stderr.
    pub fn new() -> Self {
        Self { tty: std::io::stderr().is_terminal(), line_open: false, last_decile: None, downloaded: false }
    }

    /// Show one progress event. Pass it as `&mut |p| line.update(p)`.
    pub fn update(&mut self, p: Progress) {
        match p {
            Progress::Resolving => self.show("resolving the release and checking the signature of SHA256SUMS.txt..."),
            Progress::Downloading { done, total } => {
                self.downloaded = true;
                let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
                if total == 0 {
                    return self.show(&format!("downloading {:.1} MiB", mib(done)));
                }
                let pct = (done.min(total) * 100).checked_div(total).unwrap_or(0);
                if self.tty || self.last_decile != Some(pct / 10) {
                    self.last_decile = Some(pct / 10);
                    self.show(&format!("downloading {:.1} / {:.1} MiB ({pct}%)", mib(done), mib(total)));
                }
            }
            Progress::Verifying => self.show("verifying the archive against the signed checksum..."),
            Progress::Extracting => self.show("extracting..."),
            Progress::Done => {}
        }
    }

    /// Whether any download was reported: `false` means `install` found the kernel already there.
    pub fn downloaded(&self) -> bool {
        self.downloaded
    }

    /// Wipe the updating line (terminal only) so the next output starts on a clean line. Call it when `install`
    /// returns, success or not.
    pub fn finish(&mut self) {
        if self.line_open {
            let _ = write!(std::io::stderr(), "\r\x1b[K");
            self.line_open = false;
        }
    }

    fn show(&mut self, text: &str) {
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r\x1b[K{text}");
            self.line_open = true;
        } else {
            let _ = writeln!(err, "{text}");
        }
        let _ = err.flush();
    }
}
