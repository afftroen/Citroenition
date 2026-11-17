//! "A new version is available": the check at start-up, the link at the foot of the sidebar, the Updates card in
//! Settings and the dialog that downloads, installs and restarts. The work itself is `crate::updater`.

use super::app::CitroApp;
use super::tasks::Msg;
use super::widgets::{self, dim, small_dim};
use crate::backend::BackendKind;
use crate::updater::{AppRelease, Install};
use eframe::egui::{self, CursorIcon, RichText, ScrollArea, Ui};
use std::time::{Duration, Instant};

/// Ask GitHub again after this long (the app may stay open for days).
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// Not asked yet (or the demo, which never goes online).
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(AppRelease),
    Downloading {
        release: AppRelease,
        done: u64,
        total: u64,
    },
    /// Installed; the new version starts after a restart.
    Installed(String),
    Failed(String),
}

#[derive(Default)]
pub struct State {
    pub status: Status,
    /// The dialog is open.
    pub dialog: bool,
    checked_at: Option<Instant>,
    /// The newest release seen, kept while a later check or download fails.
    pub latest: Option<AppRelease>,
}

impl State {
    pub fn available(&self) -> Option<&AppRelease> {
        match &self.status {
            Status::Available(r) | Status::Downloading { release: r, .. } => Some(r),
            Status::Failed(_) => self.latest.as_ref(),
            _ => None,
        }
    }
}

impl CitroApp {
    fn updates_offline(&self) -> bool {
        self.info.kind == BackendKind::Demo || self.shot.is_some()
    }

    /// Ask GitHub for a newer release (in the background). `force` ignores the recheck interval.
    pub fn check_updates(&mut self, force: bool) {
        let u = &self.update;
        let busy = matches!(u.status, Status::Checking | Status::Downloading { .. } | Status::Installed(_));
        let fresh = u.checked_at.is_some_and(|t| t.elapsed() < RECHECK);
        if self.updates_offline() || busy || (fresh && !force) {
            return;
        }
        self.update.status = Status::Checking;
        self.update.checked_at = Some(Instant::now());
        self.tasks.spawn(false, |w| w.send(Msg::UpdateChecked(Install::check())));
    }

    pub fn update_checked(&mut self, r: Result<Option<AppRelease>, String>) {
        self.update.status = match r {
            Ok(Some(rel)) => {
                self.update.latest = Some(rel.clone());
                Status::Available(rel)
            }
            Ok(None) => Status::UpToDate,
            Err(e) => Status::Failed(e),
        };
    }

    fn start_update(&mut self, release: AppRelease) {
        let install = match Install::current() {
            Ok(i) => i,
            Err(e) => {
                self.update.status = Status::Failed(e);
                return;
            }
        };
        self.update.status = Status::Downloading { release: release.clone(), done: 0, total: release.archive_size };
        self.tasks.spawn(true, move |w| {
            let mut progress = |done, total| w.send(Msg::UpdateProgress { done, total });
            let result = install.update(&release, &mut progress).map(|()| release.version.clone());
            w.send(Msg::UpdateDone(result));
        });
    }

    pub fn update_progress(&mut self, d: u64, t: u64) {
        if let Status::Downloading { done, total, .. } = &mut self.update.status {
            (*done, *total) = (d, t);
        }
    }

    pub fn update_done(&mut self, r: Result<String, String>) {
        self.update.status = match r {
            Ok(v) => Status::Installed(v),
            Err(e) => Status::Failed(e),
        };
        self.update.dialog = true;
    }

    fn restart(&mut self, ctx: &egui::Context) {
        match Install::current().and_then(|i| i.relaunch()) {
            // the usual quit path: team browsers are closed and uploaded first
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.update.status = Status::Failed(e),
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // drawing

    /// The line under the sidebar's version: a link when a newer version exists.
    pub fn sidebar_update_link(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let text = match &self.update.status {
            Status::Available(r) => format!("\u{2191} Update to v{}", r.version),
            Status::Downloading { done, total, .. } if *total > 0 => {
                format!("Updating\u{2026} {}%", done * 100 / total.max(&1))
            }
            Status::Downloading { .. } => "Updating\u{2026}".to_owned(),
            Status::Installed(v) => format!("Restart to use v{v}"),
            _ => return,
        };
        let r =
            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.accent_hover)).sense(egui::Sense::click()));
        if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            self.update.dialog = true;
        }
    }

    /// The Updates card on the Settings page.
    pub fn settings_updates(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let current = env!("CARGO_PKG_VERSION");
        let mut check = false;
        widgets::card(ui, &p, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, "Updates");
            ui.add_space(6.0);
            widgets::kv(ui, &p, "This version", RichText::new(format!("v{current}")).color(p.strong));
            let (line, col) = match &self.update.status {
                _ if self.updates_offline() => ("Not checked in demo mode".to_owned(), p.dim),
                Status::Idle => ("Not checked yet".to_owned(), p.dim),
                Status::Checking => ("Checking\u{2026}".to_owned(), p.dim),
                Status::UpToDate => ("You have the newest version".to_owned(), p.ok),
                Status::Available(r) => (format!("v{} is available", r.version), p.accent_hover),
                Status::Downloading { release, .. } => (format!("Installing v{}\u{2026}", release.version), p.dim),
                Status::Installed(v) => (format!("v{v} is installed: restart to use it"), p.ok),
                Status::Failed(e) => (e.clone(), p.err),
            };
            ui.horizontal(|ui| {
                widgets::key_label(ui, &p, "Status");
                ui.add(egui::Label::new(RichText::new(line).color(col)).wrap());
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_check = !self.updates_offline()
                    && !matches!(self.update.status, Status::Checking | Status::Downloading { .. });
                if widgets::button(ui, "Check for updates", can_check).clicked() {
                    check = true;
                }
                if self.update.available().is_some() || matches!(self.update.status, Status::Installed(_)) {
                    let label = if matches!(self.update.status, Status::Installed(_)) {
                        "Restart\u{2026}"
                    } else {
                        "Update\u{2026}"
                    };
                    if widgets::primary_button(ui, &p, label, true).clicked() {
                        self.update.dialog = true;
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(small_dim(
                &p,
                "Citroenition looks for a new release on GitHub when it starts. Updating replaces the programs in \
                 the folder it runs from; your profiles and kernels are not touched.",
            ));
        });
        if check {
            self.check_updates(true);
        }
    }

    pub fn update_modal(&mut self, ctx: &egui::Context) {
        let p = self.pal;
        let current = env!("CARGO_PKG_VERSION");
        let status = self.update.status.clone();
        let release = self.update.available().cloned();
        let install = Install::current();
        enum Act {
            None,
            Close,
            Update(AppRelease),
            Restart,
        }
        let mut act = Act::None;
        let r = widgets::modal(ctx, &p, "update-modal", 520.0, |ui| {
            ui.horizontal(|ui| {
                widgets::logo(ui, 28.0);
                ui.label(RichText::new("Update Citroenition").size(17.0).strong());
            });
            ui.add_space(6.0);
            match (&status, &release) {
                (Status::Installed(v), _) => {
                    ui.label(RichText::new(format!("Version {v} is installed.")).color(p.ok));
                    ui.label(dim(&p, "Restart Citroenition to use it. Open browsers keep running."));
                }
                (Status::Downloading { release, done, total }, _) => {
                    ui.label(dim(&p, format!("Downloading and checking v{}\u{2026}", release.version)));
                    let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(frac).show_percentage().desired_width(ui.available_width()));
                }
                (_, Some(rel)) => {
                    ui.label(dim(&p, format!("Version {} is available. You have {current}.", rel.version)));
                    if !rel.notes.trim().is_empty() {
                        ui.add_space(6.0);
                        egui::Frame::NONE
                            .fill(p.raised)
                            .corner_radius(egui::CornerRadius::same(10))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                    ui.add(egui::Label::new(small_dim(&p, rel.notes.trim())).wrap());
                                });
                            });
                    }
                    if let Err(e) = &install {
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(e).color(p.warn).size(12.5)).wrap());
                    }
                }
                _ => {
                    ui.label(dim(&p, format!("You have the newest version ({current}).")));
                }
            }
            if let Status::Failed(e) = &status {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(e).color(p.err).size(12.5)).wrap());
            }
            widgets::footer(ui, |ui| {
                let downloading = matches!(status, Status::Downloading { .. });
                if matches!(status, Status::Installed(_)) {
                    if widgets::primary_button(ui, &p, "Restart now", true).clicked() {
                        act = Act::Restart;
                    }
                } else if let Some(rel) = release.as_ref().filter(|_| install.is_ok())
                    && widgets::primary_button(ui, &p, "Update now", !downloading).clicked()
                {
                    act = Act::Update(rel.clone());
                }
                if let Some(rel) = &release
                    && widgets::button(ui, "Release page", true).clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&rel.page_url));
                }
                let close = if matches!(status, Status::Installed(_)) { "Later" } else { "Close" };
                if widgets::button(ui, close, !downloading).clicked() {
                    act = Act::Close;
                }
            });
        });
        if r.should_close() && !matches!(status, Status::Downloading { .. }) {
            act = Act::Close;
        }
        match act {
            Act::Close => self.update.dialog = false,
            Act::Update(rel) => self.start_update(rel),
            Act::Restart => self.restart(ctx),
            Act::None => {}
        }
    }
}
