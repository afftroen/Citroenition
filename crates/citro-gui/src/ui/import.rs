//! Settings -> Import profiles: from a running antidetect browser (its Local API) or from a file (CSV, Excel, JSON, a list of
//! proxies), with a preview that shows every warning before anything is added. A file can also be dropped on the
//! window. The reading is `citro_core::import` / `citro_core::antidetect`.

use super::app::{CitroApp, Page, ToastKind};
use super::tasks::{Msg, Refresh};
use super::widgets::{self, dim, semibold, small_dim};
use crate::backend::BResult;
use citro_core::import::{Parsed, Report, SAMPLE_CSV};
use eframe::egui::{self, Margin, RichText, ScrollArea, TextEdit, Ui};
use std::path::PathBuf;

pub struct State {
    pub ads_url: String,
    /// Kept for this session only, never written to disk.
    pub ads_key: String,
    pub path: String,
    pub reading: bool,
    pub progress: Option<(usize, usize)>,
    pub preview: Option<Parsed>,
    pub error: Option<String>,
    pub applying: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            ads_url: citro_core::antidetect::DEFAULT_URL.to_owned(),
            ads_key: String::new(),
            path: String::new(),
            reading: false,
            progress: None,
            preview: None,
            error: None,
            applying: false,
        }
    }
}

impl CitroApp {
    fn read_local_api(&mut self) {
        let (url, key) = (self.import_ui.ads_url.trim().to_owned(), self.import_ui.ads_key.trim().to_owned());
        self.start_reading();
        self.tasks.spawn(true, move |w| {
            let r = citro_core::antidetect::fetch(&url, &key, &mut |d, t| w.send(Msg::ImportProgress(d, t)));
            w.send(Msg::ImportRead(r));
        });
    }

    fn read_import_file(&mut self) {
        let path = crate::util::expand_tilde(self.import_ui.path.trim().trim_matches('"'), self.home.as_deref());
        let (url, key) = (self.import_ui.ads_url.trim().to_owned(), self.import_ui.ads_key.trim().to_owned());
        self.start_reading();
        self.tasks.spawn(true, move |w| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
            let mut r = std::fs::read(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))
                .and_then(|b| citro_core::import::parse_file(&b, &name));
            // an antidetect browser's export leaves big cookie lists out: fetch them through the API with the key above
            if let Ok(p) = r.as_mut()
                && p.profiles.iter().any(|i| i.cookies_elsewhere)
            {
                if key.is_empty() {
                    p.notes.push("The export left the cookies out (too large for Excel). Enter the other browser's Local API address and key above and press Read file again to fetch them.".into());
                } else {
                    match citro_core::antidetect::fill_cookies(&url, &key, p, &mut |d, t| w.send(Msg::ImportProgress(d, t))) {
                        Ok(n) => p.notes.push(format!("Fetched the cookies of {n} profile(s) through the Local API.")),
                        Err(e) => p.notes.push(format!("Could not fetch the cookies through the Local API: {e}")),
                    }
                }
            }
            w.send(Msg::ImportRead(r));
        });
    }

    fn start_reading(&mut self) {
        let s = &mut self.import_ui;
        s.reading = true;
        s.error = None;
        s.preview = None;
        s.progress = None;
    }

    pub fn import_read(&mut self, r: Result<Parsed, String>) {
        let s = &mut self.import_ui;
        s.reading = false;
        s.progress = None;
        match r {
            Ok(p) => s.preview = Some(p),
            Err(e) => s.error = Some(e),
        }
    }

    pub fn imported(&mut self, r: BResult<Report>) {
        self.import_ui.applying = false;
        match r {
            Ok(r) => {
                self.import_ui.preview = None;
                let updated = if r.updated > 0 { format!(", filled in {}", r.updated) } else { String::new() };
                self.toast(ToastKind::Ok, format!("Imported {} profiles{updated}", r.created));
                self.apply_refresh(Refresh::ROWS_GROUPS);
            }
            Err(e) => self.import_ui.error = Some(e.to_string()),
        }
    }

    /// A file dropped on the window goes to the importer.
    pub fn take_dropped_file(&mut self, ctx: &egui::Context) {
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().find_map(|f| Some(f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()))
        });
        if let Some(path) = dropped
            && self.edit.is_none()
        {
            self.import_ui.path = path.display().to_string();
            self.go(Page::Settings);
            self.settings_ui.scroll_to_import = true;
            self.read_import_file();
        }
    }

    fn save_sample(&mut self) {
        let dir =
            self.home.clone().map(|h| h.join("Downloads")).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
        let path = dir.join("citroenition-import-sample.csv");
        match std::fs::write(&path, SAMPLE_CSV) {
            Ok(()) => {
                self.import_ui.path = path.display().to_string();
                // show it in Finder (macOS) or the file manager
                let _ = if cfg!(target_os = "macos") {
                    std::process::Command::new("open").arg("-R").arg(&path).spawn()
                } else {
                    std::process::Command::new("xdg-open").arg(&dir).spawn()
                };
                self.toast(
                    ToastKind::Ok,
                    format!("Saved {}", crate::util::tildify(&path.display().to_string(), self.home.as_deref())),
                );
            }
            Err(e) => self.toast(ToastKind::Err, format!("Could not save the sample: {e}")),
        }
    }

    pub fn settings_import(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let demo = self.settings_path.is_none();
        let busy = self.import_ui.reading || self.import_ui.applying;
        let (mut ads, mut file, mut sample, mut apply, mut discard) = (false, false, false, false, false);
        let r = widgets::card_resp(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Import profiles");
            ui.label(small_dim(
                &p,
                "Bring profiles over with their groups, proxies (with logins), accounts and cookies. You see everything \
                 before it is added; a value that is almost right becomes a warning instead of stopping the import.",
            ));
            ui.add_space(12.0);

            ui.label(semibold("From another antidetect browser", 13.5));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.import_ui.ads_url)
                        .hint_text(citro_core::antidetect::DEFAULT_URL)
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(280.0),
                );
                ui.add(
                    TextEdit::singleline(&mut self.import_ui.ads_key)
                        .password(true)
                        .hint_text("API key")
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(260.0),
                );
                if widgets::primary_button(ui, &p, "Read profiles", !busy && !demo).clicked() {
                    ads = true;
                }
            });
            ui.label(small_dim(
                &p,
                "The other antidetect browser must be open with its Local API turned on; its address and API key are in its API \
                 settings. It answers about one request a second, so reading the cookies takes a second per profile. \
                 Fingerprint settings are not handed out by its API, so imported profiles get fresh ones. The key is not saved.",
            ));
            ui.add_space(12.0);

            ui.label(semibold("From a file", 13.5));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 290.0).max(200.0);
                ui.add(
                    TextEdit::singleline(&mut self.import_ui.path)
                        .hint_text("~/Downloads/profiles.csv  (or drop the file on this window)")
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(w),
                );
                if widgets::primary_button(
                    ui,
                    &p,
                    "Read file",
                    !busy && !demo && !self.import_ui.path.trim().is_empty(),
                )
                .clicked()
                {
                    file = true;
                }
                if widgets::button(ui, "Save sample CSV", true).clicked() {
                    sample = true;
                }
            });
            ui.label(small_dim(
                &p,
                "CSV (comma, semicolon or tab), Excel .xlsx (an antidetect browser's profile export), JSON, or a list of proxies, one per line. \
                 Columns are found by name: name, group, remark, proxy (or proxy type / host / port / user / password), \
                 platform, username, password, start_urls, os, country, user_agent, cookie.",
            ));
            if demo {
                ui.label(small_dim(&p, "Not available with demo data."));
            }

            if self.import_ui.reading {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    let text = match self.import_ui.progress {
                        Some((d, t)) if t > 0 => format!("Reading cookies {d} / {t}\u{2026}"),
                        _ => "Reading\u{2026}".to_owned(),
                    };
                    ui.label(dim(&p, text));
                });
            }
            if let Some(e) = &self.import_ui.error {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(e).color(p.err)).wrap());
            }
            if let Some(pv) = &self.import_ui.preview {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                let n = pv.profiles.len();
                let proxies = pv.profiles.iter().filter(|i| !i.profile.user_proxy_config.is_direct()).count();
                let cookies = pv.profiles.iter().filter(|i| !i.profile.cookie.trim().is_empty()).count();
                let warned = pv.profiles.iter().filter(|i| !i.warnings.is_empty()).count();
                ui.label(semibold(format!("{n} profiles ready to import"), 15.0));
                ui.label(dim(
                    &p,
                    format!("{proxies} with a proxy \u{b7} {cookies} with cookies \u{b7} {warned} with warnings"),
                ));
                for note in &pv.notes {
                    ui.label(small_dim(&p, note));
                }
                ui.add_space(6.0);
                ScrollArea::vertical().id_salt("import-preview").max_height(260.0).show(ui, |ui| {
                    egui::Grid::new("import-grid").striped(true).spacing([18.0, 6.0]).show(ui, |ui| {
                        for h in ["Name", "Group", "Proxy", "Cookies", "Warnings"] {
                            ui.label(RichText::new(h).size(12.0).color(p.dim).strong());
                        }
                        ui.end_row();
                        for i in &pv.profiles {
                            ui.add(egui::Label::new(&i.profile.name).truncate());
                            ui.label(if i.group.is_empty() { "\u{2013}".to_owned() } else { i.group.clone() });
                            ui.label(
                                i.profile
                                    .user_proxy_config
                                    .hop()
                                    .map_or("direct".to_owned(), |h| format!("{} {}:{}", h.scheme, h.host, h.port)),
                            );
                            let c = citro_core::cookies::parse(&i.profile.cookie, &i.profile.domain_name)
                                .map(|c| c.len())
                                .unwrap_or(0);
                            ui.label(if c == 0 { "\u{2013}".to_owned() } else { c.to_string() });
                            if i.warnings.is_empty() {
                                ui.label("");
                            } else {
                                ui.label(RichText::new(i.warnings.join("; ")).color(p.warn).size(12.0));
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let label = format!("Import {n} profiles");
                    if widgets::primary_button(ui, &p, &label, !busy).clicked() {
                        apply = true;
                    }
                    if widgets::button(ui, "Discard", !busy).clicked() {
                        discard = true;
                    }
                    if self.import_ui.applying {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                    ui.label(small_dim(&p, "Profiles imported before are filled in, not duplicated."));
                });
            }
        });
        if std::mem::take(&mut self.settings_ui.scroll_to_import) {
            r.response.scroll_to_me(Some(egui::Align::TOP));
        }
        if ads {
            self.read_local_api();
        }
        if file {
            self.read_import_file();
        }
        if sample {
            self.save_sample();
        }
        if discard {
            self.import_ui.preview = None;
        }
        if apply && let Some(pv) = self.import_ui.preview.clone() {
            self.import_ui.applying = true;
            self.tasks.spawn(true, move |w| w.send(Msg::Imported(w.backend.apply_import(&pv))));
        }
    }
}
