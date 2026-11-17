//! Drawing of the New / Edit profile page. The state and the rules live one level up in `edit.rs`.
//!
//! Layout: a header (Back, title), the section tabs, then the scrolling form on the left and the Overview card on
//! the right, and a footer with the buttons that never scrolls away.

use super::{Do, EditDialog, Follow, Hw, SECTIONS, URL_HINT};
use crate::backend::{ProxyCheck, group_label};
use crate::countries;
use crate::proxy::ProxyKind;
use crate::ui::theme::{self, Palette};
use crate::ui::widgets::{self, dim, semibold, small_dim};
use citro_core::fingerprint::{Gpu, Noise, Os, WebRtc};
use citro_core::profile::Group;
use eframe::egui::{
    self, Align, ComboBox, CornerRadius, CursorIcon, Frame, Key, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, vec2,
};

const LABEL_W: f32 = 170.0;
/// Between a label and its control.
const LABEL_GAP: f32 = 32.0;
/// Between two rows of the form.
const ROW_GAP: f32 = 26.0;
/// The widest a form control gets: wide enough to breathe, narrow enough to read.
const FIELD_W: f32 = 580.0;
const OVERVIEW_W: f32 = 340.0;
const FOOTER_H: f32 = 72.0;

const OS_CHOICES: [(&str, &str); 4] = [("", "Auto"), ("windows", "Windows"), ("macos", "macOS"), ("linux", "Linux")];
const OS_HINT: &str = "Auto uses this computer's OS. Linux and macOS personas pass iphey.com at 100 %; the Windows \
                       persona is currently flagged on Linux hosts.";
const BRANDS: [(&str, &str); 4] =
    [("", "Chrome"), ("edge", "Microsoft Edge"), ("opera", "Opera"), ("vivaldi", "Vivaldi")];
/// `Profile::search_engine` values (see `citro_core::browser_prefs`).
const SEARCH_CHOICES: [(&str, &str); 8] = [
    ("", "Google"),
    ("bing", "Microsoft Bing"),
    ("duckduckgo", "DuckDuckGo"),
    ("brave", "Brave Search"),
    ("yahoo", "Yahoo"),
    ("ecosia", "Ecosia"),
    ("startpage", "Startpage"),
    ("browser", "Leave it to the browser"),
];
const WEBRTC: [(&str, &str); 3] = [("disabled", "Disabled"), ("proxy", "Replace (proxy IP)"), ("local", "Real")];
const SCREENS: [&str; 12] = [
    "1280 x 720",
    "1280 x 800",
    "1366 x 768",
    "1440 x 900",
    "1536 x 864",
    "1600 x 900",
    "1680 x 1050",
    "1920 x 1080",
    "1920 x 1200",
    "2560 x 1440",
    "2560 x 1600",
    "3840 x 2160",
];
const CPUS: [&str; 7] = ["2", "4", "6", "8", "12", "16", "20"];
const RAMS: [&str; 3] = ["2", "4", "8"];
/// Common, believable GPUs: (label, unmasked vendor, unmasked renderer).
const GPUS: [(&str, &str, &str); 6] = [
    (
        "NVIDIA GeForce RTX 3060 (Windows)",
        "Google Inc. (NVIDIA)",
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "NVIDIA GeForce GTX 1650 (Windows)",
        "Google Inc. (NVIDIA)",
        "ANGLE (NVIDIA, NVIDIA GeForce GTX 1650 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "Intel UHD Graphics 630 (Windows)",
        "Google Inc. (Intel)",
        "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "AMD Radeon RX 6600 (Windows)",
        "Google Inc. (AMD)",
        "ANGLE (AMD, AMD Radeon RX 6600 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    ("Apple M1 (macOS)", "Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)"),
    (
        "Intel UHD Graphics 620 (Linux)",
        "Google Inc. (Intel)",
        "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)",
    ),
];

/// Time zones and language tags of the countries we know, sorted and without duplicates.
fn zones_and_languages() -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut tz, mut lang): (Vec<_>, Vec<_>) =
        countries::COUNTRIES.iter().filter_map(|(code, _)| countries::implied(code)).unzip();
    tz.push("UTC");
    for v in [&mut tz, &mut lang] {
        v.sort_unstable();
        v.dedup();
    }
    (tz, lang)
}

/// `Label   control` with the label centred on the control's first line, however tall the control gets.
fn form_row<R>(ui: &mut Ui, p: &Palette, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = LABEL_GAP;
            ui.allocate_ui_with_layout(vec2(LABEL_W, 36.0), Layout::right_to_left(Align::Center), |ui| {
                ui.set_min_width(LABEL_W);
                ui.label(RichText::new(label).size(14.0).color(p.strong));
            });
            ui.vertical(|ui| {
                ui.set_max_width(FIELD_W.min(ui.available_width()));
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.spacing_mut().item_spacing.x = 10.0;
                add(ui)
            })
            .inner
        })
        .inner;
    ui.add_space(ROW_GAP);
    r
}

fn field(text: &mut String) -> TextEdit<'_> {
    TextEdit::singleline(text).margin(Margin::symmetric(10, 8)).desired_width(f32::INFINITY)
}

fn hint(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add(egui::Label::new(small_dim(p, text)).wrap());
}

fn error_line(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(p.err).size(12.5)).wrap());
}

/// A text field with a "Presets" menu next to it.
fn with_presets(ui: &mut Ui, id: &str, text: &mut String, hint_text: &str, presets: &[&str]) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 110.0).max(140.0);
        ch |= ui.add(field(text).hint_text(hint_text).desired_width(w)).changed();
        ComboBox::from_id_salt(id)
            .width(92.0)
            .icon(widgets::chevron_icon)
            .selected_text("Presets")
            .height(320.0)
            .show_ui(ui, |ui| {
                for v in presets {
                    if ui.selectable_label(text == v, *v).clicked() {
                        *text = (*v).to_owned();
                        ch = true;
                    }
                }
            });
    });
    ch
}

impl EditDialog {
    pub(super) fn show(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], kernel: Option<&str>) -> Do {
        let mut act = Do::Nothing;
        let mut changed = false;
        let existing = !self.id.is_empty();

        // header: Back, title, serial
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let back = ui.add(egui::Button::new(RichText::new("\u{2039}  Back").color(p.text)).frame(false));
            if back.on_hover_cursor(CursorIcon::PointingHand).clicked() && !self.saving {
                act = Do::Cancel;
            }
            let (sep, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
            ui.painter().rect_filled(sep, CornerRadius::ZERO, p.border_strong);
            let title = if self.template {
                "Defaults for new profiles"
            } else if existing {
                "Edit profile"
            } else {
                "New profile"
            };
            ui.label(semibold(title, 21.0));
            if existing {
                widgets::badge(ui, &format!("#{}", self.serial), p.accent_hover);
            }
            if self.loading {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        ui.add_space(6.0);
        let template = self.template;
        if let Some(i) = widgets::tab_header(ui, p, &SECTIONS, self.current, |ui| {
            if template {
                return;
            }
            let r = ui.add(egui::Button::new(RichText::new("New fingerprint").color(p.accent_hover)).frame(false));
            if r.on_hover_text("A fresh seed: every derived value (hardware, screen, GPU, noise) changes at once")
                .clicked()
            {
                self.new_fingerprint();
                self.touched = true;
            }
        }) {
            self.jump = Some(i);
        }

        // body: form on the left, overview on the right
        let body_h = (ui.available_height() - FOOTER_H).max(120.0);
        let full_w = ui.available_width();
        let show_overview = full_w >= 900.0;
        let form_w = if show_overview { full_w - OVERVIEW_W - 28.0 } else { full_w };
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 28.0;
            ui.allocate_ui_with_layout(vec2(form_w, body_h), Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(vec2(form_w, body_h));
                changed |= self.form(ui, p, groups, &mut act, body_h);
            });
            if show_overview {
                ui.allocate_ui_with_layout(vec2(OVERVIEW_W, body_h), Layout::top_down(Align::Min), |ui| {
                    ScrollArea::vertical().id_salt("edit-overview").max_height(body_h).show(ui, |ui| {
                        self.overview(ui, p, kernel);
                    });
                });
            }
        });
        if changed {
            self.touched = true;
        }

        // footer
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(line, CornerRadius::ZERO, p.border);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W + 16.0);
            let label = if self.template {
                "Save defaults"
            } else if existing {
                "Save changes"
            } else {
                "Create profile"
            };
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
            } else {
                ui.label(small_dim(p, "Ctrl/Cmd + Enter saves"));
            }
        });
        if ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command) {
            act = Do::Save;
        }
        act
    }

    fn form(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], act: &mut Do, h: f32) -> bool {
        let mut ch = false;
        let mut tops = [0.0f32; 5];
        let out =
            ScrollArea::vertical().id_salt("edit-form").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
                // rows space themselves (`ROW_GAP`)
                ui.spacing_mut().item_spacing.y = 0.0;
                Frame::NONE.inner_margin(Margin { left: 0, right: 18, top: 0, bottom: 24 }).show(ui, |ui| {
                    for (i, top) in tops.iter_mut().enumerate() {
                        *top = ui.cursor().top();
                        if self.jump == Some(i) {
                            ui.scroll_to_cursor(Some(Align::TOP));
                            self.jump = None;
                        }
                        self.section_title(ui, p, i);
                        ch |= match i {
                            0 => self.general(ui, p, groups, act),
                            1 => self.proxy_block(ui, p, act),
                            2 => self.platform(ui, p),
                            3 => self.fingerprint_block(ui, p),
                            _ => self.advanced(ui, p),
                        };
                        ui.add_space(14.0);
                    }
                });
            });
        // the highlighted tab: the last section whose title has scrolled to (near) the top, or the last one at
        // the very bottom of the page
        let view_top = out.inner_rect.top() + 48.0;
        let at_end = out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 4.0;
        self.current = if at_end && out.state.clamped_offset().y > 0.0 {
            SECTIONS.len() - 1
        } else {
            tops.iter().rposition(|t| *t <= view_top).unwrap_or(0)
        };
        ch
    }

    fn section_title(&self, ui: &mut Ui, p: &Palette, i: usize) {
        ui.add_space(if i == 0 { 8.0 } else { 18.0 });
        ui.horizontal(|ui| {
            ui.label(semibold(SECTIONS[i], 17.0));
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            let y = r.center().y;
            ui.painter().hline(r.left() + 10.0..=r.right(), y, Stroke::new(1.0, p.border));
        });
        ui.add_space(22.0);
    }

    // ----------------------------------------------------------------------------------------------------------

    fn general(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], act: &mut Do) -> bool {
        let mut ch = false;
        if self.template {
            form_row(ui, p, "", |ui| {
                hint(
                    ui,
                    p,
                    "Every new profile starts with these settings; change them per profile as you like. Names, groups, \
                     proxy logins, accounts and cookies are set per profile.",
                );
            });
        } else {
            ch |= self.identity_rows(ui, p, groups, act);
        }
        form_row(ui, p, "Browser", |ui| {
            let shown = BRANDS.iter().find(|b| b.0 == self.brand).map_or("Chrome", |b| b.1);
            ComboBox::from_id_salt("edit-brand").width(240.0).icon(widgets::chevron_icon).selected_text(shown).show_ui(
                ui,
                |ui| {
                    for (v, label) in BRANDS {
                        ch |= ui.selectable_value(&mut self.brand, v.to_owned(), label).changed();
                    }
                },
            );
            hint(ui, p, "Clearcote kernel (Chromium). The brand sets the name and version tokens the browser reports.");
        });
        form_row(ui, p, "OS", |ui| {
            ch |= widgets::segmented(ui, p, &mut self.os, &OS_CHOICES.map(|(v, l)| (v.to_owned(), l)));
            let col = if self.os == "windows" { p.warn } else { p.dim };
            ui.add(egui::Label::new(RichText::new(OS_HINT).size(12.0).color(col)).wrap());
        });
        form_row(ui, p, "User-Agent", |ui| {
            ch |= ui
                .add(
                    field(&mut self.ua)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("Automatic (matches OS and browser)"),
                )
                .changed();
            hint(
                ui,
                p,
                "Leave empty: the kernel writes a user agent that matches the persona. A custom one is used to pick \
                 the OS and brand; the version always comes from the installed kernel.",
            );
        });
        form_row(ui, p, "Search engine", |ui| {
            let shown = SEARCH_CHOICES.iter().find(|c| c.0 == self.search_engine).map_or("Google", |c| c.1);
            ComboBox::from_id_salt("edit-search")
                .width(240.0)
                .icon(widgets::chevron_icon)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (v, label) in SEARCH_CHOICES {
                        ch |= ui.selectable_value(&mut self.search_engine, v.to_owned(), label).changed();
                    }
                });
            hint(
                ui,
                p,
                "What the address bar searches with. Set when the browser first opens and whenever you change it here; \
                 you can still pick another one inside the browser.",
            );
        });
        if !self.template {
            ch |= self.account_rows(ui, p, act);
        }
        ch
    }

    /// Name and group (not part of the defaults).
    fn identity_rows(&mut self, ui: &mut Ui, p: &Palette, groups: &[Group], act: &mut Do) -> bool {
        let mut ch = false;
        form_row(ui, p, "Name", |ui| {
            let r = ui.add(field(&mut self.name).hint_text("e.g. Vinted FR - Camille"));
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
                    .icon(widgets::chevron_icon)
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
                    let r = ui.add(field(&mut self.new_group).hint_text("New group name").desired_width(240.0));
                    let name = self.new_group.trim().to_owned();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let clicked = ui.add_enabled(!name.is_empty(), egui::Button::new("Create")).clicked();
                    if (clicked || enter) && !name.is_empty() {
                        *act = Do::CreateGroup(name);
                    }
                });
            }
            hint(
                ui,
                p,
                "A group can add its own start pages and extensions (Profiles \u{2192} Groups \u{2192} Settings).",
            );
        });
        ch
    }

    /// Cookie and remark (not part of the defaults).
    fn account_rows(&mut self, ui: &mut Ui, p: &Palette, act: &mut Do) -> bool {
        let mut ch = false;
        let existing = !self.id.is_empty();
        form_row(ui, p, "Cookie", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.cookie)
                    .font(egui::TextStyle::Monospace)
                    .hint_text(
                        "Paste cookies: JSON (Cookie-Editor, EditThisCookie), Netscape cookies.txt or name=value",
                    )
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(6)
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let n = self.cookie.lines().filter(|l| l.trim_start().starts_with('{')).count();
                if n > 0 {
                    ui.label(small_dim(p, format!("{n} cookies")));
                }
                if existing
                    && ui
                        .add_enabled(!self.cookies_reading, egui::Button::new("Read from the open browser").small())
                        .on_hover_text(
                            "Puts the cookies the profile's browser holds right now in this field (the profile must be open)",
                        )
                        .clicked()
                {
                    *act = Do::ReadCookies;
                }
                if self.cookies_reading {
                    ui.add(egui::Spinner::new().size(12.0));
                }
                if !self.cookie.trim().is_empty() {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(self.cookie.clone());
                    }
                    if ui.small_button("Clear").on_hover_text("Delete the cookies when you save").clicked() {
                        self.cookie.clear();
                        ch = true;
                    }
                }
            });
            match &self.cookie_note {
                Some(Ok(m)) => {
                    ui.label(RichText::new(m).color(p.ok).size(12.5));
                }
                Some(Err(e)) => error_line(ui, p, e),
                None => {}
            }
            hint(
                ui,
                p,
                "The cookies of this profile. When Citroenition closes the browser (or you close its window) the \
                 browser's current cookies are saved here. Cookies you paste or change go into the browser the next \
                 time it opens. A name=value list uses the platform's site.",
            );
        });
        form_row(ui, p, "Remark", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.remark)
                    .hint_text("Optional note, shown in the list")
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .char_limit(2000),
            );
            ch |= r.changed();
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.label(small_dim(p, format!("{} / 2000", self.remark.chars().count())));
            });
        });
        ch
    }

    fn proxy_block(&mut self, ui: &mut Ui, p: &Palette, act: &mut Do) -> bool {
        let (mut ch, mut fields) = (false, false);
        form_row(ui, p, "Quick paste", |ui| {
            let r =
                ui.add(field(&mut self.url).font(egui::TextStyle::Monospace).hint_text("socks5://user:pass@host:port"));
            if r.changed() {
                self.sync_from_url();
                ch = true;
            }
            if r.lost_focus() && self.url_err.is_none() {
                self.sync_to_url();
            }
            match &self.url_err {
                Some(e) if !self.url.trim().is_empty() => error_line(ui, p, e),
                _ => hint(ui, p, URL_HINT),
            }
        });
        form_row(ui, p, "Proxy type", |ui| {
            let kind_box = ComboBox::from_id_salt("edit-proxy-kind").width(240.0).icon(widgets::chevron_icon);
            kind_box.selected_text(self.kind.label()).show_ui(ui, |ui| {
                for k in ProxyKind::ALL {
                    fields |= ui.selectable_value(&mut self.kind, k, k.label()).changed();
                }
            });
        });
        let direct = self.kind == ProxyKind::Direct;
        if !direct {
            form_row(ui, p, "Host : Port", |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 110.0 - ui.spacing().item_spacing.x * 2.0 - 8.0).max(120.0);
                    fields |= ui.add(field(&mut self.host).hint_text("host name or IP").desired_width(w)).changed();
                    ui.label(":");
                    fields |= ui.add(field(&mut self.port).hint_text("port").desired_width(110.0)).changed();
                });
            });
        }
        if !direct && !self.template {
            form_row(ui, p, "Proxy username", |ui| {
                fields |= ui.add(field(&mut self.user).hint_text("optional")).changed();
            });
            form_row(ui, p, "Proxy password", |ui| {
                ui.horizontal(|ui| {
                    let forget = self.pass_stored && self.pass.is_empty();
                    let w = if forget { ui.available_width() - 90.0 } else { ui.available_width() };
                    let hint_text = if self.pass_stored { "saved \u{2013} type to replace" } else { "optional" };
                    fields |=
                        ui.add(field(&mut self.pass).password(true).hint_text(hint_text).desired_width(w)).changed();
                    if forget && ui.button("Forget").on_hover_text("Remove the saved proxy password").clicked() {
                        self.pass_stored = false;
                        fields = true;
                    }
                });
            });
            form_row(ui, p, "", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let ready = self.proxy_problem().is_none() && !self.checking;
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
        }
        form_row(ui, p, "Country", |ui| {
            let shown = match countries::label(&self.country) {
                s if s.is_empty() => "Auto (from the proxy IP)".to_owned(),
                s => s,
            };
            let country_box = ComboBox::from_id_salt("edit-country").width(280.0).icon(widgets::chevron_icon);
            country_box.selected_text(shown).height(360.0).show_ui(ui, |ui| {
                ch |= ui.selectable_value(&mut self.country, String::new(), "Auto (from the proxy IP)").changed();
                for (code, name) in countries::COUNTRIES {
                    let text = format!("{name}  ({})", code.to_uppercase());
                    ch |= ui.selectable_value(&mut self.country, (*code).to_owned(), text).changed();
                }
            });
            let text = match countries::implied(&self.country) {
                Some((tz, lang)) => format!("Language {lang} \u{b7} time zone {tz} when they follow the IP"),
                None => "Language and time zone follow the proxy's exit country.".to_owned(),
            };
            hint(ui, p, &text);
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

    fn platform(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        form_row(ui, p, "Platform", |ui| {
            ch |= ui.add(field(&mut self.domain).hint_text("e.g. google.com")).changed();
            hint(ui, p, "Opened when the browser starts and there are no start URLs.");
        });
        if !self.template {
            ch |= self.platform_account(ui, p);
        }
        form_row(ui, p, "Start URLs", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.tabs)
                    .hint_text("https://example.com")
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            match self.tabs_problem() {
                Some(m) if self.tried => error_line(ui, p, &m),
                _ => hint(
                    ui,
                    p,
                    "One address per line, each in its own tab. When the browser brings back its last session, start \
                     pages you added or changed since also open. The group's start pages open first.",
                ),
            }
        });
        form_row(ui, p, "Bookmarks", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.bookmarks)
                    .hint_text("TikTok Ads | https://ads.tiktok.com/")
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            hint(
                ui,
                p,
                "Name | URL, one per line (or just the URL). They go on the bookmarks bar, after the group's. \
                 Bookmarks you add in the browser itself are kept.",
            );
        });
        ch
    }

    fn platform_account(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        form_row(ui, p, "Username", |ui| {
            ch |= ui.add(field(&mut self.plat_user).hint_text("The account on this platform (optional)")).changed();
        });
        form_row(ui, p, "Password", |ui| {
            ui.horizontal(|ui| {
                let forget = self.plat_pass_stored && self.plat_pass.is_empty();
                let w = if forget { ui.available_width() - 90.0 } else { ui.available_width() };
                let hint_text = if self.plat_pass_stored { "saved \u{2013} type to replace" } else { "optional" };
                ch |= ui.add(field(&mut self.plat_pass).password(true).hint_text(hint_text).desired_width(w)).changed();
                if forget && ui.button("Forget").clicked() {
                    self.plat_pass_stored = false;
                    ch = true;
                }
            });
            hint(ui, p, "Stored for your reference and shared with the team; never typed in for you.");
        });
        ch
    }

    fn fingerprint_block(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        let (probe, reading) = self.current_probe();
        let probe = probe.cloned();
        if !self.template {
            form_row(ui, p, "", |ui| {
                Frame::NONE
                    .fill(p.raised)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(dim(
                                p,
                                "\u{201c}Based on seed\u{201d}: the value comes from this profile's seed, a code that \
                                 stands for one made-up computer. The same seed always gives the same values, so the \
                                 profile looks like the same machine every time and its values never contradict each \
                                 other. What each setting gives right now is shown next to it and in the Overview, \
                                 read from the browser itself.",
                            ))
                            .wrap(),
                        );
                    });
            });
        }
        let seen = |f: &dyn Fn(&citro_core::probe::Probe) -> String| -> Option<String> { probe.as_ref().map(f) };
        let measured = |ui: &mut Ui, text: Option<String>| {
            if let Some(t) = text {
                ui.label(RichText::new(format!("This profile: {t}")).color(p.accent_hover).size(12.5));
            } else if reading {
                ui.label(small_dim(p, "Reading from the browser\u{2026}"));
            }
        };
        let (zones, langs) = zones_and_languages();
        form_row(ui, p, "WebRTC", |ui| {
            ch |= widgets::segmented(ui, p, &mut self.webrtc, &WEBRTC.map(|(v, l)| (v.to_owned(), l)));
            let text = match self.webrtc.as_str() {
                "proxy" => "Sites see the proxy's exit IP in WebRTC, as a real browser behind it would.",
                "local" => "Sites see this computer's real IP addresses. Only for testing without a proxy.",
                _ => "No WebRTC traffic outside the proxy: the leak-proof default.",
            };
            hint(ui, p, text);
        });
        form_row(ui, p, "Timezone", |ui| {
            ch |= widgets::segmented(
                ui,
                p,
                &mut self.tz_mode,
                &[(Follow::Auto, "Based on IP"), (Follow::Custom, "Custom")],
            );
            if self.tz_mode == Follow::Custom {
                ch |= with_presets(ui, "edit-tz", &mut self.timezone, "Europe/Paris", &zones);
            }
        });
        form_row(ui, p, "Location", |ui| {
            ch |=
                widgets::segmented(ui, p, &mut self.loc_mode, &[(Follow::Auto, "Not set"), (Follow::Custom, "Custom")]);
            if self.loc_mode == Follow::Custom {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    ch |= ui.add(field(&mut self.lat).hint_text("latitude, e.g. 48.8566").desired_width(w)).changed();
                    ch |= ui.add(field(&mut self.lon).hint_text("longitude, e.g. 2.3522").desired_width(w)).changed();
                });
            } else {
                hint(ui, p, "Pages that ask for the location get no fixed position.");
            }
        });
        form_row(ui, p, "Language", |ui| {
            ch |= widgets::segmented(
                ui,
                p,
                &mut self.lang_mode,
                &[(Follow::Auto, "Based on IP"), (Follow::Custom, "Custom")],
            );
            if self.lang_mode == Follow::Custom {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 110.0).max(140.0);
                    ch |= ui.add(field(&mut self.languages).hint_text("en-US, en").desired_width(w)).changed();
                    ComboBox::from_id_salt("edit-lang-add")
                        .width(92.0)
                        .icon(widgets::chevron_icon)
                        .selected_text("Add")
                        .height(320.0)
                        .show_ui(ui, |ui| {
                            for l in &langs {
                                if ui.selectable_label(false, *l).clicked()
                                    && !self.language_list().iter().any(|x| x == l)
                                {
                                    if !self.languages.trim().is_empty() {
                                        self.languages.push_str(", ");
                                    }
                                    self.languages.push_str(l);
                                    ch = true;
                                }
                            }
                        });
                });
                hint(ui, p, "In order of preference. One tag expands to what a fresh Chrome shows for that locale.");
            }
        });
        form_row(ui, p, "Display language", |ui| {
            ch |= widgets::segmented(
                ui,
                p,
                &mut self.page_lang_mode,
                &[(Follow::Auto, "Based on language"), (Follow::Custom, "Custom")],
            );
            if self.page_lang_mode == Follow::Custom {
                ch |= with_presets(ui, "edit-page-lang", &mut self.page_lang, "en-US", &langs);
            }
        });
        form_row(ui, p, "Screen resolution", |ui| {
            ch |= widgets::segmented(
                ui,
                p,
                &mut self.screen_mode,
                &[(Hw::Seed, "Based on seed"), (Hw::Custom, "Custom")],
            );
            if self.screen_mode == Hw::Custom {
                ch |= with_presets(ui, "edit-screen", &mut self.screen, "1920 x 1080", &SCREENS);
            } else {
                measured(
                    ui,
                    seen(&|x| {
                        format!(
                            "{} x {} (available {} x {})",
                            x.screen.0, x.screen.1, x.avail_screen.0, x.avail_screen.1
                        )
                    }),
                );
            }
        });
        form_row(ui, p, "Hardware noise", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                ch |= widgets::switch(ui, p, &mut self.canvas, "Canvas");
                ch |= widgets::switch(ui, p, &mut self.webgl_image, "WebGL image");
            });
            hint(
                ui,
                p,
                "Noise changes the canvas, WebGL, audio and text-size readings a little, the same way every time for \
                 this seed, so sites cannot match this profile with others. AudioContext, ClientRects, media devices \
                 and speech voices always come from the seed. WebGL image noise is only turned off together with \
                 canvas.",
            );
            measured(
                ui,
                seen(&|x| {
                    format!(
                        "canvas {} \u{b7} WebGL {} \u{b7} audio {} \u{b7} rects {}",
                        x.canvas_hash, x.webgl_hash, x.audio_hash, x.rects_hash
                    )
                }),
            );
        });
        form_row(ui, p, "WebGL metadata", |ui| {
            ch |= widgets::segmented(
                ui,
                p,
                &mut self.gl_mode,
                &[(Hw::Seed, "Based on seed"), (Hw::Real, "Real"), (Hw::Custom, "Custom")],
            );
            if self.gl_mode == Hw::Custom {
                ComboBox::from_id_salt("edit-gpu-preset")
                    .width(300.0)
                    .icon(widgets::chevron_icon)
                    .selected_text("Choose a common GPU\u{2026}")
                    .show_ui(ui, |ui| {
                        for (label, v, r) in GPUS {
                            if ui.selectable_label(self.gl_renderer == r, label).clicked() {
                                (self.gl_vendor, self.gl_renderer) = (v.to_owned(), r.to_owned());
                                ch = true;
                            }
                        }
                    });
                Frame::NONE.fill(p.raised).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(14)).show(
                    ui,
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        ui.horizontal(|ui| {
                            ui.add_sized(vec2(70.0, 30.0), egui::Label::new(dim(p, "Vendor")));
                            ch |= ui.add(field(&mut self.gl_vendor).hint_text("Google Inc. (NVIDIA)")).changed();
                        });
                        ui.horizontal(|ui| {
                            ui.add_sized(vec2(70.0, 30.0), egui::Label::new(dim(p, "Renderer")));
                            ch |= ui.add(field(&mut self.gl_renderer).hint_text("ANGLE (NVIDIA, \u{2026})")).changed();
                        });
                    },
                );
            } else if self.gl_mode == Hw::Real {
                hint(ui, p, "This computer's own GPU: names, limits and pixels alike.");
            } else {
                measured(ui, seen(&|x| format!("{}\n{}", x.gpu_vendor, x.gpu_renderer)));
            }
        });
        form_row(ui, p, "CPU cores", |ui| {
            let now = seen(&|x| format!("{} cores", x.cores));
            ch |= hw_combo(ui, "edit-cpu", &mut self.cpu, &CPUS, "cores", now);
        });
        form_row(ui, p, "Memory", |ui| {
            let now = seen(&|x| format!("{} GB", x.memory));
            ch |= hw_combo(ui, "edit-ram", &mut self.ram, &RAMS, "GB", now);
            hint(ui, p, "What sites read as navigator.deviceMemory.");
        });
        if !self.template {
            form_row(ui, p, "Fingerprint seed", |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 150.0).max(160.0);
                    let auto = if self.id.is_empty() {
                        "the profile id (assigned on create)".to_owned()
                    } else {
                        self.id.clone()
                    };
                    ch |= ui
                        .add(
                            field(&mut self.custom_seed)
                                .font(egui::TextStyle::Monospace)
                                .hint_text(auto)
                                .desired_width(w),
                        )
                        .changed();
                    if ui.button("New").on_hover_text("Generate a new fingerprint").clicked() {
                        self.new_fingerprint();
                        ch = true;
                    }
                    let seed = self.resolved().seed;
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(seed);
                    }
                });
                hint(
                    ui,
                    p,
                    "One seed is one coherent machine. Empty = the profile id, so a profile keeps its identity.",
                );
            });
        }
        if self.tried
            && let Some(m) = self.fingerprint_problem()
        {
            form_row(ui, p, "", |ui| error_line(ui, p, &m));
        }
        ch
    }

    fn advanced(&mut self, ui: &mut Ui, p: &Palette) -> bool {
        let mut ch = false;
        form_row(ui, p, "Launch arguments", |ui| {
            let r = ui.add(
                TextEdit::multiline(&mut self.launch_args)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("--disable-features=Translate")
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ch |= r.changed();
            match self.advanced_problem() {
                Some(m) if self.tried => error_line(ui, p, &m),
                _ => hint(ui, p, "Extra Chromium switches, one per line; each must start with --."),
            }
        });
        ch
    }

    // ----------------------------------------------------------------------------------------------------------
    // overview

    fn overview(&self, ui: &mut Ui, p: &Palette, kernel: Option<&str>) {
        let r = self.resolved();
        let (probe, reading) = self.current_probe();
        let failed = match &self.probe {
            Some(Err(e)) if !reading => Some(e.clone()),
            _ => None,
        };
        let pr = |f: &dyn Fn(&citro_core::probe::Probe) -> String, otherwise: &str| -> String {
            probe.map_or_else(|| otherwise.to_owned(), f)
        };
        let noise = |on: bool, f: &dyn Fn(&citro_core::probe::Probe) -> String| -> String {
            if on { probe.map_or("Noise".to_owned(), |x| format!("Noise [{}]", f(x))) } else { "Real".to_owned() }
        };
        let os = match r.os {
            Os::Windows => "Windows",
            Os::Mac => "macOS",
            Os::Linux => "Linux",
        };
        let brand = BRANDS.iter().find(|b| b.0 == r.brand).map_or("Chrome", |b| b.1);
        let seed_or = |v: Option<String>, f: &dyn Fn(&citro_core::probe::Probe) -> String| {
            v.unwrap_or_else(|| pr(f, "Based on seed"))
        };
        let rows: Vec<(&str, String)> = vec![
            ("Browser", format!("{brand} \u{b7} Clearcote {}", kernel.unwrap_or("(not installed)"))),
            ("OS", os.to_owned()),
            (
                "User-Agent",
                if self.ua.trim().is_empty() {
                    pr(&|x| x.user_agent.clone(), "Automatic")
                } else {
                    self.ua.trim().to_owned()
                },
            ),
            ("Platform", pr(&|x| x.platform.clone(), "Based on OS")),
            (
                "Proxy",
                match self.kind {
                    ProxyKind::Direct => "Direct".to_owned(),
                    k => format!("{} {}:{}", k.scheme(), self.host.trim(), self.port.trim()),
                },
            ),
            (
                "WebRTC",
                match &r.webrtc {
                    WebRtc::Leakproof => "Disabled (leak-proof)".to_owned(),
                    WebRtc::Address(ip) => format!("Replace \u{2192} {ip}"),
                    WebRtc::Local => "Real".to_owned(),
                },
            ),
            ("Timezone", r.timezone.clone().unwrap_or_else(|| "System".to_owned())),
            ("Location", r.location.map_or("Not set".to_owned(), |(la, lo)| format!("{la:.4}, {lo:.4}"))),
            ("Language", r.accept_lang.clone()),
            ("Display language", r.ui_locale.clone().unwrap_or_else(|| "Default".to_owned())),
            (
                "Screen",
                seed_or(r.screen.map(|(w, h)| format!("{w} x {h}")), &|x| format!("{} x {}", x.screen.0, x.screen.1)),
            ),
            ("Canvas", noise(r.noise == Noise::On, &|x| x.canvas_hash.clone())),
            ("WebGL image", noise(r.noise != Noise::AllOff, &|x| x.webgl_hash.clone())),
            ("AudioContext", noise(true, &|x| x.audio_hash.clone())),
            ("ClientRects", noise(true, &|x| x.rects_hash.clone())),
            ("Speech voices", pr(&|x| format!("{} voices", x.voices), "Based on seed")),
            (
                "Media devices",
                pr(
                    &|x| {
                        format!(
                            "{} mic, {} speaker, {} camera",
                            x.media_devices.0, x.media_devices.1, x.media_devices.2
                        )
                    },
                    "Based on seed",
                ),
            ),
            (
                "WebGL metadata",
                match &r.gpu {
                    Gpu::Persona => pr(&|x| format!("{}\n{}", x.gpu_vendor, x.gpu_renderer), "Based on seed"),
                    Gpu::Real => "This computer's GPU".to_owned(),
                    Gpu::Custom(v, rd) => format!("{v}\n{rd}"),
                },
            ),
            ("CPU", seed_or(r.hardware_concurrency.map(|v| format!("{v} cores")), &|x| format!("{} cores", x.cores))),
            ("Memory", seed_or(r.device_memory.map(|v| format!("{v} GB")), &|x| format!("{} GB", x.memory))),
            (
                "Seed",
                if self.id.is_empty() && self.custom_seed.trim().is_empty() {
                    "Assigned on create".to_owned()
                } else {
                    r.seed.clone()
                },
            ),
        ];
        Frame::NONE
            .fill(p.raised)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(semibold("Overview", 16.0));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if self.template {
                        ui.label(small_dim(p, "What sites will see, computed live from this form."));
                    } else if reading {
                        ui.add(egui::Spinner::new().size(11.0));
                        ui.label(small_dim(p, "Reading from the browser\u{2026}"));
                    } else {
                        ui.label(small_dim(p, "What sites will see, read from the browser itself."));
                    }
                });
                if let Some(e) = &failed {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Could not read the browser: {e}")).color(p.warn).size(12.0),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(10.0);
                ui.spacing_mut().item_spacing.y = 9.0;
                for (k, v) in rows {
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(vec2(112.0, 16.0), Layout::left_to_right(Align::Min), |ui| {
                            ui.set_min_width(112.0);
                            ui.label(RichText::new(k).color(p.dim).size(12.5));
                        });
                        ui.with_layout(Layout::top_down(Align::Max), |ui| {
                            let text = RichText::new(v).color(p.strong).size(12.5);
                            let text = if k == "Seed" || k == "User-Agent" {
                                text.family(theme::medium_family())
                            } else {
                                text
                            };
                            ui.add(egui::Label::new(text).wrap().halign(Align::Max));
                        });
                    });
                }
            });
    }
}

/// "Based on seed" or a fixed number from a short list.
fn hw_combo(ui: &mut Ui, id: &str, value: &mut String, choices: &[&str], unit: &str, now: Option<String>) -> bool {
    let mut ch = false;
    let shown = match (value.is_empty(), now) {
        (true, Some(n)) => format!("Based on seed ({n})"),
        (true, None) => "Based on seed".to_owned(),
        _ => format!("{value} {unit}"),
    };
    ComboBox::from_id_salt(id).width(240.0).icon(widgets::chevron_icon).selected_text(shown).show_ui(ui, |ui| {
        ch |= ui.selectable_value(value, String::new(), "Based on seed").changed();
        for c in choices {
            ch |= ui.selectable_value(value, (*c).to_owned(), format!("{c} {unit}")).changed();
        }
    });
    ch
}
