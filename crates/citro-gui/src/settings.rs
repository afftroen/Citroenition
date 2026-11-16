//! The GUI's own preferences (theme, headless, sandbox, kernel override ...). Stored as JSON next to the profile
//! store (`<data dir>/gui-settings.json`). Nothing secret is kept here: a hub token is never written to disk.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }

    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Open browsers without a window.
    pub headless: bool,
    /// Keep Chromium's sandbox.
    pub sandbox: bool,
    /// Pin the kernel binary; empty = newest installed (or `$CITRO_KERNEL`).
    pub kernel_path: String,
    /// Write a UTF-8 byte order mark in CSV exports (Excel needs it to read non-ASCII names).
    pub csv_bom: bool,
    /// Team hub address (the access token is never stored).
    pub hub_url: String,
    /// How this machine is called in a team ("MIA-LAPTOP").
    pub device_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Dark,
            headless: false,
            sandbox: false,
            kernel_path: String::new(),
            csv_bom: true,
            hub_url: String::new(),
            device_name: default_device_name(),
        }
    }
}

fn default_device_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "this-device".to_owned())
}

impl Settings {
    pub fn kernel_override(&self) -> Option<String> {
        Some(self.kernel_path.trim().to_owned()).filter(|p| !p.is_empty())
    }

    /// A missing or unreadable file gives the defaults (a broken settings file must never stop the app).
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp: PathBuf = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("citro-gui-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("gui-settings.json");
        assert_eq!(Settings::load(&path), Settings::default());
        let s =
            Settings { theme: Theme::Light, headless: true, kernel_path: "/opt/k/chrome".into(), ..Default::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert_eq!(s.kernel_override().as_deref(), Some("/opt/k/chrome"));
        assert_eq!(Settings::default().kernel_override(), None);
        // a broken file falls back to the defaults
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        // unknown / missing keys are fine
        std::fs::write(&path, r#"{"theme":"light","future":1}"#).unwrap();
        assert_eq!(Settings::load(&path).theme, Theme::Light);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_never_holds_a_token() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!json.to_lowercase().contains("token"));
        assert!(!json.to_lowercase().contains("password"));
    }

    #[test]
    fn theme_names() {
        assert_eq!(Theme::parse(" LIGHT "), Some(Theme::Light));
        assert_eq!(Theme::parse("blue"), None);
    }
}
