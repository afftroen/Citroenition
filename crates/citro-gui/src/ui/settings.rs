//! The Settings page: theme, how browsers are launched, a pinned kernel path, CSV options and where the data lives.
//! Edits go to `CitroApp::settings` (the theme applies at once); `Save settings` writes them to the settings file.

use super::app::{CitroApp, ToastKind};
use super::widgets::{self, dim, mono, small_dim};
use crate::settings::Theme;
use crate::util;
use eframe::egui::{self, RichText, ScrollArea, TextEdit, Ui};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct State {
    pub error: Option<String>,
}

/// What the pinned-kernel field says about its content.
#[derive(Debug, PartialEq, Eq)]
enum KernelCheck {
    /// Empty: the newest installed kernel is used.
    Auto,
    Found,
    Missing,
    NotAFile,
}

fn check_kernel(path: &str, home: Option<&Path>) -> KernelCheck {
    let t = path.trim();
    if t.is_empty() {
        return KernelCheck::Auto;
    }
    let p: PathBuf = util::expand_tilde(t, home);
    if p.is_file() {
        KernelCheck::Found
    } else if p.exists() {
        KernelCheck::NotAFile
    } else {
        KernelCheck::Missing
    }
}

impl CitroApp {
    pub fn settings_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let dirty = self.settings != self.saved_settings;
        let (mut save, mut revert) = (false, false);
        widgets::tab_header(ui, &p, &["Settings"], 0, |ui| {
            save = widgets::primary_button(ui, &p, "Save settings", dirty).clicked();
            revert = widgets::button(ui, "Revert", dirty).clicked();
            if dirty {
                ui.label(RichText::new("Unsaved changes").color(p.warn));
            } else if self.settings_ui.error.is_none() {
                ui.label(small_dim(&p, "All changes saved"));
            }
        });
        ui.label(dim(&p, "Preferences of this app. Profiles themselves are edited on the Profiles page."));
        if let Some(e) = &self.settings_ui.error {
            ui.label(RichText::new(format!("Could not save: {e}")).color(p.err));
        }
        ui.add_space(10.0);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.settings_updates(ui);
            ui.add_space(12.0);
            self.settings_look(ui);
            ui.add_space(12.0);
            self.settings_launch(ui);
            ui.add_space(12.0);
            self.settings_data(ui);
        });
        if revert {
            self.settings = self.saved_settings.clone();
            self.set_theme(self.settings.theme);
            self.settings_ui.error = None;
        }
        if save {
            self.save_settings();
        }
    }

    fn settings_look(&mut self, ui: &mut Ui) {
        let p = self.pal;
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Appearance");
            ui.add_space(6.0);
            let mut theme = self.settings.theme;
            ui.horizontal(|ui| {
                widgets::key_label(ui, &p, "Theme");
                for t in [Theme::Dark, Theme::Light] {
                    ui.selectable_value(&mut theme, t, t.label());
                }
            });
            if theme != self.settings.theme {
                self.set_theme(theme);
            }
        });
    }

    fn settings_launch(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let home = self.home.clone();
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Opening browsers");
            ui.add_space(6.0);
            let s = &mut self.settings;
            ui.checkbox(&mut s.headless, "Headless: open browsers without a window");
            widgets::check_hint(
                ui,
                &p,
                "For servers and automation. Turn it off when you want to see and use the browser.",
            );
            ui.add_space(8.0);
            ui.checkbox(&mut s.sandbox, "Keep Chromium's sandbox");
            widgets::check_hint(
                ui,
                &p,
                "Safer, but the kernel then needs permission to create user namespaces. Leave it off in containers or when you run as root.",
            );
            ui.add_space(12.0);
            widgets::field_label(ui, &p, "Kernel path (optional)");
            ui.add(
                TextEdit::singleline(&mut s.kernel_path)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("empty = newest installed kernel")
                    .desired_width(f32::INFINITY),
            );
            let (text, color) = match check_kernel(&s.kernel_path, home.as_deref()) {
                KernelCheck::Auto => ("Using the newest installed kernel (or $CITRO_KERNEL when it is set).", p.dim),
                KernelCheck::Found => ("This file exists and will be used for every profile.", p.ok),
                KernelCheck::NotAFile => ("That path is a folder; point at the `chrome` binary inside it.", p.err),
                KernelCheck::Missing => ("No file at that path.", p.err),
            };
            ui.label(RichText::new(text).size(12.0).color(color));
            ui.add_space(12.0);
            ui.checkbox(&mut s.csv_bom, "Start CSV exports with a UTF-8 byte-order mark (Excel needs it for accents)");
        });
    }

    fn settings_data(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let home = self.home.clone();
        let data = util::tildify(&self.info.data_dir, home.as_deref());
        let file = self
            .settings_path
            .as_ref()
            .map(|f| util::tildify(&f.display().to_string(), home.as_deref()))
            .unwrap_or_else(|| "not saved to disk (demo data)".to_owned());
        let mut copy: Option<String> = None;
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "This device and its data");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                widgets::key_label(ui, &p, "Device name");
                ui.add(TextEdit::singleline(&mut self.settings.device_name).desired_width(240.0));
                ui.label(small_dim(&p, "shown to teammates"));
            });
            ui.add_space(4.0);
            for (key, value, raw) in [
                ("Data directory", data, self.info.data_dir.clone()),
                (
                    "Settings file",
                    file,
                    self.settings_path.as_ref().map(|f| f.display().to_string()).unwrap_or_default(),
                ),
            ] {
                ui.horizontal(|ui| {
                    widgets::key_label(ui, &p, key);
                    ui.add(egui::Label::new(mono(value)).truncate());
                    if !raw.is_empty() && ui.small_button("Copy").clicked() {
                        copy = Some(raw);
                    }
                });
            }
            widgets::kv(ui, &p, "Version", RichText::new(format!("Citroenition {}", env!("CARGO_PKG_VERSION"))));
        });
        if let Some(t) = copy {
            self.ctx.copy_text(t);
            self.toast(ToastKind::Info, "Copied");
        }
    }

    pub fn save_settings(&mut self) {
        let result = match &self.settings_path {
            Some(path) => self.settings.save(path).map_err(|e| e.to_string()),
            None => Ok(()),
        };
        match result {
            Ok(()) => {
                self.saved_settings = self.settings.clone();
                self.settings_ui.error = None;
                self.backend.set_kernel_override(self.settings.kernel_override());
                self.refresh_kernel();
                let note =
                    if self.settings_path.is_some() { "Settings saved" } else { "Settings kept for this demo session" };
                self.toast(ToastKind::Ok, note);
            }
            Err(e) => {
                self.settings_ui.error = Some(e.clone());
                self.toast(ToastKind::Err, format!("Could not save the settings: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_field_is_checked_against_the_disk() {
        assert_eq!(check_kernel("  ", None), KernelCheck::Auto);
        assert_eq!(check_kernel("/definitely/not/here/chrome", None), KernelCheck::Missing);
        let dir = std::env::temp_dir();
        assert_eq!(check_kernel(&dir.display().to_string(), None), KernelCheck::NotAFile);
        let file = dir.join(format!("citro-gui-kernel-check-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(check_kernel(&file.display().to_string(), None), KernelCheck::Found);
        let _ = std::fs::remove_file(file);
    }
}
