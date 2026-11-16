//! The CSV export dialog. There is no native file dialog in this app: the destination is a path field with a
//! sensible default (`~/citro-profiles-YYYYMMDD-HHMM.csv`), and the write happens on a worker thread.

use super::app::CitroApp;
use super::tasks::Msg;
use super::widgets::{self, dim, mono, small_dim};
use crate::backend::{Backend, group_label};
use crate::timefmt::DateTime;
use crate::util;
use citro_core::csv;
use eframe::egui::{self, Key, RadioButton, RichText, TextEdit, Ui};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Selected,
    All,
    Group,
}

pub struct ExportDialog {
    path: String,
    scope: Scope,
    bom: bool,
    selected: Vec<String>,
    total: usize,
    /// The group the profile list is filtered to: id, name, profile count.
    group: Option<(String, String, usize)>,
    writing: bool,
    done: Option<Result<(String, usize), String>>,
    /// `(path text, exists)` so the disk is only asked when the text changes.
    exists: (String, bool),
}

impl ExportDialog {
    /// The worker finished: remember what to show.
    pub fn finished(&mut self, path: &str, rows: usize, r: Result<(), String>) {
        self.writing = false;
        self.done = Some(r.map(|()| (path.to_owned(), rows)));
    }

    fn rows_in_scope(&self) -> usize {
        match self.scope {
            Scope::Selected => self.selected.len(),
            Scope::All => self.total,
            Scope::Group => self.group.as_ref().map_or(0, |g| g.2),
        }
    }

    fn path_problem(&self, home: Option<&Path>) -> Option<String> {
        let t = self.path.trim();
        if t.is_empty() {
            return Some("Enter the file to write.".to_owned());
        }
        if util::expand_tilde(t, home).is_dir() {
            return Some("That is a folder; add a file name such as profiles.csv.".to_owned());
        }
        None
    }
}

/// Fetch the CSV from the backend and write it; every failure becomes a readable line.
pub fn write_csv(
    backend: &dyn Backend,
    ids: Option<Vec<String>>,
    group: Option<String>,
    bom: bool,
    path: &Path,
) -> Result<(), String> {
    let bytes = backend.export_csv(ids, group, bom).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn scope_radio(ui: &mut Ui, cur: &mut Scope, me: Scope, enabled: bool, text: String) {
    if ui.add_enabled(enabled, RadioButton::new(*cur == me, text)).clicked() {
        *cur = me;
    }
}

impl CitroApp {
    pub fn open_export(&mut self) {
        let selected = self.selected_ids();
        let group = self.profiles.query.group.clone().map(|id| {
            let n = self
                .rows
                .iter()
                .filter(|r| r.profile.group_id == id || (id == "0" && r.profile.group_id.is_empty()))
                .count();
            let name = group_label(&self.groups, &id);
            (id, name, n)
        });
        let scope = if !selected.is_empty() {
            Scope::Selected
        } else if group.is_some() {
            Scope::Group
        } else {
            Scope::All
        };
        let path = util::default_csv_path(self.home.as_deref(), &DateTime::from_unix(self.now, self.offset));
        self.export = Some(ExportDialog {
            path,
            scope,
            bom: self.settings.csv_bom,
            selected,
            total: self.rows.len(),
            group,
            writing: false,
            done: None,
            exists: (String::new(), false),
        });
    }

    pub fn export_modal(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.export.take() else { return };
        let p = self.pal;
        let home = self.home.clone();
        if d.exists.0 != d.path {
            d.exists = (d.path.clone(), util::expand_tilde(&d.path, home.as_deref()).exists());
        }
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let (mut go, mut close) = (false, false);
        widgets::modal(ctx, &p, "export-modal", 600.0, |ui| {
            ui.label(RichText::new("Export profiles to CSV").size(18.0).strong());
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "What to export");
            ui.horizontal_wrapped(|ui| {
                let n = d.selected.len();
                scope_radio(ui, &mut d.scope, Scope::Selected, n > 0, format!("Selected ({n})"));
                scope_radio(ui, &mut d.scope, Scope::All, true, format!("All profiles ({})", d.total));
                let (label, has) = match &d.group {
                    Some((_, name, n)) => (format!("Group \u{201c}{name}\u{201d} ({n})"), true),
                    None => ("Current group".to_owned(), false),
                };
                scope_radio(ui, &mut d.scope, Scope::Group, has, label);
            });
            ui.add_space(8.0);
            widgets::field_label(ui, &p, "Write to");
            let r = ui.add(
                TextEdit::singleline(&mut d.path)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("~/profiles.csv")
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                go = true;
            }
            let problem = d.path_problem(home.as_deref());
            match &problem {
                Some(m) => ui.label(RichText::new(m).color(p.err).size(12.0)),
                None if d.exists.1 => {
                    ui.label(RichText::new("This file exists and will be replaced.").color(p.warn).size(12.0))
                }
                None => ui.label(small_dim(
                    &p,
                    format!(
                        "{} columns \u{b7} UTF-8 \u{b7} one row per profile \u{b7} proxy passwords are never written",
                        csv::HEADER.len()
                    ),
                )),
            };
            ui.add_space(4.0);
            ui.checkbox(&mut d.bom, "Add a UTF-8 byte-order mark (Excel needs it for accented names)");

            if d.writing {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(dim(&p, "Writing\u{2026}"));
                });
            }
            match &d.done {
                Some(Ok((path, rows))) => {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        widgets::dot(ui, p.ok, true);
                        let what = if *rows == 1 { "1 profile".to_owned() } else { format!("{rows} profiles") };
                        ui.label(RichText::new(format!("Wrote {what} to")).color(p.ok));
                        ui.label(mono(path.clone()));
                        if ui.small_button("Copy path").clicked() {
                            ui.ctx().copy_text(path.clone());
                        }
                    });
                }
                Some(Err(e)) => {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        widgets::dot(ui, p.err, true);
                        ui.label(RichText::new(e.clone()).color(p.err));
                    });
                }
                None => {}
            }
            widgets::footer(ui, |ui| {
                let ok = problem.is_none() && !d.writing && d.rows_in_scope() + usize::from(d.scope == Scope::All) > 0;
                let label = if d.done.is_some() { "Export again" } else { "Export" };
                if widgets::primary_button(ui, &p, label, ok).clicked() {
                    go = true;
                }
                if widgets::button(ui, if d.done.is_some() { "Close" } else { "Cancel" }, !d.writing).clicked() {
                    close = true;
                }
            });
        });
        if esc && !d.writing {
            close = true;
        }
        if close {
            return;
        }
        if go && !d.writing && d.path_problem(home.as_deref()).is_none() {
            let path = util::expand_tilde(&d.path, home.as_deref());
            let (ids, group) = match d.scope {
                Scope::Selected => (Some(d.selected.clone()), None),
                Scope::All => (None, None),
                Scope::Group => (None, d.group.as_ref().map(|g| g.0.clone())),
            };
            let (bom, rows) = (d.bom, d.rows_in_scope());
            d.writing = true;
            d.done = None;
            self.tasks.spawn(true, move |w| {
                let result = write_csv(w.backend.as_ref(), ids, group, bom, &path);
                w.send(Msg::Exported { path: path.display().to_string(), rows, result });
            });
        }
        self.export = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Query;
    use crate::backend::mock::MockBackend;
    use std::time::Duration;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("citro-gui-export-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_file_has_a_bom_a_header_and_one_row_per_profile() {
        let backend = MockBackend::demo().with_latency(Duration::ZERO);
        let n = backend.list_profiles(&Query::default()).unwrap().len();
        let dir = scratch("all");
        let path = dir.join("nested").join("out.csv");
        write_csv(&backend, None, None, true, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(csv::UTF8_BOM));
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        assert_eq!(text.matches("\r\n").count(), n + 1);
        assert!(text.starts_with("no,id,name,group,remark,persona_os"));
        assert!(!text.contains("hunter2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_or_empty_path_is_refused_before_anything_runs() {
        let mut d = ExportDialog {
            path: String::new(),
            scope: Scope::All,
            bom: true,
            selected: vec![],
            total: 3,
            group: None,
            writing: false,
            done: None,
            exists: (String::new(), false),
        };
        assert!(d.path_problem(None).is_some());
        d.path = std::env::temp_dir().display().to_string();
        assert!(d.path_problem(None).unwrap().contains("folder"));
        d.path = format!("{}/x.csv", std::env::temp_dir().display());
        assert!(d.path_problem(None).is_none());
    }

    #[test]
    fn a_write_error_is_a_sentence_not_a_panic() {
        let backend = MockBackend::demo().with_latency(Duration::ZERO);
        let err = write_csv(&backend, None, None, false, Path::new("/proc/definitely/not/here.csv")).unwrap_err();
        assert!(err.starts_with("cannot "), "{err}");
    }
}
