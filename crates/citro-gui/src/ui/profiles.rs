//! The Profiles page: tabs, filters, toolbar and the sortable, virtualized profile table.

use super::app::{Busy, CitroApp, Page, ToastKind};
use super::dialogs::{MoveDialog, RemarkDialog};
use super::theme::{self, Palette};
use super::widgets::{self, Icon, Pill, dim, small_dim};
use crate::backend::{Query, Row, RunState, SortKey, StatusFilter, group_label};
use crate::selection::{Mods, Selection};
use crate::ui::tasks::Refresh;
use crate::{countries, query, timefmt};
use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CursorIcon, FontFamily, FontId, Key, Layout, Modifiers, Popup,
    Response, RichText, Sense, TextStyle, Ui, pos2, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};

const ROW_H: f32 = 56.0;
const HEAD_H: f32 = 40.0;
/// Room kept under the table for the "Total" line.
const FOOTER_H: f32 = 34.0;

#[derive(Default)]
pub struct State {
    pub query: Query,
    pub selection: Selection,
    pub focus_search: bool,
    view: Vec<usize>,
    view_key: Option<(u64, u64, Query)>,
    /// A name or remark being edited in the list.
    inline: Option<Inline>,
    /// The Tags window is open for these profiles.
    pub tags_for: Option<Vec<String>>,
    pub new_tag: String,
    pub new_tag_color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Remark,
}

struct Inline {
    id: String,
    field: Field,
    text: String,
    focused: bool,
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
    SetGroup(Vec<String>, String),
    StartEdit(String, Field, String),
    CommitEdit,
    CancelEdit,
    Retag(Vec<String>, Vec<String>, Vec<String>),
    Tags(Vec<String>),
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

fn tag_color(key: &str) -> Color32 {
    let c = citro_core::profile::tag_rgb(key);
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// The text field of an inline edit, when `id`'s `field` is being edited. Enter or leaving the field saves, Esc
/// cancels. Returns `false` when this cell is not being edited.
fn inline_edit(ui: &mut Ui, inline: &mut Option<Inline>, id: &str, field: Field, acts: &mut Vec<Act>) -> bool {
    let Some(e) = inline.as_mut().filter(|e| e.id == id && e.field == field) else { return false };
    let r = ui.add(
        egui::TextEdit::singleline(&mut e.text).desired_width(f32::INFINITY).char_limit(if field == Field::Name {
            100
        } else {
            2000
        }),
    );
    if !e.focused {
        r.request_focus();
        e.focused = true;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        acts.push(Act::CancelEdit);
    } else if r.lost_focus() {
        acts.push(Act::CommitEdit);
    }
    true
}

/// Text on up to two lines, then cut with an ellipsis (the antidetect browser's name and remark cells).
fn two_lines(ui: &mut Ui, text: RichText) -> Response {
    let w = ui.available_width().max(40.0);
    let style = ui.style().clone();
    let mut job = egui::text::LayoutJob::default();
    text.append_to(&mut job, &style, egui::FontSelection::Default, Align::LEFT);
    job.wrap = egui::text::TextWrapping {
        max_width: w,
        max_rows: 2,
        break_anywhere: false,
        overflow_character: Some('\u{2026}'),
    };
    ui.add(egui::Label::new(job).wrap().sense(Sense::hover()))
}

const OS_MACOS: &[u8] = include_bytes!("../../assets/os/macos.png");
const OS_WINDOWS: &[u8] = include_bytes!("../../assets/os/windows.png");
const OS_LINUX: &[u8] = include_bytes!("../../assets/os/linux.png");

/// The mark of the OS the profile presents (Simple Icons, CC0), in the accent blue like other antidetect browsers'; "Auto" is a
/// ring with an A.
fn os_mark(ui: &mut Ui, p: &Palette, os: &str) {
    let tint = p.accent_hover;
    let resp = match os {
        "windows" => widgets::tinted_png(ui, "os-windows", OS_WINDOWS, 15.0, tint),
        "macos" => widgets::tinted_png(ui, "os-macos", OS_MACOS, 16.0, tint),
        "linux" => widgets::tinted_png(ui, "os-linux", OS_LINUX, 18.0, tint),
        _ => {
            let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            let c = rect.center();
            ui.painter().circle_stroke(c, 7.0, egui::Stroke::new(1.3, p.dim));
            ui.painter().text(c, Align2::CENTER_CENTER, "A", FontId::new(9.0, theme::semibold_family()), p.dim);
            resp
        }
    };
    resp.on_hover_text(match os {
        "windows" => "Windows persona",
        "macos" => "macOS persona",
        "linux" => "Linux persona",
        _ => "Auto: this computer's OS",
    });
}

#[allow(dead_code)]
fn avatar(ui: &mut Ui, p: &Palette, id: &str, name: &str) {
    const HUES: [u32; 8] = [0x4f8cff, 0x2fbf9b, 0xe0a030, 0xd9648a, 0x8d7cf0, 0x4fb0d8, 0xe07a4f, 0x7ab648];
    let h = id.bytes().fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    let c = HUES[h % HUES.len()];
    let col = Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 13.0, col.gamma_multiply(if p.dark { 0.30 } else { 0.22 }));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        crate::util::initial(name),
        FontId::new(12.5, theme::semibold_family()),
        col,
    );
}

fn head(header: &mut TableRow<'_, '_>, p: &Palette, q: &Query, label: &str, key: Option<SortKey>, acts: &mut Vec<Act>) {
    header.col(|ui| {
        let active = key.is_some_and(|k| k == q.sort);
        let col = if active { p.strong } else { p.dim };
        let font = FontId::new(12.5, if active { theme::semibold_family() } else { FontFamily::Proportional });
        let tw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), col).size().x;
        let arrow = if key.is_some() { 16.0 } else { 0.0 };
        let sense = if key.is_some() { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(tw + arrow, 22.0), sense);
        ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, label, font, col);
        if key.is_some() {
            let (icon, tint) = match (active, q.desc) {
                (true, true) => (Icon::SortDown, p.accent_hover),
                (true, false) => (Icon::SortUp, p.accent_hover),
                _ => (Icon::Sort, p.dim.gamma_multiply(0.7)),
            };
            widgets::paint_icon_scaled(ui.painter(), pos2(rect.right() - 6.0, rect.center().y), icon, tint, 0.62);
        }
        if let Some(k) = key
            && resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Click to sort").clicked()
        {
            acts.push(Act::Sort(k));
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
        let mut export = false;
        let tab = widgets::tab_header(ui, &p, &["Profiles", "Groups"], 0, |ui| {
            export = widgets::icon_text_button(
                ui,
                &p,
                Icon::Export,
                "Export CSV",
                "Write the profile list to a CSV file",
                true,
            )
            .clicked();
        });
        if tab == Some(1) {
            self.go(Page::Groups);
        }
        if export {
            self.open_export();
        }
        self.filter_bar(ui);
        ui.add_space(10.0);
        self.action_bar(ui);
        ui.add_space(10.0);
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

    /// Group dropdown, the wide search box and the status dropdown.
    fn filter_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let q = &mut self.profiles.query;
            let group_text = match q.group.as_deref() {
                None => "All groups".to_owned(),
                Some(id) => group_label(&self.groups, id),
            };
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 40.0;
                ui.spacing_mut().button_padding = vec2(14.0, 9.0);
                let group_box = ComboBox::from_id_salt("group-filter").width(150.0).icon(widgets::chevron_icon);
                group_box.selected_text(group_text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut q.group, None, "All groups");
                    ui.selectable_value(&mut q.group, Some("0".to_owned()), "Ungrouped");
                    for g in &self.groups {
                        ui.selectable_value(&mut q.group, Some(g.group_id.clone()), &g.group_name);
                    }
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.scope(|ui| {
                    ui.spacing_mut().interact_size.y = 40.0;
                    ui.spacing_mut().button_padding = vec2(14.0, 9.0);
                    ComboBox::from_id_salt("status-filter")
                        .width(104.0)
                        .icon(widgets::chevron_icon)
                        .selected_text(format!("Status: {}", q.status.label()))
                        .show_ui(ui, |ui| {
                            for s in StatusFilter::ALL {
                                ui.selectable_value(&mut q.status, s, s.label());
                            }
                        });
                });
                let width = ui.available_width();
                let search = widgets::search_field(ui, &p, &mut q.q, "Search name, remark, proxy…", width);
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

    /// The row of square buttons under the search box. Everything acts on the selected profiles.
    fn action_bar(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let sel = self.selected_ids();
        let rows: Vec<&Row> = sel.iter().filter_map(|id| self.row(id)).collect();
        let free = |id: &str| !self.busy.contains_key(id);
        let can_open = rows.iter().any(|r| matches!(r.run, RunState::Closed) && free(r.id()));
        let can_close = rows.iter().any(|r| r.run.is_open() && free(r.id()));
        let any = !sel.is_empty();
        let one = sel.len() == 1;
        let hub_on = self.hub.connected;
        let mut acts = Vec::new();
        let mut refresh = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if widgets::open_button(ui, &p, Icon::Play, "Open", can_open)
                .on_hover_text("Open the selected profiles (Enter)")
                .clicked()
            {
                acts.push(Act::Open(sel.clone()));
            }
            if widgets::icon_button(ui, &p, Icon::Stop, "Close the selected browsers", can_close, false).clicked() {
                acts.push(Act::Close(sel.clone()));
            }
            if widgets::icon_button(ui, &p, Icon::Edit, "Edit the selected profile…", one, false).clicked() {
                acts.push(Act::Edit(sel[0].clone()));
            }
            if widgets::icon_button(ui, &p, Icon::MoveTo, "Move to group…", any, false).clicked() {
                acts.push(Act::Move(sel.clone()));
            }
            if widgets::icon_button(ui, &p, Icon::Tag, "Tags…", any, false).clicked() {
                acts.push(Act::Tags(sel.clone()));
            }
            if hub_on
                && widgets::icon_button(
                    ui,
                    &p,
                    Icon::Share,
                    "Share with team…\nEverybody in the team can open the selected profiles",
                    any,
                    false,
                )
                .clicked()
            {
                acts.push(Act::Share(sel.clone()));
            }
            if widgets::icon_button(ui, &p, Icon::Trash, "Delete the selected profiles (Del)", any, true).clicked() {
                acts.push(Act::Delete(sel.clone()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh |= widgets::icon_button(ui, &p, Icon::Refresh, "Reload everything (F5)", true, false).clicked();
                if any {
                    ui.label(RichText::new(format!("{} selected", sel.len())).size(13.0).color(p.accent_hover));
                }
            });
        });
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
            Act::SetGroup(ids, gid) => {
                let name = group_label(&self.groups, &gid);
                for r in self.rows.iter_mut().filter(|r| ids.iter().any(|i| i == r.id())) {
                    r.profile.group_id = gid.clone();
                }
                let n = ids.len();
                let what = if n == 1 { self.name_of(&ids[0]) } else { format!("{n} profiles") };
                self.run_op(Vec::new(), format!("Moved {what} to {name}"), Refresh::ROWS, move |b| {
                    b.move_to_group(&ids, &gid)
                });
            }
            Act::StartEdit(id, field, text) => {
                self.profiles.inline = Some(Inline { id, field, text, focused: false });
            }
            Act::CancelEdit => self.profiles.inline = None,
            Act::CommitEdit => {
                let Some(e) = self.profiles.inline.take() else { return };
                let Some(row) = self.row(&e.id) else { return };
                let (id, text) = (e.id.clone(), e.text.trim().to_owned());
                match e.field {
                    Field::Name => {
                        if text.is_empty() || text == row.profile.name {
                            return;
                        }
                        for r in self.rows.iter_mut().filter(|r| r.id() == id) {
                            r.profile.name = text.clone();
                        }
                        self.run_op(Vec::new(), String::new(), Refresh::ROWS, move |b| {
                            let mut p = b.profile(&id)?;
                            p.name = text;
                            b.save_profile(p).map(|_| ())
                        });
                    }
                    Field::Remark => {
                        if text == row.profile.remark.trim() {
                            return;
                        }
                        for r in self.rows.iter_mut().filter(|r| r.id() == id) {
                            r.profile.remark = text.clone();
                        }
                        self.run_op(Vec::new(), String::new(), Refresh::ROWS, move |b| b.set_remark(&id, &text));
                    }
                }
            }
            Act::Retag(ids, add, remove) => {
                for r in self.rows.iter_mut().filter(|r| ids.iter().any(|i| i == r.id())) {
                    r.profile.tags.retain(|t| !remove.contains(t));
                    for a in &add {
                        if !r.profile.tags.contains(a) {
                            r.profile.tags.push(a.clone());
                        }
                    }
                }
                self.run_op(Vec::new(), String::new(), Refresh::ROWS, move |b| b.retag(&ids, &add, &remove));
            }
            Act::Tags(ids) => self.profiles.tags_for = Some(ids),
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
                    q.desc = matches!(key, SortKey::LastOpened | SortKey::LastClosed | SortKey::Created);
                }
            }
        }
    }

    fn table(&mut self, ui: &mut Ui) {
        let p = self.pal;
        let (now, offset) = (self.now, self.offset);
        let ctx = self.ctx.clone();
        let hub_on = self.hub.connected;
        let (total, shown) = (self.rows.len(), self.profiles.view.len());
        let selected_n = self.profiles.selection.len();
        let mut acts: Vec<Act> = Vec::new();
        {
            let CitroApp { rows, groups, tags, busy, last_error, profiles, .. } = &mut *self;
            let State { view, selection, query: q, inline, .. } = profiles;
            let view: &Vec<usize> = view;
            let all_selected = {
                let ids: Vec<String> = view.iter().map(|&i| rows[i].id().to_owned()).collect();
                selection.all_selected(&ids)
            };
            let some_selected = !selection.is_empty();
            let body_h = (ui.available_height() - FOOTER_H).max(140.0);
            ui.allocate_ui(vec2(ui.available_width(), body_h), |ui| {
                egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // the table reads a notch smaller than the rest of the app
                    if let Some(f) = ui.style_mut().text_styles.get_mut(&TextStyle::Body) {
                        f.size = 13.0;
                    }
                    TableBuilder::new(ui)
                        .id_salt("profiles-table")
                        .striped(true)
                        .resizable(false)
                        .sense(Sense::click())
                        .cell_layout(Layout::left_to_right(Align::Center))
                        .min_scrolled_height(120.0)
                        .column(Column::exact(28.0))
                        .column(Column::exact(42.0))
                        .column(Column::exact(72.0))
                        .column(Column::initial(150.0).at_least(110.0).clip(true))
                        .column(Column::initial(92.0).at_least(70.0).clip(true))
                        .column(Column::initial(118.0).at_least(90.0).clip(true))
                        .column(Column::initial(140.0).at_least(110.0).clip(true))
                        .column(Column::remainder().at_least(110.0).clip(true))
                        .column(Column::exact(80.0))
                        .column(Column::exact(96.0))
                        .column(Column::exact(112.0))
                        .header(HEAD_H, |mut header| {
                            header.col(|ui| {
                                let tip = if all_selected { "Select none" } else { "Select all (Ctrl+A)" };
                                let mixed = some_selected && !all_selected;
                                if widgets::checkbox(ui, &p, all_selected, mixed).on_hover_text(tip).clicked() {
                                    acts.push(Act::ToggleAll);
                                }
                            });
                            head(&mut header, &p, q, "No.", Some(SortKey::Serial), &mut acts);
                            head(&mut header, &p, q, "ID", None, &mut acts);
                            head(&mut header, &p, q, "Name", Some(SortKey::Name), &mut acts);
                            head(&mut header, &p, q, "Group", Some(SortKey::Group), &mut acts);
                            head(&mut header, &p, q, "Tags", None, &mut acts);
                            head(&mut header, &p, q, "IP", Some(SortKey::Proxy), &mut acts);
                            head(&mut header, &p, q, "Remark", Some(SortKey::Remark), &mut acts);
                            head(&mut header, &p, q, "Created", Some(SortKey::Created), &mut acts);
                            head(&mut header, &p, q, "Last opened", Some(SortKey::LastOpened), &mut acts);
                            head(&mut header, &p, q, "Operation", None, &mut acts);
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
                                    if widgets::checkbox(ui, &p, selected, false).clicked() {
                                        acts.push(Act::ToggleCheck(id.clone()));
                                    }
                                });
                                cell(&mut row, |ui| {
                                    ui.label(dim(&p, pr.serial_number.to_string()));
                                });
                                cell(&mut row, |ui| {
                                    ui.add(egui::Label::new(dim(&p, &id)).truncate());
                                });
                                // Name: the OS mark, then the name on up to two lines
                                cell(&mut row, |ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    os_mark(ui, &p, &pr.persona_os);
                                    if r.team.is_some() {
                                        widgets::team_mark(ui, p.accent_hover).on_hover_text(
                                            "Shared through the team hub: it opens on one device at a time",
                                        );
                                    }
                                    if !inline_edit(ui, inline, &id, Field::Name, &mut acts) {
                                        let r = two_lines(
                                            ui,
                                            RichText::new(&pr.name)
                                                .size(13.5)
                                                .family(theme::medium_family())
                                                .color(p.strong),
                                        )
                                        .on_hover_text(format!("{}\nDouble-click to rename", pr.name));
                                        if r.double_clicked() {
                                            acts.push(Act::StartEdit(id.clone(), Field::Name, pr.name.clone()));
                                        }
                                    }
                                });
                                // Group: click to move the profile (or the selection it is part of)
                                cell(&mut row, |ui| {
                                    let label = group_label(groups, &pr.group_id);
                                    let t = if label == "Ungrouped" {
                                        dim(&p, label)
                                    } else {
                                        RichText::new(label).color(p.strong)
                                    };
                                    let resp = ui
                                        .add(egui::Label::new(t).truncate().sense(Sense::click()))
                                        .on_hover_cursor(CursorIcon::PointingHand)
                                        .on_hover_text("Click to change the group");
                                    let targets = if selected {
                                        selection
                                            .ordered(&view.iter().map(|&i| rows[i].id().to_owned()).collect::<Vec<_>>())
                                    } else {
                                        vec![id.clone()]
                                    };
                                    let cur = if pr.group_id.is_empty() { "0".to_owned() } else { pr.group_id.clone() };
                                    Popup::menu(&resp).show(|ui| {
                                        ui.set_min_width(170.0);
                                        if targets.len() > 1 {
                                            ui.label(small_dim(&p, format!("Move {} profiles to", targets.len())));
                                        }
                                        let mut opts: Vec<(String, String)> = vec![("0".into(), "Ungrouped".into())];
                                        opts.extend(groups.iter().map(|g| (g.group_id.clone(), g.group_name.clone())));
                                        for (gid, name) in opts {
                                            if ui.selectable_label(gid == cur, name).clicked() {
                                                acts.push(Act::SetGroup(targets.clone(), gid));
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                                // Tags: colored chips; click to tick tags
                                cell(&mut row, |ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    let mine: Vec<&crate::backend::Tag> =
                                        pr.tags.iter().filter_map(|t| tags.iter().find(|x| &x.id == t)).collect();
                                    let resp = ui
                                        .horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            if mine.is_empty() {
                                                ui.label(dim(&p, "+ Tag"));
                                            }
                                            // as many chips as fit, then "+N"
                                            let mut shown = 0;
                                            for t in &mine {
                                                let room = ui.available_width();
                                                let need = t.name.chars().count() as f32 * 6.6 + 18.0;
                                                if shown > 0 && need + 24.0 > room {
                                                    break;
                                                }
                                                widgets::badge(ui, &t.name, tag_color(&t.color));
                                                shown += 1;
                                            }
                                            if mine.len() > shown {
                                                ui.label(small_dim(&p, format!("+{}", mine.len() - shown)));
                                            }
                                        })
                                        .response
                                        .interact(Sense::click())
                                        .on_hover_cursor(CursorIcon::PointingHand)
                                        .on_hover_text(if mine.is_empty() {
                                            "Click to add tags".to_owned()
                                        } else {
                                            mine.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                                        });
                                    Popup::menu(&resp).show(|ui| {
                                        ui.set_min_width(190.0);
                                        for t in tags.iter() {
                                            let mut on = pr.tags.contains(&t.id);
                                            ui.horizontal(|ui| {
                                                if ui.checkbox(&mut on, "").changed() {
                                                    let (add, rem) = if on {
                                                        (vec![t.id.clone()], vec![])
                                                    } else {
                                                        (vec![], vec![t.id.clone()])
                                                    };
                                                    acts.push(Act::Retag(vec![id.clone()], add, rem));
                                                }
                                                widgets::badge(ui, &t.name, tag_color(&t.color));
                                            });
                                        }
                                        if !tags.is_empty() {
                                            ui.separator();
                                        }
                                        if ui.button("New tag / manage\u{2026}").clicked() {
                                            acts.push(Act::Tags(vec![id.clone()]));
                                            ui.close();
                                        }
                                    });
                                });
                                // IP: the exit address (the proxy's until a check or a launch knows better), the country
                                cell(&mut row, |ui| {
                                    let hop = pr.user_proxy_config.hop();
                                    let ip = if !pr.ip.trim().is_empty() {
                                        pr.ip.trim().to_owned()
                                    } else {
                                        hop.as_ref().map(|h| h.host.clone()).unwrap_or_default()
                                    };
                                    let cc = pr.country.trim().to_ascii_uppercase();
                                    let place = match (cc.is_empty(), countries::name(&cc.to_ascii_lowercase())) {
                                        (true, _) => String::new(),
                                        (false, Some(n)) => format!("{cc} - {n}"),
                                        (false, None) => cc.clone(),
                                    };
                                    let tip = match &hop {
                                        Some(h) if h.account.is_empty() => {
                                            format!("{}://{}:{}", h.scheme, h.host, h.port)
                                        }
                                        Some(h) => format!("{}://{}:***@{}:{}", h.scheme, h.account, h.host, h.port),
                                        None => "No proxy: the browser connects directly".to_owned(),
                                    };
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            let (tag, col) = match &hop {
                                                Some(h) => (h.scheme.to_uppercase(), p.accent_hover),
                                                None => ("DIRECT".to_owned(), p.dim),
                                            };
                                            ui.label(
                                                RichText::new(tag)
                                                    .size(9.5)
                                                    .family(theme::semibold_family())
                                                    .color(col),
                                            );
                                            let main = if ip.is_empty() {
                                                dim(&p, "\u{2013}")
                                            } else {
                                                RichText::new(&ip).color(p.strong)
                                            };
                                            ui.add(egui::Label::new(main).truncate()).on_hover_text(tip);
                                        });
                                        if !place.is_empty() {
                                            ui.add(egui::Label::new(small_dim(&p, place)).truncate());
                                        }
                                    });
                                });
                                cell(&mut row, |ui| {
                                    if inline_edit(ui, inline, &id, Field::Remark, &mut acts) {
                                        return;
                                    }
                                    let text = pr.remark.trim();
                                    let r = if text.is_empty() {
                                        ui.add(egui::Label::new(dim(&p, "\u{2013}")).sense(Sense::click()))
                                    } else {
                                        two_lines(ui, RichText::new(text).color(p.text))
                                    };
                                    let r = r.on_hover_text(if text.is_empty() {
                                        "Double-click to write a remark".to_owned()
                                    } else {
                                        format!("{text}\nDouble-click to edit")
                                    });
                                    if r.double_clicked() {
                                        acts.push(Act::StartEdit(id.clone(), Field::Remark, pr.remark.clone()));
                                    }
                                });
                                cell(&mut row, |ui| {
                                    if pr.created_time > 0 {
                                        let d = timefmt::DateTime::from_unix(pr.created_time, offset);
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            ui.label(
                                                RichText::new(format!("{:02}-{:02}", d.month, d.day)).color(p.text),
                                            );
                                            ui.label(small_dim(
                                                &p,
                                                format!("{:02}:{:02}:{:02}", d.hour, d.minute, d.second),
                                            ));
                                        })
                                        .response
                                        .on_hover_text(timefmt::absolute(pr.created_time, offset));
                                    } else {
                                        ui.label(dim(&p, "\u{2013}"));
                                    }
                                });
                                // last opened, or what is going on right now
                                cell(&mut row, |ui| match &st {
                                    St::Closed => {
                                        let ts = pr.last_open_time;
                                        let t = if ts <= 0 {
                                            dim(&p, "never")
                                        } else {
                                            RichText::new(timefmt::relative(now, ts, offset))
                                        };
                                        ui.label(t).on_hover_text(format!(
                                            "Opened {}\nClosed {}",
                                            timefmt::absolute(pr.last_open_time, offset),
                                            timefmt::absolute(pr.last_close_time, offset)
                                        ));
                                    }
                                    other => status_cell(ui, &p, other, now, offset),
                                });
                                let mut more: Option<Response> = None;
                                cell(&mut row, |ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    match &st {
                                        St::Open { .. } => {
                                            if widgets::pill_button(
                                                ui,
                                                &p,
                                                "Close",
                                                Some(Icon::Stop),
                                                Pill::Outline,
                                                !working,
                                            )
                                            .clicked()
                                            {
                                                acts.push(Act::Close(vec![id.clone()]));
                                            }
                                        }
                                        St::Locked { .. } => {
                                            widgets::pill_button(
                                                ui,
                                                &p,
                                                "Open",
                                                Some(Icon::Play),
                                                Pill::Primary,
                                                false,
                                            );
                                        }
                                        _ => {
                                            if widgets::pill_button(
                                                ui,
                                                &p,
                                                "Open",
                                                Some(Icon::Play),
                                                Pill::Primary,
                                                !working,
                                            )
                                            .clicked()
                                            {
                                                acts.push(Act::Open(vec![id.clone()]));
                                            }
                                        }
                                    }
                                    more = Some(widgets::more_button(ui, &p));
                                });

                                // the same menu from a right click on the row and from the three dots
                                let menu = |ui: &mut Ui, acts: &mut Vec<Act>| {
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
                                };

                                let resp = row.response();
                                if resp.secondary_clicked() && !selected {
                                    acts.push(Act::Context(vi));
                                }
                                if resp.clicked() {
                                    let mods =
                                        ctx.input(|i| Mods { ctrl: i.modifiers.command, shift: i.modifiers.shift });
                                    acts.push(Act::Click(vi, mods));
                                }
                                resp.context_menu(|ui| menu(ui, &mut acts));
                                if let Some(m) = &more {
                                    if m.clicked() && !selected {
                                        acts.push(Act::Context(vi));
                                    }
                                    Popup::menu(m).show(|ui| menu(ui, &mut acts));
                                }
                            });
                        });
                });
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(small_dim(&p, format!("Total: {total}")));
            if shown != total {
                ui.label(small_dim(&p, format!("·  {shown} match the filters")));
            }
            if selected_n > 0 {
                ui.label(RichText::new(format!("·  {selected_n} selected")).size(12.0).color(p.accent_hover));
            }
        });
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
