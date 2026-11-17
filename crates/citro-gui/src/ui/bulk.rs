//! Bulk edit: change many profiles at once. Every part starts at "Keep", so only what the user picks changes.

use super::app::{CitroApp, ToastKind};
use super::tasks::Refresh;
use super::widgets::{self, dim, semibold, small_dim};
use crate::backend::Tag;
use crate::proxy::{self, ProxyKind};
use citro_core::profile::{Profile, ProxyConfig};
use eframe::egui::{self, Align, Color32, ComboBox, Layout, Margin, RichText, ScrollArea, TextEdit, Ui, vec2};

const LABEL_W: f32 = 110.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    #[default]
    Keep,
    /// One proxy for every profile.
    One,
    /// One per line, handed out in order (round robin when there are fewer lines than profiles).
    List,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextMode {
    #[default]
    Keep,
    Replace,
    Add,
}

#[derive(Default)]
pub struct BulkDialog {
    pub ids: Vec<String>,
    group: Option<String>,
    proxy_mode: ProxyMode,
    proxies: String,
    remark_mode: TextMode,
    remark: String,
    tabs_mode: TextMode,
    tabs: String,
    os: Option<String>,
    tags_add: Vec<String>,
    tags_remove: Vec<String>,
    error: Option<String>,
}

/// The proxies the dialog hands out, or what is wrong with them. `None` = leave the proxy alone.
fn proxies(d: &BulkDialog) -> Result<Option<Vec<ProxyConfig>>, String> {
    let lines: Vec<&str> = d.proxies.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    match d.proxy_mode {
        ProxyMode::Keep => Ok(None),
        ProxyMode::None => Ok(Some(vec![ProxyConfig { proxy_soft: Some("no_proxy".into()), ..Default::default() }])),
        ProxyMode::One | ProxyMode::List => {
            let lines = if d.proxy_mode == ProxyMode::One { lines.into_iter().take(1).collect() } else { lines };
            if lines.is_empty() {
                return Err("Paste a proxy (or pick Keep)".into());
            }
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    proxy::parse(l)
                        .map(|pp| proxy::to_config(&pp, ProxyKind::Http))
                        .map_err(|e| format!("Proxy line {}: {e}", i + 1))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
    }
}

/// Apply the text parts of the dialog to one profile (`n` = its place in the selection, for the proxy list).
fn apply_to(p: &mut Profile, n: usize, d: &BulkDialog, proxies: &Option<Vec<ProxyConfig>>) {
    if let Some(list) = proxies {
        p.user_proxy_config = list[n % list.len()].clone();
        p.ip.clear();
    }
    match d.remark_mode {
        TextMode::Keep => {}
        TextMode::Replace => p.remark = d.remark.trim().to_owned(),
        TextMode::Add => {
            let add = d.remark.trim();
            if !add.is_empty() {
                p.remark =
                    if p.remark.trim().is_empty() { add.to_owned() } else { format!("{}\n{add}", p.remark.trim_end()) };
            }
        }
    }
    let tabs: Vec<String> = d
        .tabs
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| if l.contains("://") || l.starts_with("about:") { l.to_owned() } else { format!("https://{l}") })
        .collect();
    match d.tabs_mode {
        TextMode::Keep => {}
        TextMode::Replace => p.tabs = tabs,
        TextMode::Add => {
            for t in tabs {
                if !p.tabs.contains(&t) {
                    p.tabs.push(t);
                }
            }
        }
    }
    if let Some(os) = &d.os {
        p.persona_os = os.clone();
    }
}

fn label_row(ui: &mut Ui, p: &crate::ui::theme::Palette, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_W, 30.0), Layout::right_to_left(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(dim(p, label));
        });
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        });
    });
}

fn rgb(key: &str) -> Color32 {
    let c = citro_core::profile::tag_rgb(key);
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

impl CitroApp {
    pub fn open_bulk(&mut self, ids: Vec<String>) {
        if !ids.is_empty() {
            self.bulk_dlg = Some(BulkDialog { ids, ..Default::default() });
        }
    }

    pub fn bulk_modal(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.bulk_dlg.take() else { return };
        let p = self.pal;
        let groups = self.groups.clone();
        let tags: Vec<Tag> = self.tags.clone();
        let n = d.ids.len();
        let (mut apply, mut cancel) = (false, false);
        let r = widgets::modal(ctx, &p, "bulk-modal", 660.0, |ui| {
            ui.label(semibold(format!("Bulk edit {n} profiles"), 19.0));
            ui.label(small_dim(&p, "Everything is on Keep: only what you change here is changed."));
            ui.add_space(10.0);
            ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.65).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                label_row(ui, &p, "Group", |ui| {
                    let shown = match &d.group {
                        None => "Keep".to_owned(),
                        Some(g) => crate::backend::group_label(&groups, g),
                    };
                    ComboBox::from_id_salt("bulk-group")
                        .width(280.0)
                        .icon(widgets::chevron_icon)
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut d.group, None, "Keep");
                            ui.selectable_value(&mut d.group, Some("0".into()), "Ungrouped");
                            for g in &groups {
                                ui.selectable_value(&mut d.group, Some(g.group_id.clone()), &g.group_name);
                            }
                        });
                });
                label_row(ui, &p, "Proxy", |ui| {
                    widgets::segmented(
                        ui,
                        &p,
                        &mut d.proxy_mode,
                        &[
                            (ProxyMode::Keep, "Keep"),
                            (ProxyMode::One, "Same for all"),
                            (ProxyMode::List, "One per line"),
                            (ProxyMode::None, "No proxy"),
                        ],
                    );
                    if matches!(d.proxy_mode, ProxyMode::One | ProxyMode::List) {
                        let one = d.proxy_mode == ProxyMode::One;
                        widgets::capped(
                            ui,
                            "bulk-proxies",
                            TextEdit::multiline(&mut d.proxies)
                                .font(egui::TextStyle::Monospace)
                                .hint_text(if one {
                                    "socks5://user:pass@host:port"
                                } else {
                                    "one proxy per line: the first profile gets line 1, the second line 2, \u{2026}"
                                })
                                .desired_rows(if one { 1 } else { 4 })
                                .margin(Margin::symmetric(10, 8))
                                .desired_width(f32::INFINITY),
                        );
                        if !one {
                            let k = d.proxies.lines().filter(|l| !l.trim().is_empty()).count();
                            let note = if k > 0 && k < n {
                                format!("{k} proxies for {n} profiles: the list starts again from the top")
                            } else {
                                format!("{k} proxies for {n} profiles, in the order of the list")
                            };
                            ui.label(small_dim(&p, note));
                        }
                    }
                });
                label_row(ui, &p, "Remark", |ui| {
                    widgets::segmented(
                        ui,
                        &p,
                        &mut d.remark_mode,
                        &[(TextMode::Keep, "Keep"), (TextMode::Replace, "Replace"), (TextMode::Add, "Add a line")],
                    );
                    if d.remark_mode != TextMode::Keep {
                        widgets::capped(
                            ui,
                            "bulk-remark",
                            TextEdit::multiline(&mut d.remark)
                                .desired_rows(2)
                                .margin(Margin::symmetric(10, 8))
                                .desired_width(f32::INFINITY)
                                .char_limit(2000),
                        );
                    }
                });
                label_row(ui, &p, "Tags", |ui| {
                    if tags.is_empty() {
                        ui.label(small_dim(&p, "No tags yet: create them with the tag button."));
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        for t in &tags {
                            // click: add -> remove -> leave alone
                            let state = if d.tags_add.contains(&t.id) {
                                "+"
                            } else if d.tags_remove.contains(&t.id) {
                                "\u{2212}"
                            } else {
                                ""
                            };
                            let text = if state.is_empty() { t.name.clone() } else { format!("{state} {}", t.name) };
                            let col = if state == "\u{2212}" { p.err } else { rgb(&t.color) };
                            let r = widgets::badge(ui, &text, col)
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Click: add to all \u{2192} remove from all \u{2192} leave alone");
                            if r.clicked() {
                                if d.tags_add.contains(&t.id) {
                                    d.tags_add.retain(|x| x != &t.id);
                                    d.tags_remove.push(t.id.clone());
                                } else if d.tags_remove.contains(&t.id) {
                                    d.tags_remove.retain(|x| x != &t.id);
                                } else {
                                    d.tags_add.push(t.id.clone());
                                }
                            }
                        }
                    });
                });
                label_row(ui, &p, "Start URLs", |ui| {
                    widgets::segmented(
                        ui,
                        &p,
                        &mut d.tabs_mode,
                        &[(TextMode::Keep, "Keep"), (TextMode::Replace, "Replace"), (TextMode::Add, "Add")],
                    );
                    if d.tabs_mode != TextMode::Keep {
                        widgets::capped(
                            ui,
                            "bulk-tabs",
                            TextEdit::multiline(&mut d.tabs)
                                .hint_text("one address per line")
                                .desired_rows(3)
                                .margin(Margin::symmetric(10, 8))
                                .desired_width(f32::INFINITY),
                        );
                    }
                });
                label_row(ui, &p, "OS", |ui| {
                    let mut os = d.os.clone().unwrap_or_else(|| "keep".into());
                    if widgets::segmented(
                        ui,
                        &p,
                        &mut os,
                        &[
                            ("keep".to_owned(), "Keep"),
                            (String::new(), "Auto"),
                            ("windows".to_owned(), "Windows"),
                            ("macos".to_owned(), "macOS"),
                            ("linux".to_owned(), "Linux"),
                        ],
                    ) {
                        d.os = (os != "keep").then_some(os);
                    }
                });
            });
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(p.err));
            }
            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + 14.0);
                if widgets::primary_button(ui, &p, &format!("Apply to {n} profiles"), true).clicked() {
                    apply = true;
                }
                if widgets::button(ui, "Cancel", true).clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || r.should_close() {
            return;
        }
        if apply {
            match proxies(&d) {
                Err(e) => d.error = Some(e),
                Ok(list) => {
                    self.run_bulk(d, list);
                    return;
                }
            }
        }
        self.bulk_dlg = Some(d);
    }

    fn run_bulk(&mut self, d: BulkDialog, proxies: Option<Vec<ProxyConfig>>) {
        let n = d.ids.len();
        let ids = d.ids.clone();
        let texts = d.proxy_mode != ProxyMode::Keep
            || d.remark_mode != TextMode::Keep
            || d.tabs_mode != TextMode::Keep
            || d.os.is_some();
        if proxies.is_some() {
            for id in &ids {
                self.profiles.checks.remove(id);
            }
        }
        self.toast(ToastKind::Info, format!("Changing {n} profiles\u{2026}"));
        self.run_op(ids.clone(), format!("Changed {n} profiles"), Refresh::ROWS, move |b| {
            if texts {
                for (i, id) in d.ids.iter().enumerate() {
                    let mut p = b.profile(id)?;
                    apply_to(&mut p, i, &d, &proxies);
                    b.save_profile(p)?;
                }
            }
            if let Some(g) = &d.group {
                b.move_to_group(&d.ids, g)?;
            }
            if !d.tags_add.is_empty() || !d.tags_remove.is_empty() {
                b.retag(&d.ids, &d.tags_add, &d.tags_remove)?;
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxy_list_is_handed_out_in_order_and_starts_again() {
        let d = BulkDialog {
            proxy_mode: ProxyMode::List,
            proxies: "1.1.1.1:80\n\nsocks5://u:p@2.2.2.2:1080\n".into(),
            remark_mode: TextMode::Add,
            remark: "batch 2".into(),
            tabs_mode: TextMode::Add,
            tabs: "tiktok.com".into(),
            os: Some("macos".into()),
            ..Default::default()
        };
        let list = proxies(&d).unwrap();
        let mut ps: Vec<Profile> = (0..3)
            .map(|i| Profile { remark: if i == 0 { "old".into() } else { String::new() }, ..Default::default() })
            .collect();
        for (i, p) in ps.iter_mut().enumerate() {
            apply_to(p, i, &d, &list);
        }
        let hosts: Vec<String> = ps.iter().map(|p| p.user_proxy_config.hop().unwrap().host).collect();
        assert_eq!(hosts, ["1.1.1.1", "2.2.2.2", "1.1.1.1"]);
        assert_eq!(ps[1].user_proxy_config.hop().unwrap().account, "u");
        assert_eq!((ps[0].remark.as_str(), ps[1].remark.as_str()), ("old\nbatch 2", "batch 2"));
        assert_eq!(ps[2].tabs, ["https://tiktok.com"]);
        assert_eq!(ps[0].persona_os, "macos");
    }

    #[test]
    fn keep_changes_nothing_and_bad_proxies_are_named() {
        let d = BulkDialog::default();
        let mut p = Profile { remark: "r".into(), tabs: vec!["https://a".into()], ..Default::default() };
        let before = p.clone();
        apply_to(&mut p, 0, &d, &proxies(&d).unwrap());
        assert_eq!(p, before);
        let bad =
            BulkDialog { proxy_mode: ProxyMode::List, proxies: "1.1.1.1:80\nnot a proxy".into(), ..Default::default() };
        assert!(proxies(&bad).unwrap_err().contains("line 2"));
        let empty = BulkDialog { proxy_mode: ProxyMode::One, ..Default::default() };
        assert!(proxies(&empty).is_err());
    }
}
