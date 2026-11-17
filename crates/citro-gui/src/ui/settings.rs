//! The Settings page: theme, how browsers are launched, a pinned kernel path, CSV options and where the data lives.
//! Edits go to `CitroApp::settings` (the theme applies at once); `Save settings` writes them to the settings file.

use super::app::{CitroApp, ToastKind};
use super::tasks::Msg;
use super::widgets::{self, dim, mono, small_dim};
use crate::settings::Theme;
use crate::util;
use eframe::egui::{self, RichText, ScrollArea, TextEdit, Ui};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct State {
    pub error: Option<String>,
    /// Where the data folder should move to.
    pub move_to: String,
    pub moving: bool,
    /// A dropped file was read: bring the import card into view.
    pub scroll_to_import: bool,
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
            self.settings_new_profiles(ui);
            ui.add_space(12.0);
            self.settings_look(ui);
            ui.add_space(12.0);
            self.settings_launch(ui);
            ui.add_space(12.0);
            self.settings_data(ui);
            ui.add_space(12.0);
            self.settings_import(ui);
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
        let mut move_now = false;
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
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(widgets::semibold("Move the data folder", 13.5));
            ui.add_space(4.0);
            let demo = self.settings_path.is_none();
            let pinned = citro_core::store::Home::from_env();
            let open = self.open_count();
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 210.0).max(200.0);
                ui.add_enabled(
                    !demo && !pinned && !self.settings_ui.moving,
                    TextEdit::singleline(&mut self.settings_ui.move_to)
                        .hint_text("~/Documents/Citroenition data")
                        .margin(egui::Margin::symmetric(10, 8))
                        .desired_width(w),
                );
                let ready = !demo
                    && !pinned
                    && open == 0
                    && !self.settings_ui.moving
                    && !self.settings_ui.move_to.trim().is_empty();
                if widgets::primary_button(ui, &p, "Move and restart", ready).clicked() {
                    move_now = true;
                }
                if self.settings_ui.moving {
                    ui.add(egui::Spinner::new().size(16.0));
                }
            });
            let note = if demo {
                "Not available with demo data.".to_owned()
            } else if pinned {
                "CITRO_HOME is set in the environment, so it decides where the data lives.".to_owned()
            } else if open > 0 {
                format!("Close the {open} open browser(s) first: they are using the data folder.")
            } else {
                "A new or empty folder. Everything moves: profiles with their cookies and logins, the browser kernel, \
                 extensions and these settings. The command line follows too. Citroenition restarts afterwards."
                    .to_owned()
            };
            ui.label(small_dim(&p, note));
        });
        if let Some(t) = copy {
            self.ctx.copy_text(t);
            self.toast(ToastKind::Info, "Copied");
        }
        if move_now {
            self.start_move_data();
        }
    }

    fn start_move_data(&mut self) {
        let to = self.settings_ui.move_to.trim().to_owned();
        let target = util::expand_tilde(&to, self.home.as_deref());
        // a kernel pinned inside the data folder moves with it
        let old = self.info.data_dir.clone();
        if !old.is_empty() && self.settings.kernel_path.starts_with(&old) {
            self.settings.kernel_path = format!("{}{}", target.display(), &self.settings.kernel_path[old.len()..]);
            self.save_settings();
        }
        self.settings_ui.moving = true;
        self.settings_ui.error = None;
        self.tasks.spawn(true, move |w| w.send(Msg::DataMoved(w.backend.move_data(&to))));
    }

    pub fn data_moved(&mut self, r: crate::backend::BResult<()>) {
        self.settings_ui.moving = false;
        match r {
            Ok(()) => match crate::updater::relaunch_current() {
                Ok(()) => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.toast(ToastKind::Warn, format!("Data moved; start Citroenition again yourself ({e})")),
            },
            Err(e) => {
                self.settings_ui.error = Some(e.to_string());
                self.toast(ToastKind::Err, format!("Could not move the data: {e}"));
            }
        }
    }

    /// Settings -> New profiles: the defaults a new profile starts with (like the Preferences of other antidetect browsers).
    fn settings_new_profiles(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let (mut edit, mut reset) = (false, false);
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "New profiles");
            ui.add_space(6.0);
            let line = match &self.settings.new_profile {
                Some(_) => RichText::new("New profiles start with your defaults.").color(p.ok),
                None => RichText::new("New profiles start with the built-in defaults.").color(p.text),
            };
            ui.label(line);
            ui.label(small_dim(
                &p,
                "Browser, OS, search engine, proxy type, start pages, fingerprint and launch arguments: the same \
                 options as a profile. Each new profile can still change them.",
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, &p, "Edit defaults\u{2026}", true).clicked() {
                    edit = true;
                }
                if self.settings.new_profile.is_some() && widgets::button(ui, "Use built-in defaults", true).clicked() {
                    reset = true;
                }
            });
        });
        if edit {
            self.edit = Some(super::edit::EditDialog::for_defaults(self.settings.new_profile.as_ref()));
        }
        if reset {
            self.settings.new_profile = None;
            self.save_settings();
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
