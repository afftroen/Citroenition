//! The command line of `citro-gui`. Six flags do not need clap; the parser is a plain function so it is unit-tested.

use crate::settings::Theme;
use std::path::PathBuf;

pub const USAGE: &str = "\
citro-gui - Citroenition desktop app (pure Rust, egui)

USAGE:
    citro-gui [--home DIR] [--kernel PATH] [--demo]
    citro-gui --screenshot OUT.png [--page PAGE] [--size WxH] [--theme dark|light] [--demo | --home DIR]

OPTIONS:
    --home DIR          data directory (default: $CITRO_HOME or ~/.local/share/citroenition)
    --kernel PATH       use this browser kernel binary for this run
    --demo              fake in-memory data: nothing is read from or written to disk
    --screenshot OUT    draw the window, write a PNG and exit (needs a display: xvfb-run on a server)
    --page PAGE         page for --screenshot: profiles groups logs logs-browser kernel teams settings
                        edit new export error (default: profiles)
    --size WxH          window size (default 1280x800, minimum 960x600)
    --theme dark|light  colour theme for this run (default: the saved setting)
    -V, --version       print the version
    -h, --help          this text
";

/// What a screenshot (or the first frame) shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartPage {
    #[default]
    Profiles,
    Groups,
    Logs,
    LogsBrowser,
    Kernel,
    Teams,
    Settings,
    /// The profiles page with the edit dialog open on the first profile.
    Edit,
    /// The profiles page with an empty "New profile" dialog.
    New,
    /// The profiles page with the CSV export dialog open.
    Export,
    /// The profiles page with the "could not open" modal (demo data only).
    Error,
}

impl StartPage {
    pub fn parse(s: &str) -> Option<StartPage> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "profiles" => StartPage::Profiles,
            "groups" => StartPage::Groups,
            "logs" => StartPage::Logs,
            "logs-browser" | "browser" => StartPage::LogsBrowser,
            "kernel" => StartPage::Kernel,
            "teams" => StartPage::Teams,
            "settings" => StartPage::Settings,
            "edit" => StartPage::Edit,
            "new" => StartPage::New,
            "export" => StartPage::Export,
            "error" => StartPage::Error,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    pub home: Option<PathBuf>,
    pub kernel: Option<String>,
    pub demo: bool,
    pub screenshot: Option<PathBuf>,
    pub page: StartPage,
    pub size: Option<(f32, f32)>,
    pub theme: Option<Theme>,
    pub help: bool,
    pub version: bool,
}

fn parse_size(s: &str) -> Option<(f32, f32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?);
    ((320.0..=8000.0).contains(&w) && (240.0..=8000.0).contains(&h)).then_some((w, h))
}

impl Args {
    /// `args` excludes the program name. Both `--flag value` and `--flag=value` work.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
        let mut out = Args::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
                _ => (arg.clone(), None),
            };
            let mut value = |name: &str| -> Result<String, String> {
                inline.clone().or_else(|| it.next()).ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "-h" | "--help" => out.help = true,
                "-V" | "--version" => out.version = true,
                "--demo" => out.demo = true,
                "--home" => out.home = Some(PathBuf::from(value("--home")?)),
                "--kernel" => out.kernel = Some(value("--kernel")?),
                "--screenshot" => out.screenshot = Some(PathBuf::from(value("--screenshot")?)),
                "--page" => {
                    let v = value("--page")?;
                    out.page = StartPage::parse(&v).ok_or_else(|| format!("unknown page '{v}'"))?;
                }
                "--size" => {
                    let v = value("--size")?;
                    out.size = Some(parse_size(&v).ok_or_else(|| format!("bad size '{v}' (use WIDTHxHEIGHT)"))?);
                }
                "--theme" => {
                    let v = value("--theme")?;
                    out.theme = Some(Theme::parse(&v).ok_or_else(|| format!("unknown theme '{v}' (dark or light)"))?);
                }
                other => return Err(format!("unknown argument '{other}'")),
            }
        }
        if out.demo && out.home.is_some() {
            return Err("--demo and --home cannot be combined".into());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_is_the_real_app() {
        assert_eq!(p(&[]).unwrap(), Args::default());
    }

    #[test]
    fn screenshot_flags() {
        let a = p(&["--demo", "--screenshot", "out.png", "--page", "groups", "--size", "1000x700", "--theme=light"])
            .unwrap();
        assert!(a.demo);
        assert_eq!(a.screenshot, Some(PathBuf::from("out.png")));
        assert_eq!(a.page, StartPage::Groups);
        assert_eq!(a.size, Some((1000.0, 700.0)));
        assert_eq!(a.theme, Some(Theme::Light));
    }

    #[test]
    fn version_and_help_are_flags() {
        assert!(p(&["--version"]).unwrap().version);
        assert!(p(&["-V"]).unwrap().version);
        assert!(p(&["-h"]).unwrap().help);
        assert!(!p(&[]).unwrap().version);
    }

    #[test]
    fn equals_form_and_paths() {
        let a = p(&["--home=/tmp/x", "--kernel", "/opt/k/chrome"]).unwrap();
        assert_eq!(a.home, Some(PathBuf::from("/tmp/x")));
        assert_eq!(a.kernel.as_deref(), Some("/opt/k/chrome"));
    }

    #[test]
    fn errors_are_specific() {
        assert!(p(&["--page", "nope"]).unwrap_err().contains("unknown page"));
        assert!(p(&["--size", "wide"]).unwrap_err().contains("bad size"));
        assert!(p(&["--size", "10x10"]).is_err());
        assert!(p(&["--theme", "blue"]).unwrap_err().contains("unknown theme"));
        assert!(p(&["--home"]).unwrap_err().contains("needs a value"));
        assert!(p(&["--frobnicate"]).unwrap_err().contains("unknown argument"));
        assert!(p(&["--demo", "--home", "/tmp/x"]).is_err());
    }

    #[test]
    fn every_page_name_parses() {
        for n in [
            "profiles",
            "groups",
            "logs",
            "logs-browser",
            "kernel",
            "teams",
            "settings",
            "edit",
            "new",
            "export",
            "error",
        ] {
            assert!(StartPage::parse(n).is_some(), "{n}");
        }
        assert!(p(&["-h"]).unwrap().help);
    }
}
