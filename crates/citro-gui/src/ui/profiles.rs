//! The Profiles page: toolbar, filters and the sortable, virtualized profile table.

use super::app::{Busy, CitroApp, ToastKind};
use super::dialogs::{MoveDialog, RemarkDialog};
use super::theme::Palette;
use super::widgets::{self, dim, small_dim};
use crate::backend::{Query, Row, RunState, SortKey, StatusFilter, group_label, os_label, proxy_parts};
use crate::selection::{Mods, Selection};
use crate::{countries, query, timefmt};
use eframe::egui::{
    self, Align, Button, Color32, ComboBox, CornerRadius, Key, Layout, Modifiers, RichText, Sense, TextEdit, TextStyle,
    Ui, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};

const ROW_H: f32 = 33.0;

#[derive(Default)]
pub struct State {
    pub query: Query,
    pub selection: Selection,
    pub focus_search: bool,
    view: Vec<usize>,
    view_key: Option<(u64, u64, Query)>,
}

/// What the user did this frame; applied after the table has been drawn (the table borrows the app).
enum Act {
    Click(usize, Mods),
    Context(usize),
    Open(Vec<String>),
    Close(Vec<String>),
    Edit(String),
    Duplicate(String),
    ClearCache(Vec<String>),
    Move(Vec<String>),
    Remark(Vec<String>),
    CopyId(String),
    Delete(Vec<String>),
    Share(Vec<String>),
    Sort(SortKey),
    ToggleCheck(String),
    ToggleAll,
}

/// The status column's view of a row: the backend's run state plus what the UI itself is doing to it.
enum St {
    Open { pid: u32, since: i64 },
    Closed,
    Busy(&'static str),
    Locked { user: String, device: String, since: i64 },
    Failed(String),
}

fn status_of(r: &Row, busy: Option<Busy>, failure: Option<&String>) -> St {
    match (busy, &r.run) {
        (Some(Busy::Opening), _) | (_, RunState::Opening) => St::Busy("Opening…"),
        (Some(Busy::Closing), _) | (_, RunState::Closing) => St::Busy("Closing…"),
        (Some(Busy::Working), _) => St::Busy("Working…"),
        (_, RunState::LockedElsewhere { user, device, since }) => {
            St::Locked { user: user.clone(), device: device.clone(), since: *since }
        }
        (_, RunState::Open { pid, since }) => St::Open { pid: *pid, since: *since },
        (_, RunState::Closed) => match failure {
            Some(e) => St::Failed(e.clone()),
            None => St::Closed,
        },
    }
}

fn avatar(ui: &mut Ui, p: &Palette, id: &str, name: &str) {
    const HUES: [u32; 8] = [0x4f8cff, 0x2fbf9b, 0xe0a030, 0xd9648a, 0x8d7cf0, 0x4fb0d8, 0xe07a4f, 0x7ab648];
    let h = id.bytes().fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    let c = HUES[h % HUES.len()];
    let col = Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    let (rect, _) = ui.allocate_exact_size(vec2(19.0, 19.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 9.5, col.gamma_multiply(if p.dark { 0.30 } else { 0.22 }));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        crate::util::initial(name),
        egui::FontId::proportional(11.0),
        col,
    );
}

fn head(header: &mut TableRow<'_, '_>, p: &Palette, q: &Query, label: &str, key: Option<SortKey>, acts: &mut Vec<Act>) {
    header.col(|ui| {
        let active = key.is_some_and(|k| k == q.sort);
        let arrow = if !active {
            ""
        } else if q.desc {
            " ▼"
        } else {
            " ▲"
        };
        let text =
            RichText::new(format!("{label}{arrow}")).size(12.5).strong().color(if active { p.text } else { p.dim });
        match key {
            Some(k) => {
                let r = ui.add(egui::Label::new(text).sense(Sense::click()));
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click to sort").clicked() {
                    acts.push(Act::Sort(k));
                }
            }
            None => {
                ui.label(text);
            }
        }
    });
}

impl CitroApp {
    pub fn selected_ids(&self) -> Vec<String> {
        let sel = &self.profiles.selection;
        self.profiles
            .view
            .iter()
            .map(|&i| &self.rows[i])
            .filter(|r| sel.contains(r.id()))
            .map(|r| r.id().to_owned())
            .collect()
    }

    fn visible_ids(&self) -> Vec<String> {
        self.profiles.view.iter().map(|&i| self.rows[i].id().to_owned()).collect()
    }

    pub(super) fn refresh_view(&mut self) {
        let key = (self.rows_version, self.groups_version);
        let fresh =
            self.profiles.view_key.as_ref().is_some_and(|(v, g, q)| (*v, *g) == key && *q == self.profiles.query);
        if fresh {
            return;
        }
        self.profiles.view = query::view_indices(&self.rows, &self.profiles.query, &self.groups);
        self.profiles.view_key = Some((key.0, key.1, self.profiles.query.clone()));
        let visible = self.visible_ids();
        self.profiles.selection.retain_visible(&visible);
    }

    pub fn profile_shortcuts(&mut self, ctx: &egui::Context) {
        let (all, delete, enter, esc, up, down, shift_up, shift_down) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::A),
                i.consume_key(Modifiers::NONE, Key::Delete),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::SHIFT, Key::ArrowUp),
                i.consume_key(Modifiers::SHIFT, Key::ArrowDown),
            )
        });
        let visible = self.visible_ids();
        if all {
            self.profiles.selection.select_all(&visible);
        }
        if esc {
            self.profiles.selection.clear();
        }
        for (hit, delta, extend) in [(up, -1, false), (down, 1, false), (shift_up, -1, true), (shift_down, 1, true)] {
            if hit {
                self.profiles.selection.move_cursor(&visible, delta, extend);
            }
        }
        if delete {
            let ids = self.selected_ids();
            self.confirm_delete_profiles(ids);
        }
        if enter {
            let ids = self.selected_ids();
            self.open_profiles(ids);
        }
    }

    pub fn profiles_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        self.refresh_view();
        self.filter_bar(ui);
        self.action_bar(ui);
        ui.add_space(4.0);
        if !self.rows_loaded {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.add(egui::Spinner::new().size(28.0));
                ui.add_space(8.0);
                ui.label(dim(&p, "Loading profiles…"));
            });
        } else if let (Some(e), true) = (&self.rows_error, self.rows.is_empty()) {
            let e = e.clone();
            widgets::empty_state(ui, &p, "⚠", "Could not read the profile list", &e);
            ui.vertical_centered(|ui| {
                if widgets::button(ui, "Try again", true).clicked() {
                    self.refresh_rows();
                }
            });
        } else if self.rows.is_empty() {
            widgets::empty_state(
                ui,
                &p,
                "👤",
                "No profiles yet",
                "A profile is one browser identity: its own cookies, fingerprint and proxy. Create the first one.",
            );
            ui.vertical_centered(|ui| {
                if widgets::primary_button(ui, &p, "+ New profile", true).clicked() {
                    self.open_edit(None);
                }
                ui.add_space(6.0);
                ui.label(small_dim(
                    &p,
                    "Or from a terminal:  citro new my-profile --proxy socks5://user:pass@host:port",
                ));
            });
        } else if self.profiles.view.is_empty() {
            widgets::empty_state(
                ui,
                &p,
                "🔍",
                "No profile matches these filters",
                "Try another search, group or status.",
            );
            ui.vertical_centered(|ui| {
                if widgets::button(ui, "Clear filters", true).clicked() {
                    let q = &mut self.profiles.query;
                    q.q.clear();
                    q.group = None;
                    q.status = StatusFilter::All;
                }
            });
        } else {
            self.table(ui);
        }
    }

    fn filter_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Profiles").size(22.0).strong());
            let (shown, total) = (self.profiles.view.len(), self.rows.len());
            let count = if shown == total { format!("{total}") } else { format!("{shown} of {total}") };
            ui.label(dim(&p, count));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let q = &mut self.profiles.query;
                ComboBox::from_id_salt("status-filter")
                    .width(96.0)
                    .selected_text(format!("Status: {}", q.status.label()))
                    .show_ui(ui, |ui| {
                        for s in StatusFilter::ALL {
                            ui.selectable_value(&mut q.status, s, s.label());
                        }
                    });
                let group_text = match q.group.as_deref() {
                    None => "All groups".to_owned(),
                    Some(id) => group_label(&self.groups, id),
                };
                ComboBox::from_id_salt("group-filter").width(130.0).selected_text(group_text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut q.group, None, "All groups");
                    ui.selectable_value(&mut q.group, Some("0".to_owned()), "Ungrouped");
                    for g in &self.groups {
                        ui.selectable_value(&mut q.group, Some(g.group_id.clone()), &g.group_name);
                    }
                });
                if !q.q.is_empty() && ui.add(Button::new("✕").frame(false)).on_hover_text("Clear search").clicked() {
                    q.q.clear();
                }
                let search = ui.add(
                    TextEdit::singleline(&mut q.q)
                        .hint_text("Search name, remark, proxy…")
                        .desired_width(270.0)
                        .margin(vec2(8.0, 5.0)),
                );
                if std::mem::take(&mut self.profiles.focus_search) {
                    search.request_focus();
                }
                if search.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    q.q.clear();
                    search.surrender_focus();
                }
            });
        });
    }

    fn action_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let sel = self.selected_ids();
        let rows: Vec<&Row> = sel.iter().filter_map(|id| self.row(id)).collect();
        let free = |id: &str| !self.busy.contains_key(id);
        let can_open = rows.iter().any(|r| matches!(r.run, RunState::Closed) && free(r.id()));
        let can_close = rows.iter().any(|r| r.run.is_open() && free(r.id()));
        let any = !sel.is_empty();
        let hub_on = self.hub.connected;
        let mut acts = Vec::new();
        let mut new = false;
        let mut export = false;
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            new |= widgets::primary_button(ui, &p, "+ New profile", true).clicked();
            ui.separator();
            if widgets::button(ui, "▶ Open", can_open).on_hover_text("Open the selected profiles (Enter)").clicked() {
                acts.push(Act::Open(sel.clone()));
            }
            if widgets::button(ui, "■ Close", can_close).on_hover_text("Close the selected browsers").clicked() {
                acts.push(Act::Close(sel.clone()));
            }
            ui.separator();
            if widgets::button(ui, "Move to group", any).clicked() {
                acts.push(Act::Move(sel.clone()));
            }
            if widgets::button(ui, "Set remark", any).clicked() {
                acts.push(Act::Remark(sel.clone()));
            }
            if hub_on
                && widgets::button(ui, "Share with team", any)
                    .on_hover_text("Put the selected profiles on the team hub: everybody in the team can open them")
                    .clicked()
            {
                acts.push(Act::Share(sel.clone()));
            }
            export |=
                widgets::button(ui, "Export CSV", true).on_hover_text("Write the profile list to a CSV file").clicked();
            if widgets::danger_button(ui, &p, "Delete", any)
                .on_hover_text("Delete the selected profiles (Del)")
                .clicked()
            {
                acts.push(Act::Delete(sel.clone()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh |= widgets::button(ui, "Refresh", true).on_hover_text("Reload everything (F5)").clicked();
                if any {
                    ui.label(small_dim(&p, format!("{} selected", sel.len())));
                }
            });
        });
        if new {
            self.open_edit(None);
        }
        if export {
            self.open_export();
        }
        if refresh {
            self.refresh_all();
        }
        for a in acts {
            self.apply(a);
        }
    }

    fn apply(&mut self, act: Act) {
        match act {
            Act::Click(idx, mods) => {
                let visible = self.visible_ids();
                self.profiles.selection.click(&visible, idx, mods);
            }
            Act::Context(idx) => {
                let visible = self.visible_ids();
                self.profiles.selection.click(&visible, idx, Mods::default());
            }
            Act::ToggleCheck(id) => self.profiles.selection.toggle(&id),
            Act::ToggleAll => {
                let visible = self.visible_ids();
                self.profiles.selection.toggle_all(&visible);
            }
            Act::Open(ids) => self.open_profiles(ids),
            Act::Close(ids) => self.close_profiles(ids),
            Act::Edit(id) => self.open_edit(Some(id)),
            Act::Duplicate(id) => {
                let name = self.name_of(&id);
                let id2 = id.clone();
                self.run_op(vec![id], format!("Duplicated {name}"), super::tasks::Refresh::ROWS, move |b| {
                    b.duplicate_profile(&id2).map(|_| ())
                });
            }
            Act::ClearCache(ids) => self.confirm_clear_cache(ids),
            Act::Move(ids) => self.move_dlg = Some(MoveDialog::new(ids)),
            Act::Remark(ids) => {
                let text = if ids.len() == 1 {
                    self.row(&ids[0]).map(|r| r.profile.remark.clone()).unwrap_or_default()
                } else {
                    String::new()
                };
                self.remark_dlg = Some(RemarkDialog { ids, text });
            }
            Act::CopyId(id) => {
                self.ctx.copy_text(id.clone());
                self.toast(ToastKind::Info, format!("Copied {id}"));
            }
            Act::Delete(ids) => self.confirm_delete_profiles(ids),
            Act::Share(ids) => self.confirm_share(ids),
            Act::Sort(key) => {
                let q = &mut self.profiles.query;
                if q.sort == key {
                    q.desc = !q.desc;
                } else {
                    q.sort = key;
                    q.desc = matches!(key, SortKey::LastOpened | SortKey::LastClosed);
                }
            }
        }
    }

    fn table(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let (now, offset) = (self.now, self.offset);
        let ctx = self.ctx.clone();
        let hub_on = self.hub.connected;
        let mut acts: Vec<Act> = Vec::new();
        {
            let CitroApp { rows, groups, busy, last_error, profiles, .. } = &mut *self;
            let State { view, selection, query: q, .. } = profiles;
            let view: &Vec<usize> = view;
            let all_selected = {
                let ids: Vec<String> = view.iter().map(|&i| rows[i].id().to_owned()).collect();
                selection.all_selected(&ids)
            };
            let some_selected = !selection.is_empty();
            egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if let Some(f) = ui.style_mut().text_styles.get_mut(&TextStyle::Body) {
                    f.size = 13.0;
                }
                TableBuilder::new(ui)
                    .id_salt("profiles-table")
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click())
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .min_scrolled_height(120.0)
                    .column(Column::exact(22.0))
                    .column(Column::exact(26.0))
                    .column(Column::initial(164.0).at_least(110.0).clip(true))
                    .column(Column::initial(84.0).at_least(60.0).clip(true))
                    .column(Column::remainder().at_least(80.0).clip(true))
                    .column(Column::initial(150.0).at_least(90.0).clip(true))
                    .column(Column::initial(98.0).at_least(70.0).clip(true))
                    .column(Column::initial(70.0).at_least(60.0).clip(true))
                    .column(Column::initial(70.0).at_least(60.0).clip(true))
                    .column(Column::initial(70.0).at_least(60.0).clip(true))
                    .column(Column::initial(112.0).at_least(80.0).clip(true))
                    .column(Column::exact(92.0))
                    .header(30.0, |mut header| {
                        header.col(|ui| {
                            let mut c = all_selected;
                            let resp = ui.checkbox(&mut c, "");
                            if resp
                                .on_hover_text(if all_selected { "Select none" } else { "Select all (Ctrl+A)" })
                                .changed()
                            {
                                acts.push(Act::ToggleAll);
                            }
                            let _ = some_selected;
                        });
                        head(&mut header, &p, q, "#", Some(SortKey::Serial), &mut acts);
                        head(&mut header, &p, q, "Name", Some(SortKey::Name), &mut acts);
                        head(&mut header, &p, q, "Group", Some(SortKey::Group), &mut acts);
                        head(&mut header, &p, q, "Remark", Some(SortKey::Remark), &mut acts);
                        head(&mut header, &p, q, "Proxy", Some(SortKey::Proxy), &mut acts);
                        head(&mut header, &p, q, "Country / IP", None, &mut acts);
                        head(&mut header, &p, q, "OS persona", Some(SortKey::Os), &mut acts);
                        head(&mut header, &p, q, "Last opened", Some(SortKey::LastOpened), &mut acts);
                        head(&mut header, &p, q, "Last closed", Some(SortKey::LastClosed), &mut acts);
                        head(&mut header, &p, q, "Status", Some(SortKey::Status), &mut acts);
                        head(&mut header, &p, q, "", None, &mut acts);
                    })
                    .body(|body| {
                        body.rows(ROW_H, view.len(), |mut row| {
                            let vi = row.index();
                            let r = &rows[view[vi]];
                            let id = r.id().to_owned();
                            let pr = &r.profile;
                            let selected = selection.contains(&id);
                            let st = status_of(r, busy.get(&id).copied(), last_error.get(&id));
                            let working = matches!(st, St::Busy(_));
                            row.set_selected(selected);

                            cell(&mut row, |ui| {
                                let mut c = selected;
                                if ui.checkbox(&mut c, "").changed() {
                                    acts.push(Act::ToggleCheck(id.clone()));
                                }
                            });
                            cell(&mut row, |ui| {
                                ui.label(small_dim(&p, pr.serial_number.to_string()));
                            });
                            cell(&mut row, |ui| {
                                avatar(ui, &p, &id, &pr.name);
                                ui.add(egui::Label::new(RichText::new(&pr.name).strong()).truncate());
                                if r.team.is_some() {
                                    widgets::badge(ui, "Team", p.accent)
                                        .on_hover_text("Shared through the team hub: it opens on one device at a time");
                                }
                            });
                            cell(&mut row, |ui| {
                                let label = group_label(groups, &pr.group_id);
                                let t = if label == "Ungrouped" { dim(&p, label) } else { RichText::new(label) };
                                ui.add(egui::Label::new(t).truncate());
                            });
                            cell(&mut row, |ui| {
                                let line = crate::util::first_line(&pr.remark);
                                ui.add(egui::Label::new(dim(&p, line)).truncate()).on_hover_text(&pr.remark);
                            });
                            cell(&mut row, |ui| match proxy_parts(&pr.user_proxy_config) {
                                Some((scheme, addr)) => {
                                    let hop = pr.user_proxy_config.hop();
                                    let user = hop.as_ref().map(|h| h.account.clone()).unwrap_or_default();
                                    let tip = if user.is_empty() {
                                        format!("{scheme}://{addr}")
                                    } else {
                                        format!("{scheme}://{user}:***@{addr}")
                                    };
                                    ui.label(RichText::new(scheme.to_uppercase()).size(10.5).strong().color(p.accent));
                                    ui.add(egui::Label::new(RichText::new(addr).size(12.5)).truncate())
                                        .on_hover_text(tip);
                                }
                                None => {
                                    ui.label(dim(&p, "Direct"));
                                }
                            });
                            cell(&mut row, |ui| {
                                let cc = pr.country.trim();
                                let name = countries::name(cc).unwrap_or("");
                                if cc.is_empty() && pr.ip.is_empty() {
                                    ui.label(dim(&p, "—"));
                                } else {
                                    if !cc.is_empty() {
                                        ui.label(RichText::new(cc.to_uppercase()).strong()).on_hover_text(name);
                                    }
                                    if !pr.ip.is_empty() {
                                        ui.add(egui::Label::new(small_dim(&p, &pr.ip)).truncate());
                                    }
                                }
                            });
                            cell(&mut row, |ui| {
                                let os = os_label(&pr.persona_os);
                                ui.label(if os == "Auto" { dim(&p, os) } else { RichText::new(os) });
                            });
                            for ts in [pr.last_open_time, pr.last_close_time] {
                                cell(&mut row, |ui| {
                                    let t = timefmt::relative(now, ts, offset);
                                    let t = if ts <= 0 { dim(&p, "never") } else { RichText::new(t) };
                                    ui.label(t).on_hover_text(timefmt::absolute(ts, offset));
                                });
                            }
                            cell(&mut row, |ui| status_cell(ui, &p, &st, now, offset));
                            cell(&mut row, |ui| {
                                ui.spacing_mut().button_padding = vec2(7.0, 2.0);
                                ui.spacing_mut().item_spacing.x = 5.0;
                                let locked = matches!(st, St::Locked { .. });
                                if matches!(st, St::Open { .. }) {
                                    if ui.add_enabled(!working, small_button("Close")).clicked() {
                                        acts.push(Act::Close(vec![id.clone()]));
                                    }
                                } else {
                                    let b = ui.add_enabled(!working && !locked, small_button("Open"));
                                    if b.clicked() {
                                        acts.push(Act::Open(vec![id.clone()]));
                                    }
                                }
                                if ui.add_enabled(!working, small_button("Edit")).clicked() {
                                    acts.push(Act::Edit(id.clone()));
                                }
                            });

                            let resp = row.response();
                            if resp.secondary_clicked() && !selected {
                                acts.push(Act::Context(vi));
                            }
                            if resp.clicked() {
                                let mods = ctx.input(|i| Mods { ctrl: i.modifiers.command, shift: i.modifiers.shift });
                                acts.push(Act::Click(vi, mods));
                            }
                            if resp.double_clicked() && !working {
                                acts.push(Act::Open(vec![id.clone()]));
                            }
                            resp.context_menu(|ui| {
                                let targets: Vec<String> = if selected {
                                    let ids: Vec<String> = view.iter().map(|&i| rows[i].id().to_owned()).collect();
                                    selection.ordered(&ids)
                                } else {
                                    vec![id.clone()]
                                };
                                let n = targets.len();
                                let many = |s: &str| if n > 1 { format!("{s} ({n})") } else { s.to_owned() };
                                let is_open = matches!(st, St::Open { .. });
                                if ui
                                    .add_enabled(
                                        !is_open && !working && !matches!(st, St::Locked { .. }),
                                        Button::new(many("Open")),
                                    )
                                    .clicked()
                                {
                                    acts.push(Act::Open(targets.clone()));
                                    ui.close();
                                }
                                if ui.add_enabled(is_open && !working, Button::new(many("Close"))).clicked() {
                                    acts.push(Act::Close(targets.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.add_enabled(n == 1, Button::new("Edit…")).clicked() {
                                    acts.push(Act::Edit(id.clone()));
                                    ui.close();
                                }
                                if ui.add_enabled(n == 1, Button::new("Duplicate")).clicked() {
                                    acts.push(Act::Duplicate(id.clone()));
                                    ui.close();
                                }
                                if ui.button(many("Set remark…")).clicked() {
                                    acts.push(Act::Remark(targets.clone()));
                                    ui.close();
                                }
                                if ui.button(many("Move to group…")).clicked() {
                                    acts.push(Act::Move(targets.clone()));
                                    ui.close();
                                }
                                if ui.button(many("Clear cache…")).clicked() {
                                    acts.push(Act::ClearCache(targets.clone()));
                                    ui.close();
                                }
                                if hub_on && ui.button(many("Share with team…")).clicked() {
                                    acts.push(Act::Share(targets.clone()));
                                    ui.close();
                                }
                                if ui.button("Copy profile id").clicked() {
                                    acts.push(Act::CopyId(id.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.add(Button::new(RichText::new(many("Delete…")).color(p.err))).clicked() {
                                    acts.push(Act::Delete(targets));
                                    ui.close();
                                }
                            });
                        });
                    });
            });
        }
        for a in acts {
            self.apply(a);
        }
    }
}

/// A table cell whose text keeps the normal colour even in a selected row.
fn cell(row: &mut TableRow<'_, '_>, add: impl FnOnce(&mut Ui)) {
    row.col(|ui| {
        ui.visuals_mut().override_text_color = None;
        add(ui);
    });
}

fn small_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).size(12.5)).min_size(vec2(0.0, 22.0)).corner_radius(CornerRadius::same(5))
}

fn status_cell(ui: &mut Ui, p: &Palette, st: &St, now: i64, offset: i32) {
    match st {
        St::Open { pid, since } => {
            widgets::dot(ui, p.ok, true);
            ui.label(RichText::new("Open").color(p.ok))
                .on_hover_text(format!("Open since {} (pid {pid})", timefmt::since(now, *since, offset)));
        }
        St::Closed => {
            widgets::dot(ui, p.idle, false);
            ui.label(dim(p, "Closed"));
        }
        St::Busy(text) => {
            ui.add(egui::Spinner::new().size(12.0).color(p.warn));
            ui.label(RichText::new(*text).color(p.warn));
        }
        St::Locked { user, device, since } => {
            widgets::dot(ui, p.locked, true);
            ui.add(egui::Label::new(RichText::new(format!("Locked · {user}")).color(p.locked)).truncate())
                .on_hover_text(format!("Open on {device} by {user} since {}", timefmt::since(now, *since, offset)));
        }
        St::Failed(e) => {
            widgets::dot(ui, p.err, true);
            ui.add(egui::Label::new(RichText::new("Open failed").color(p.err)).truncate()).on_hover_text(e);
        }
    }
}
