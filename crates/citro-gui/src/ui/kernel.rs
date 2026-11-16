//! The Kernel page: which Clearcote build the profiles run on, where it lives, which others are installed, and the
//! button that installs or updates it. The installer runs on a worker thread and reports progress through `Msg`.

use super::app::{CitroApp, Page, ToastKind};
use super::tasks::Msg;
use super::theme::Palette;
use super::widgets::{self, dim, mono, small_dim};
use crate::backend::KernelStatus;
use crate::installer::STUB_HINT;
use crate::util;
use eframe::egui::{self, Align, Layout, ProgressBar, RichText, ScrollArea, Ui, vec2};

#[derive(Default)]
pub struct State {
    pub progress: Option<(u64, u64)>,
    pub installing: bool,
    pub error: Option<String>,
}

fn fraction(done: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| (done as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

fn progress_text(done: u64, total: u64) -> String {
    match fraction(done, total) {
        Some(f) => {
            format!("{} of {}  \u{b7}  {} %", util::human_bytes(done), util::human_bytes(total), (f * 100.0) as u32)
        }
        None => format!("{} downloaded", util::human_bytes(done)),
    }
}

enum Act {
    Install,
    Refresh,
    Settings,
    Copy(String),
}

impl CitroApp {
    pub fn kernel_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let mut acts: Vec<Act> = Vec::new();
        let installing = self.kernel_ui.installing;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Kernel").size(22.0).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, "Check again", !installing).clicked() {
                    acts.push(Act::Refresh);
                }
            });
        });
        ui.label(dim(&p, "The Clearcote browser build every profile runs on."));
        ui.add_space(10.0);
        match self.kernel.clone() {
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    match &self.kernel_error {
                        Some(e) => {
                            ui.label(RichText::new("Could not look for the kernel").size(18.0).strong());
                            ui.label(RichText::new(e).color(p.err));
                        }
                        None => {
                            ui.add(egui::Spinner::new().size(26.0));
                            ui.label(dim(&p, "Looking for the kernel\u{2026}"));
                        }
                    }
                });
            }
            Some(st) => {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.kernel_active(ui, &p, &st, &mut acts);
                    ui.add_space(12.0);
                    self.kernel_install_card(ui, &p, &st, &mut acts);
                    ui.add_space(12.0);
                    self.kernel_list(ui, &p, &st, &mut acts);
                });
            }
        }
        for a in acts {
            match a {
                Act::Refresh => {
                    self.refresh_kernel();
                    self.toast(ToastKind::Info, "Looking for the kernel again");
                }
                Act::Settings => self.go(Page::Settings),
                Act::Copy(text) => {
                    self.ctx.copy_text(text);
                    self.toast(ToastKind::Info, "Copied");
                }
                Act::Install => {
                    self.kernel_ui = State { installing: true, ..State::default() };
                    self.tasks.spawn(true, |w| {
                        let progress = |done, total| w.send(Msg::KernelProgress { done, total });
                        w.send(Msg::KernelInstalled(w.backend.install_kernel(&progress)));
                    });
                }
            }
        }
    }

    fn kernel_active(&self, ui: &mut Ui, p: &Palette, st: &KernelStatus, acts: &mut Vec<Act>) {
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Active kernel").size(15.0).strong());
                match (&st.installed, &st.latest) {
                    (None, _) => widgets::badge(ui, "Not found", p.err),
                    (Some(_), Some(l)) if st.update_available() => {
                        widgets::badge(ui, &format!("Update available: {l}"), p.accent)
                    }
                    (Some(_), Some(_)) => widgets::badge(ui, "Up to date", p.ok),
                    (Some(_), None) => widgets::badge(ui, "Ready", p.ok),
                };
            });
            ui.add_space(8.0);
            match &st.installed {
                Some(k) => {
                    ui.label(RichText::new(format!("Clearcote {}", k.version)).size(21.0).strong());
                    ui.add_space(8.0);
                    let shown = util::tildify(&k.path, self.home.as_deref());
                    ui.horizontal(|ui| {
                        widgets::key_label(ui, p, "Location");
                        let w = (ui.available_width() - 70.0).max(80.0);
                        ui.allocate_ui_with_layout(vec2(w, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(mono(shown)).truncate()).on_hover_text(&k.path);
                        });
                        if ui.small_button("Copy").clicked() {
                            acts.push(Act::Copy(k.path.clone()));
                        }
                    });
                    let source = if st.source.is_empty() { "-".to_owned() } else { st.source.clone() };
                    widgets::kv(ui, p, "Chosen because", RichText::new(source));
                    let latest = st.latest.clone().unwrap_or_else(|| "unknown (offline or not checked yet)".to_owned());
                    widgets::kv(ui, p, "Newest available", RichText::new(latest));
                }
                None => {
                    ui.label(
                        RichText::new("No Clearcote kernel was found, so profiles cannot be opened yet.").size(15.0),
                    );
                    if let Some(e) = &st.error {
                        ui.add_space(2.0);
                        ui.label(RichText::new(e).color(p.err));
                    }
                    ui.add_space(2.0);
                    ui.label(dim(p, "Install one below, or point Settings at an existing `chrome` binary."));
                }
            }
            if self.settings.kernel_override().is_some() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    widgets::dot(ui, p.warn, false);
                    ui.label(small_dim(p, "A kernel path is pinned in Settings, so newer installs are not picked up."));
                    if ui.small_button("Open Settings").clicked() {
                        acts.push(Act::Settings);
                    }
                });
            }
        });
    }

    fn kernel_install_card(&self, ui: &mut Ui, p: &Palette, st: &KernelStatus, acts: &mut Vec<Act>) {
        let ui_state = &self.kernel_ui;
        let stub = !self.info.caps.kernel_install;
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Install or update").size(15.0).strong());
            ui.add_space(4.0);
            let data = util::tildify(&self.info.data_dir, self.home.as_deref());
            ui.label(dim(
                p,
                format!(
                    "Downloads the newest Clearcote build, verifies it and unpacks it into {data}/kernels. \
                     Browsers that are already open keep their current build until you close and reopen them."
                ),
            ));
            ui.add_space(10.0);
            if ui_state.installing {
                let (done, total) = ui_state.progress.unwrap_or((0, 0));
                match fraction(done, total) {
                    Some(f) => {
                        ui.add(ProgressBar::new(f).desired_width(460.0).show_percentage());
                    }
                    None => {
                        ui.add(ProgressBar::new(0.0).desired_width(460.0).animate(true));
                    }
                }
                ui.add_space(2.0);
                let text =
                    if done == 0 { "Starting the download\u{2026}".to_owned() } else { progress_text(done, total) };
                ui.label(dim(p, text));
            } else {
                ui.horizontal(|ui| {
                    let label = "Install / update kernel";
                    let r = widgets::primary_button(ui, p, label, !stub);
                    if stub {
                        r.on_disabled_hover_text("The built-in installer is not available in this build yet");
                    } else if r.clicked() {
                        acts.push(Act::Install);
                    }
                    if !stub && !st.update_available() && st.installed.is_some() {
                        ui.label(small_dim(p, "You already have the newest build."));
                    }
                });
            }
            if stub && !ui_state.installing {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(dim(p, "The in-app installer is not wired in yet."));
                    let cmd = format!("run: {STUB_HINT}");
                    egui::Frame::NONE
                        .fill(p.raised)
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| ui.label(mono(cmd).color(p.text)));
                    if ui.small_button("Copy command").clicked() {
                        acts.push(Act::Copy(STUB_HINT.to_owned()));
                    }
                });
                ui.label(small_dim(p, "Run it in a terminal, then press Check again."));
            }
            if let Some(e) = &ui_state.error {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("The install failed: {e}")).color(p.err));
            }
        });
    }

    fn kernel_list(&self, ui: &mut Ui, p: &Palette, st: &KernelStatus, acts: &mut Vec<Act>) {
        let active = st.installed.as_ref().map(|k| k.path.as_str());
        widgets::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Installed kernels").size(15.0).strong());
                ui.label(dim(p, st.all.len().to_string()));
            });
            ui.add_space(6.0);
            if st.all.is_empty() {
                ui.label(dim(p, "Nothing is installed under the data directory yet."));
                return;
            }
            for (i, k) in st.all.iter().enumerate() {
                let is_active = active == Some(k.path.as_str());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.allocate_ui_with_layout(vec2(118.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(&k.version).monospace().strong());
                    });
                    ui.allocate_ui_with_layout(vec2(54.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
                        if is_active {
                            widgets::badge(ui, "Active", p.ok);
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Copy path").clicked() {
                            acts.push(Act::Copy(k.path.clone()));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let shown = util::tildify(&k.path, self.home.as_deref());
                            ui.add(egui::Label::new(mono(shown).color(p.dim)).truncate()).on_hover_text(&k.path);
                        });
                    });
                });
                if i + 1 < st.all.len() {
                    ui.separator();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_readable_with_and_without_a_total() {
        assert_eq!(fraction(50, 200), Some(0.25));
        assert_eq!(fraction(5, 0), None);
        assert_eq!(fraction(300, 200), Some(1.0));
        assert!(progress_text(59_200_000, 118_400_000).ends_with("50 %"));
        assert!(progress_text(2_000_000, 0).ends_with("downloaded"));
    }
}
