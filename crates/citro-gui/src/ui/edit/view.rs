//! Drawing of the New / Edit profile dialog. The state and the rules live one level up in `edit.rs`.

use super::{Do, EditDialog, OS_CHOICES, OS_HINT, URL_HINT};
use crate::backend::{ProxyCheck, group_label};
use crate::countries;
use crate::proxy::ProxyKind;
use crate::ui::theme::Palette;
use crate::ui::widgets::{self, dim, small_dim};
use citro_core::profile::Group;
use eframe::egui::{self, Align, ComboBox, Key, Layout, RichText, ScrollArea, TextEdit, Ui, vec2};

const LABEL_W: f32 = 112.0;

/// `Label   control` with the label centred on the control's first line, however tall the control gets.
fn form_row<R>(ui: &mut Ui, p: &Palette, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.allocate_ui_with_layout(vec2(LABEL_W, 28.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(dim(p, label));
        });
        ui.vertical(add).inner
    })
    .inner
}

fn section(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text.to_uppercase()).size(11.5).strong().color(p.dim));
}

fn error_line(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(p.err).size(12.0)).wrap());
}

impl EditDialog {
    pub(super) fn show(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], max_h: f32) -> Do {
        let mut act = Do::Nothing;
        let mut changed = false;
        let existing = !self.id.is_empty();
        ui.horizontal(|ui| {
            ui.label(RichText::new(if existing { "Edit profile" } else { "New profile" }).size(18.0).strong());
            if existing {
                ui.label(dim(p, format!("#{}", self.serial)));
            }
            if self.loading {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        ui.add_space(2.0);
        ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 7.0;
            changed |= self.general(ui, p, groups, &mut act);
            changed |= self.proxy_block(ui, p, &mut act);
            changed |= self.identity(ui, p);
        });
        if changed {
            self.touched = true;
        }
        ui.add_space(6.0);
        widgets::footer(ui, |ui| {
            let label = if existing { "Save changes" } else { "Create profile" };
            if widgets::primary_button(ui, p, label, !self.saving && !self.loading).clicked() {
                act = Do::Save;
            }
            if widgets::button(ui, "Cancel", !self.saving).clicked() {
                act = Do::Cancel;
            }
            if self.saving {
                ui.add(egui::Spinner::new().size(16.0));
            }
            let message = match &self.error {
                Some(e) => Some(e.clone()),
                None if self.tried && self.has_problems() => Some("Fix the highlighted fields to continue.".to_owned()),
                None => None,
            };
            if let Some(m) = message {
                ui.add(egui::Label::new(RichText::new(m).color(p.err).size(13.0)).wrap());
            }
        });
        if ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command) {
            act = Do::Save;
        }
        act
    }

    fn general(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], act: &mut Do) -> bool {
        let mut ch = false;
        section(ui, p, "General");
        form_row(ui, p, "Name", |ui| {
            let r = ui.add(
                TextEdit::singleline(&mut self.name).hint_text("e.g. Vinted FR - Camille").desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut self.focus_name) {
                r.request_focus();
            }
            ch |= r.changed();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                *act = Do::Save;
            }
            if self.tried
                && let Some(m) = self.name_problem()
            {
                error_line(ui, p, &m);
            }
        });
        form_row(ui, p, "Group", |ui| {
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("edit-group")
                    .width(240.0)
                    .selected_text(group_label(groups, &self.group))
                    .show_ui(ui, |ui| {
                        ch |= ui.selectable_value(&mut self.group, "0".to_owned(), "Ungrouped").changed();
                        for g in groups {
                            ch |= ui.selectable_value(&mut self.group, g.group_id.clone(), &g.group_name).changed();
                        }
                    });
                if ui.button(if self.creating_group { "Cancel" } else { "+ New group" }).clicked() {
                    self.creating_group = !self.creating_group;
                }
            });
            if self.creating_group {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(&mut self.new_group).hint_text("New group name").desired_width(240.0),
                    );
                    let name = self.new_group.trim().to_owned();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let clicked = ui.add_enabled(!name.is_empty(), egui::Button::new("Create")).clicked();
                    if (clicked || enter) && !name.is_empty() {
                        *act = Do::CreateGroup(name);
                    }
                });
            }
        });
        form_row(ui, p, "Remark", |ui| {
            let r = ui.add(
                TextEdit::singleline(&mut self.remark)
                    .hint_text("Optional note, shown in the list")
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
        });
        ch
    }

    fn proxy_block(&mut self, ui: &mut Ui, p: &Palette, act: &mut Do) -> bool {
        let (mut ch, mut fields) = (false, false);
        section(ui, p, "Proxy");
        form_row(ui, p, "Proxy URL", |ui| {
            let r = ui.add(
                TextEdit::singleline(&mut self.url)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("socks5://user:pass@host:port")
                    .desired_width(f32::INFINITY),
            );
            if r.changed() {
                self.sync_from_url();
                ch = true;
            }
            if r.lost_focus() && self.url_err.is_none() {
                self.sync_to_url();
            }
            match &self.url_err {
                Some(e) if !self.url.trim().is_empty() => error_line(ui, p, e),
                _ => {
                    ui.label(small_dim(p, URL_HINT));
                }
            }
        });
        let direct = self.kind == ProxyKind::Direct;
        form_row(ui, p, "Server", |ui| {
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("edit-proxy-kind").width(150.0).selected_text(self.kind.label()).show_ui(
                    ui,
                    |ui| {
                        for k in ProxyKind::ALL {
                            fields |= ui.selectable_value(&mut self.kind, k, k.label()).changed();
                        }
                    },
                );
                ui.add_enabled_ui(!direct, |ui| {
                    let w = (ui.available_width() - 66.0 - 5.0 - ui.spacing().item_spacing.x * 2.0 - 1.0).max(100.0);
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.host).hint_text("host name or IP").desired_width(w))
                        .changed();
                    ui.label(":");
                    fields |=
                        ui.add(TextEdit::singleline(&mut self.port).hint_text("port").desired_width(66.0)).changed();
                });
            });
        });
        form_row(ui, p, "Login", |ui| {
            ui.add_enabled_ui(!direct, |ui| {
                ui.horizontal(|ui| {
                    let forget = self.pass_stored && self.pass.is_empty();
                    let (gaps, reserve) = if forget { (2.0, 64.0) } else { (1.0, 0.0) };
                    let w =
                        ((ui.available_width() - ui.spacing().item_spacing.x * gaps - reserve - 1.0) / 2.0).max(90.0);
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.user).hint_text("user (optional)").desired_width(w))
                        .changed();
                    let hint = if self.pass_stored { "saved \u{2013} type to replace" } else { "password (optional)" };
                    fields |= ui
                        .add(TextEdit::singleline(&mut self.pass).password(true).hint_text(hint).desired_width(w))
                        .changed();
                    if forget && ui.button("Forget").on_hover_text("Remove the saved proxy password").clicked() {
                        self.pass_stored = false;
                        fields = true;
                    }
                });
            });
        });
        form_row(ui, p, "", |ui| {
            ui.horizontal_wrapped(|ui| {
                let ready = !direct && self.proxy_problem().is_none() && !self.checking;
                if widgets::button(ui, "Check proxy", ready).clicked() {
                    *act = Do::Check;
                }
                if self.checking {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(dim(p, "Checking\u{2026}"));
                } else if let Some(res) = self.check.clone() {
                    ch |= self.check_result(ui, p, res);
                }
            });
            if self.tried
                && let Some(m) = self.proxy_problem()
            {
                error_line(ui, p, &m);
            }
        });
        if fields {
            self.sync_to_url();
            self.check = None;
            ch = true;
        }
        ch
    }

    /// The outcome line next to `Check proxy`; returns `true` when it changed the country.
    fn check_result(&mut self, ui: &mut Ui, p: &Palette, res: Result<ProxyCheck, String>) -> bool {
        match res {
            Ok(c) if c.ok => {
                widgets::dot(ui, p.ok, true);
                let place = countries::label(&c.country);
                let place = if place.is_empty() { String::new() } else { format!(" \u{b7} {place}") };
                let text = format!("Works \u{b7} exit IP {}{place} \u{b7} {} ms", c.ip, c.latency_ms);
                ui.label(RichText::new(text).color(p.ok));
                let code = c.country.trim().to_ascii_lowercase();
                if code.len() == 2
                    && code != self.country
                    && ui.small_button(format!("Use {} as country", code.to_uppercase())).clicked()
                {
                    self.country = code;
                    return true;
                }
            }
            Ok(c) => {
                widgets::dot(ui, p.err, true);
                ui.add(egui::Label::new(RichText::new(c.message).color(p.err)).wrap());
            }
            Err(e) => {
                widgets::dot(ui, p.err, true);
                ui.add(egui::Label::new(RichText::new(e).color(p.err)).wrap());
            }
        }
        false
    }

    fn identity(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        section(ui, p, "Identity");
        form_row(ui, p, "OS persona", |ui| {
            let shown = OS_CHOICES.iter().find(|c| c.0 == self.os).map_or("Auto (this computer's OS)", |c| c.1);
            ComboBox::from_id_salt("edit-os").width(240.0).selected_text(shown).show_ui(ui, |ui| {
                for (v, label) in OS_CHOICES {
                    ch |= ui.selectable_value(&mut self.os, v.to_owned(), label).changed();
                }
            });
            let col = if self.os == "windows" { p.warn } else { p.dim };
            ui.add(egui::Label::new(RichText::new(OS_HINT).size(12.0).color(col)).wrap());
        });
        form_row(ui, p, "Country", |ui| {
            ui.horizontal(|ui| {
                let shown = match countries::label(&self.country) {
                    s if s.is_empty() => "Auto (from the proxy IP)".to_owned(),
                    s => s,
                };
                ComboBox::from_id_salt("edit-country").width(240.0).selected_text(shown).show_ui(ui, |ui| {
                    ch |= ui.selectable_value(&mut self.country, String::new(), "Auto (from the proxy IP)").changed();
                    for (code, name) in countries::COUNTRIES {
                        let text = format!("{name}  ({})", code.to_uppercase());
                        ch |= ui.selectable_value(&mut self.country, (*code).to_owned(), text).changed();
                    }
                });
                let hint = match countries::implied(&self.country) {
                    Some((tz, lang)) => format!("language {lang} \u{b7} time zone {tz}"),
                    None => "language and time zone follow the proxy".to_owned(),
                };
                ui.label(small_dim(p, hint));
            });
        });
        form_row(ui, p, "Start URLs", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.tabs)
                    .hint_text("https://example.com")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            match self.tabs_problem() {
                Some(m) if self.tried => error_line(ui, p, &m),
                _ => {
                    ui.label(small_dim(p, "One address per line, opened when the browser starts."));
                }
            }
        });
        form_row(ui, p, "Fingerprint seed", |ui| {
            ui.horizontal(|ui| {
                let mut seed = if self.seed.is_empty() { "assigned on create".to_owned() } else { self.seed.clone() };
                let field = TextEdit::singleline(&mut seed)
                    .font(egui::TextStyle::Monospace)
                    .interactive(false)
                    .desired_width(300.0);
                ui.add(field);
                if !self.seed.is_empty() && ui.button("Copy").clicked() {
                    ui.ctx().copy_text(self.seed.clone());
                }
            });
            ui.label(small_dim(p, "Read-only: the profile id is the seed, so one profile always shows one identity."));
        });
        ch
    }
}
