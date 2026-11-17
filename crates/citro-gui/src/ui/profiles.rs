//! The Profiles page: tabs, filters, toolbar and the sortable, virtualized profile table.

use super::app::{Busy, CitroApp, Page, ToastKind};
use super::dialogs::{MoveDialog, RemarkDialog};
use super::theme::{self, Palette};
use super::widgets::{self, Icon, Pill, dim, small_dim};
use crate::backend::{ProxyCheck, Query, Row, RunState, SortKey, StatusFilter, group_label};
use crate::selection::{Mods, Selection};
use crate::ui::tasks::Refresh;
use crate::{countries, query, timefmt};
use citro_core::profile::ProxyConfig;
use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CornerRadius, CursorIcon, FontFamily, FontId, Key, Layout,
    Modifiers, Popup, PopupCloseBehavior, Rect, Response, RichText, ScrollArea, Sense, Stroke, TextStyle, Ui,
    UiBuilder, pos2, vec2,
};

const ROW_H: f32 = 56.0;
/// The scrolling columns: No., ID, Name, Group, Tags, IP, Remark, Created, Last opened.
const COL_W: [f32; 9] = [52.0, 80.0, 180.0, 100.0, 120.0, 170.0, 230.0, 92.0, 104.0];
const COL_MIN: [f32; 9] = [36.0, 40.0, 80.0, 56.0, 56.0, 100.0, 80.0, 60.0, 70.0];
const REMARK: usize = 6;
/// The pinned tick-box and Operation columns, the room kept for the vertical scroll bar, the sideways bar.
const LEFT_W: f32 = 40.0;
const RIGHT_W: f32 = 124.0;
const GUTTER: f32 = 10.0;
const SCROLL_H: f32 = 14.0;
const CELL_PAD: f32 = 6.0;
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
    /// The remark being edited in a row's Remark popup.
    remark_edit: Option<(String, String)>,
    /// Proxy checks made from the list: running (`None`) or their answer.
    pub checks: std::collections::HashMap<String, Option<Result<ProxyCheck, String>>>,
    /// The widths of the columns between the pinned ones (dragged in the header), and the sideways scroll.
    widths: Vec<f32>,
    h_off: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Remark,
    Serial,
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
    CheckProxy(Vec<String>),
    EditProxy(String),
    Bulk(Vec<String>),
    SaveRemark(String, String),
}

/// The status column's view of a row: the backend's run state plus what the UI itself is doing to it.
enum St {
    Open { pid: u32, since: i64, port: u16 },
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
        (_, RunState::Open { pid, since, port }) => St::Open { pid: *pid, since: *since, port: *port },
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
    let r = ui.add(egui::TextEdit::singleline(&mut e.text).desired_width(f32::INFINITY).char_limit(match field {
        Field::Name => 100,
        Field::Serial => 9,
        Field::Remark => 2000,
    }));
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

/// The mark of the OS the profile presents (Simple Icons, CC0), in the accent blue like other antidetect browsers'.
/// "Auto" shows the OS it resolves to: the user agent's, else this computer's.
fn os_mark(ui: &mut Ui, p: &Palette, pr: &citro_core::profile::Profile) {
    use citro_core::fingerprint::Os;
    let auto = pr.persona_os.trim().is_empty();
    let os = Os::from_name(&pr.persona_os)
        .or_else(|| pr.fingerprint_config.str("ua").as_deref().and_then(Os::from_ua))
        .unwrap_or_else(Os::host);
    let tint = p.accent_hover;
    let resp = match os {
        Os::Windows => widgets::tinted_png(ui, "os-windows", OS_WINDOWS, 15.0, tint),
        Os::Mac => widgets::tinted_png(ui, "os-macos", OS_MACOS, 16.0, tint),
        Os::Linux => widgets::tinted_png(ui, "os-linux", OS_LINUX, 18.0, tint),
    };
    let name = match os {
        Os::Windows => "Windows",
        Os::Mac => "macOS",
        Os::Linux => "Linux",
    };
    resp.on_hover_text(if auto { format!("{name} (Auto)") } else { format!("{name} persona") });
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

fn head(header: &mut RowCells<'_, '_>, p: &Palette, q: &Query, label: &str, key: Option<SortKey>, acts: &mut Vec<Act>) {
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
            .filter_map(|&i| self.rows.get(i))
            .filter(|r| sel.contains(r.id()))
            .map(|r| r.id().to_owned())
            .collect()
    }

    fn visible_ids(&self) -> Vec<String> {
        // `get`: the list may have shrunk (a delete) since the view was built; refresh_view rebuilds it
        self.profiles.view.iter().filter_map(|&i| self.rows.get(i)).map(|r| r.id().to_owned()).collect()
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
        // the rows may have changed since the last frame (a delete, an import): the view must follow first
        self.refresh_view();
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
            let tip = if sel.len() > 1 { "Bulk edit the selected profiles…" } else { "Edit the selected profile…" };
            if widgets::icon_button(ui, &p, Icon::Edit, tip, any, false).clicked() {
                acts.push(if one { Act::Edit(sel[0].clone()) } else { Act::Bulk(sel.clone()) });
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
                    Field::Serial => {
                        let Ok(n) = text.parse::<u64>() else {
                            self.toast(ToastKind::Warn, "The number must be a whole number, 1 or more");
                            return;
                        };
                        if n == 0 || n == row.profile.serial_number {
                            return;
                        }
                        let old = row.profile.serial_number;
                        for r in self.rows.iter_mut() {
                            if r.id() == id {
                                r.profile.serial_number = n;
                            } else if r.profile.serial_number == n {
                                r.profile.serial_number = old;
                            }
                        }
                        self.rows_version += 1;
                        self.run_op(Vec::new(), format!("Now No. {n}"), Refresh::ROWS, move |b| b.set_serial(&id, n));
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
            Act::CheckProxy(ids) => {
                for id in ids {
                    self.check_row_proxy(id, None);
                }
            }
            Act::EditProxy(id) => self.open_proxy_dialog(&id),
            Act::Bulk(ids) => self.open_bulk(ids),
            Act::SaveRemark(id, text) => {
                let text = text.trim_end().to_owned();
                self.profiles.remark_edit = None;
                if self.row(&id).is_some_and(|r| r.profile.remark == text) {
                    return;
                }
                for r in self.rows.iter_mut().filter(|r| r.id() == id) {
                    r.profile.remark = text.clone();
                }
                self.run_op(Vec::new(), "Remark saved".into(), Refresh::ROWS, move |b| b.set_remark(&id, &text));
            }
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

    /// Check a profile's proxy from the list; with `new`, save that proxy first. A working proxy's exit IP and
    /// country are kept on the profile (the IP column and the persona's language and time zone follow them).
    pub(super) fn check_row_proxy(&mut self, id: String, new: Option<ProxyConfig>) {
        if let Some(cfg) = new.clone().filter(|c| c.hop().is_none()) {
            // no proxy: only save it, there is nothing to check
            self.profiles.checks.remove(&id);
            let id2 = id.clone();
            self.run_op(Vec::new(), "Proxy removed".into(), Refresh::ROWS, move |b| {
                let mut p = b.profile(&id2)?;
                p.user_proxy_config = cfg;
                p.ip.clear();
                b.save_profile(p).map(|_| ())
            });
            return;
        }
        self.profiles.checks.insert(id.clone(), None);
        self.tasks.spawn(true, move |w| {
            let result = (|| {
                let mut p = w.backend.profile(&id)?;
                if let Some(cfg) = new {
                    p.user_proxy_config = cfg;
                    p.ip.clear();
                    w.backend.save_profile(p)?;
                    // the saved copy may be redacted: read the full one back for the check
                    p = w.backend.profile(&id)?;
                }
                if p.user_proxy_config.hop().is_none() {
                    return Err(crate::backend::BackendError::Invalid(
                        "No proxy: the browser connects directly".into(),
                    ));
                }
                let r = w.backend.check_proxy(&p.user_proxy_config)?;
                let cc = r.country.trim().to_ascii_lowercase();
                if r.ok && (p.ip != r.ip || (!cc.is_empty() && p.country != cc)) {
                    p.ip = r.ip.clone();
                    if !cc.is_empty() {
                        p.country = cc;
                    }
                    w.backend.save_profile(p)?;
                }
                Ok(r)
            })();
            w.send(super::tasks::Msg::RowProxyChecked { id, result });
        });
    }

    pub fn row_proxy_checked(&mut self, id: String, result: crate::backend::BResult<ProxyCheck>) {
        let name = self.name_of(&id);
        match &result {
            Ok(r) if r.ok => self.toast(ToastKind::Ok, format!("{name}: proxy works ({}, {} ms)", r.ip, r.latency_ms)),
            Ok(r) => self.toast(ToastKind::Warn, format!("{name}: {}", r.message)),
            Err(e) => self.toast(ToastKind::Warn, format!("{name}: {e}")),
        }
        self.profiles.checks.insert(id, Some(result.map_err(|e| e.to_string())));
        self.refresh_rows();
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
            let State { view, selection, query: q, inline, remark_edit, checks, widths, h_off, .. } = profiles;
            let view: &Vec<usize> = view;
            let all_selected = {
                let ids: Vec<String> = view.iter().map(|&i| rows[i].id().to_owned()).collect();
                selection.all_selected(&ids)
            };
            let some_selected = !selection.is_empty();
            let body_h = (ui.available_height() - FOOTER_H).max(140.0);
            let area = Rect::from_min_size(ui.available_rect_before_wrap().min, vec2(ui.available_width(), body_h));
            ui.allocate_rect(area, Sense::hover());
            let mut table_ui = ui.new_child(
                UiBuilder::new().max_rect(area).layout(Layout::top_down(Align::Min)).id_salt("profiles-table"),
            );
            let ui = &mut table_ui;
            ui.set_clip_rect(area.intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            // the table reads a notch smaller than the rest of the app
            if let Some(f) = ui.style_mut().text_styles.get_mut(&TextStyle::Body) {
                f.size = 13.0;
            }

            // Columns: the tick box pinned on the left, Operation pinned on the right, the rest in between scrolls
            // sideways under them (a trackpad swipe, Shift + wheel, or the bar at the bottom).
            if widths.len() != COL_W.len() {
                *widths = COL_W.to_vec();
            }
            let mid_x0 = area.left() + LEFT_W;
            let right_x1 = area.right() - GUTTER;
            let right_x0 = right_x1 - RIGHT_W;
            let mid_w = (right_x0 - mid_x0).max(40.0);
            let mut eff = widths.clone();
            let natural: f32 = eff.iter().sum();
            if natural < mid_w {
                eff[REMARK] += mid_w - natural; // room to spare goes to the remark
            }
            let content: f32 = eff.iter().sum();
            let max_off = (content - mid_w).max(0.0);
            if ui.rect_contains_pointer(area) {
                let dx = ui.input(|i| i.smooth_scroll_delta.x);
                if dx != 0.0 {
                    *h_off -= dx;
                }
            }
            *h_off = h_off.clamp(0.0, max_off);
            let mut xs = vec![(area.left(), mid_x0)];
            let mut x = mid_x0 - *h_off;
            for w in &eff {
                xs.push((x, x + w));
                x += w;
            }
            xs.push((right_x0, right_x1));
            let last = xs.len() - 1;
            let clips: Vec<(f32, f32)> = xs
                .iter()
                .enumerate()
                .map(|(i, &(a, b))| if i == 0 || i == last { (a, b) } else { (a.max(mid_x0), b.min(right_x0)) })
                .collect();

            // the header: it stays put while the rows scroll
            let head_rect = Rect::from_x_y_ranges(area.x_range(), area.top()..=area.top() + HEAD_H);
            ui.painter().rect_filled(head_rect, CornerRadius::same(8), p.raised);
            {
                let mut header = RowCells {
                    ui,
                    key: usize::MAX,
                    y: (head_rect.top(), head_rect.bottom()),
                    xs: &xs,
                    clips: &clips,
                    i: 0,
                };
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
            }
            // drag the short line between two headers to resize a column (lines in the header only)
            for i in 0..eff.len() {
                let x = xs[i + 1].1;
                if x <= mid_x0 + 2.0 || x >= right_x0 - 1.0 {
                    continue;
                }
                let hr = Rect::from_x_y_ranges(x - 5.0..=x + 5.0, head_rect.top() + 4.0..=head_rect.bottom() - 4.0);
                let r = ui.interact(hr, ui.scope_id().with(("col-resize", i)), Sense::drag());
                let hot = r.hovered() || r.dragged();
                if hot {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                if r.dragged() {
                    widths[i] = (eff[i] + r.drag_delta().x).max(COL_MIN[i]);
                }
                let col = if hot { p.accent_hover } else { p.border_strong };
                ui.painter().vline(x, (head_rect.top() + 12.0)..=(head_rect.bottom() - 12.0), Stroke::new(1.0, col));
            }

            // the rows
            let body_rect =
                Rect::from_x_y_ranges(area.x_range(), (head_rect.bottom() + 2.0)..=(area.bottom() - SCROLL_H));
            let n = view.len();
            let stripe = ui.visuals().faint_bg_color;
            ui.scope_builder(UiBuilder::new().max_rect(body_rect), |ui| {
                ScrollArea::vertical().id_salt("profiles-body").auto_shrink([false, false]).show_rows(
                    ui,
                    ROW_H,
                    n,
                    |ui, range| {
                        for vi in range {
                            let (rect, row_resp) =
                                ui.allocate_exact_size(vec2(body_rect.width(), ROW_H), Sense::click());
                            {
                                let r = &rows[view[vi]];
                                let id = r.id().to_owned();
                                let pr = &r.profile;
                                let selected = selection.contains(&id);
                                let st = status_of(r, busy.get(&id).copied(), last_error.get(&id));
                                let working = matches!(st, St::Busy(_));
                                let row_hot = ui.rect_contains_pointer(rect);
                                let fill = if selected {
                                    p.accent.gamma_multiply(0.16)
                                } else if row_hot {
                                    p.hover
                                } else if vi % 2 == 1 {
                                    stripe
                                } else {
                                    Color32::TRANSPARENT
                                };
                                ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                                let mut row = RowCells {
                                    ui,
                                    key: vi,
                                    y: (rect.top(), rect.bottom()),
                                    xs: &xs,
                                    clips: &clips,
                                    i: 0,
                                };

                                row.col(|ui| {
                                    if widgets::checkbox(ui, &p, selected, false).clicked() {
                                        acts.push(Act::ToggleCheck(id.clone()));
                                    }
                                });
                                row.col(|ui| {
                                    if !inline_edit(ui, inline, &id, Field::Serial, &mut acts) {
                                        let r = ui
                                            .add(
                                                egui::Label::new(dim(&p, pr.serial_number.to_string()))
                                                    .sense(Sense::click()),
                                            )
                                            .on_hover_text(
                                                "Double-click to change the number (sort by No. to put profiles first)",
                                            );
                                        if r.double_clicked() {
                                            acts.push(Act::StartEdit(
                                                id.clone(),
                                                Field::Serial,
                                                pr.serial_number.to_string(),
                                            ));
                                        }
                                    }
                                });
                                row.col(|ui| {
                                    ui.add(egui::Label::new(dim(&p, &id)).truncate());
                                });
                                // Name: the OS mark, then the name on up to two lines
                                row.col(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    os_mark(ui, &p, pr);
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
                                row.col(|ui| {
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
                                row.col(|ui| {
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
                                // IP: the check arrows, the proxy type, the exit address and (on a hovered row) the
                                // edit mark; the country under it
                                row.col(|ui| {
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
                                    let current = match &hop {
                                        Some(h) if h.account.is_empty() => {
                                            format!("{}://{}:{}", h.scheme, h.host, h.port)
                                        }
                                        Some(h) => format!("{}://{}:***@{}:{}", h.scheme, h.account, h.host, h.port),
                                        None => "No proxy: the browser connects directly".to_owned(),
                                    };
                                    let check = checks.get(&id);
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 3.0;
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            // the two arrows: check the proxy (green / red once checked)
                                            if matches!(check, Some(None)) {
                                                ui.add_sized(vec2(16.0, 14.0), egui::Spinner::new().size(11.0));
                                            } else {
                                                let tint = match check {
                                                    Some(Some(Ok(c))) if c.ok => p.ok,
                                                    Some(Some(_)) => p.err,
                                                    _ => p.dim,
                                                };
                                                let tip = match check {
                                                    Some(Some(Ok(c))) if c.ok => format!(
                                                        "Works: exit IP {} \u{b7} {} ms\nClick to check again",
                                                        c.ip, c.latency_ms
                                                    ),
                                                    Some(Some(Ok(c))) => format!("{}\nClick to check again", c.message),
                                                    Some(Some(Err(e))) => format!("{e}\nClick to check again"),
                                                    _ => "Check the proxy".to_owned(),
                                                };
                                                if swap_arrows(ui, tint, hop.is_some()).on_hover_text(tip).clicked() {
                                                    acts.push(Act::CheckProxy(vec![id.clone()]));
                                                }
                                            }
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
                                            ui.add(egui::Label::new(main).truncate()).on_hover_text(&current);
                                            if row_hot {
                                                let (r, resp) =
                                                    ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                                                let tint = if resp.hovered() { p.accent } else { p.accent_hover };
                                                widgets::paint_icon_scaled(
                                                    ui.painter(),
                                                    r.center(),
                                                    Icon::Edit,
                                                    tint,
                                                    0.62,
                                                );
                                                if resp
                                                    .on_hover_cursor(CursorIcon::PointingHand)
                                                    .on_hover_text("Edit the proxy")
                                                    .clicked()
                                                {
                                                    acts.push(Act::EditProxy(id.clone()));
                                                }
                                            }
                                        });
                                        if !place.is_empty() {
                                            egui::Frame::NONE
                                                .inner_margin(egui::Margin { left: 22, ..Default::default() })
                                                .show(ui, |ui| {
                                                    ui.add(egui::Label::new(small_dim(&p, &place)).truncate());
                                                });
                                        }
                                    });
                                });
                                // Remark: a click shows the whole text to read, select, copy or change
                                row.col(|ui| {
                                    if inline_edit(ui, inline, &id, Field::Remark, &mut acts) {
                                        return;
                                    }
                                    let text = pr.remark.trim();
                                    let r = if text.is_empty() {
                                        ui.add(egui::Label::new(dim(&p, "\u{2013}")).sense(Sense::click()))
                                    } else {
                                        two_lines(ui, RichText::new(text).color(p.text))
                                    };
                                    let r = r
                                        .interact(Sense::click())
                                        .on_hover_cursor(CursorIcon::PointingHand)
                                        .on_hover_text(if text.is_empty() {
                                            "Click to write a remark".to_owned()
                                        } else {
                                            format!("{text}\nClick to select, copy or edit")
                                        });
                                    Popup::menu(&r).close_behavior(PopupCloseBehavior::CloseOnClickOutside).show(
                                        |ui| {
                                            ui.set_width(380.0);
                                            if remark_edit.as_ref().is_none_or(|re| re.0 != id) {
                                                *remark_edit = Some((id.clone(), pr.remark.clone()));
                                            }
                                            let Some(re) = remark_edit.as_mut() else { return };
                                            ui.label(widgets::semibold("Remark", 14.0));
                                            ui.add(
                                                egui::TextEdit::multiline(&mut re.1)
                                                    .hint_text("A note about this profile")
                                                    .desired_rows(5)
                                                    .desired_width(f32::INFINITY)
                                                    .char_limit(2000),
                                            );
                                            ui.horizontal(|ui| {
                                                if widgets::primary_button(ui, &p, "Save", true).clicked() {
                                                    acts.push(Act::SaveRemark(id.clone(), re.1.clone()));
                                                    ui.close();
                                                }
                                                if ui.button("Copy all").clicked() {
                                                    ui.ctx().copy_text(re.1.clone());
                                                }
                                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                    ui.label(small_dim(&p, format!("{} / 2000", re.1.chars().count())));
                                                });
                                            });
                                        },
                                    );
                                });
                                row.col(|ui| {
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
                                row.col(|ui| match &st {
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
                                row.col(|ui| {
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
                                    if ui.add_enabled(n > 1, Button::new(many("Bulk edit…"))).clicked() {
                                        acts.push(Act::Bulk(targets.clone()));
                                        ui.close();
                                    }
                                    if ui.add_enabled(n == 1, Button::new("Duplicate")).clicked() {
                                        acts.push(Act::Duplicate(id.clone()));
                                        ui.close();
                                    }
                                    if ui.add_enabled(n == 1, Button::new("Edit proxy\u{2026}")).clicked() {
                                        acts.push(Act::EditProxy(id.clone()));
                                        ui.close();
                                    }
                                    if ui.button(many("Check proxy")).clicked() {
                                        acts.push(Act::CheckProxy(targets.clone()));
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

                                let resp = row_resp;
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
                            }
                        }
                    },
                );
            });

            // a soft shadow where the scrolled columns slide under the pinned ones
            let y = area.top()..=body_rect.bottom();
            let shade = |x: f32, dir: f32| {
                for k in 0..8 {
                    let a = (40 - k * 5) as u8;
                    ui.painter().vline(x + dir * k as f32, y.clone(), Stroke::new(1.0, Color32::from_black_alpha(a)));
                }
            };
            if *h_off > 0.5 {
                shade(mid_x0, 1.0);
            }
            if *h_off < max_off - 0.5 {
                shade(right_x0, -1.0);
            }
            // the sideways scroll bar
            if max_off > 0.0 {
                let track =
                    Rect::from_x_y_ranges(mid_x0..=right_x0, (area.bottom() - SCROLL_H + 4.0)..=(area.bottom() - 2.0));
                let tw = (mid_w * mid_w / content).max(40.0);
                let room = (mid_w - tw).max(1.0);
                let tx = mid_x0 + (*h_off / max_off) * room;
                let thumb = Rect::from_x_y_ranges(tx..=tx + tw, track.y_range());
                let r = ui.interact(track, ui.scope_id().with("h-scroll"), Sense::click_and_drag());
                if r.dragged() {
                    *h_off = (*h_off + r.drag_delta().x * max_off / room).clamp(0.0, max_off);
                } else if r.clicked()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    *h_off = ((pos.x - mid_x0 - tw / 2.0) / room * max_off).clamp(0.0, max_off);
                }
                let hot = r.hovered() || r.dragged();
                ui.painter().rect_filled(track, CornerRadius::same(4), p.raised);
                ui.painter().rect_filled(thumb, CornerRadius::same(4), if hot { p.dim } else { p.border_strong });
            }
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

/// The two arrows (⇄) that check a proxy.
fn swap_arrows(ui: &mut Ui, tint: Color32, enabled: bool) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 14.0), if enabled { Sense::click() } else { Sense::hover() });
    let tint = if enabled && resp.hovered() {
        tint.gamma_multiply(1.4)
    } else if enabled {
        tint
    } else {
        tint.gamma_multiply(0.4)
    };
    let st = Stroke::new(1.3, tint);
    let g = ui.painter();
    let (l, rr, c) = (r.left() + 1.0, r.right() - 1.0, r.center().y);
    // upper arrow to the right, lower arrow to the left
    g.line_segment([pos2(l, c - 3.0), pos2(rr, c - 3.0)], st);
    g.line_segment([pos2(rr - 3.5, c - 6.0), pos2(rr, c - 3.0)], st);
    g.line_segment([pos2(l, c + 3.0), pos2(rr, c + 3.0)], st);
    g.line_segment([pos2(l + 3.5, c + 6.0), pos2(l, c + 3.0)], st);
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// One row of the profile table: hands out a clipped child `Ui` per column, left to right.
struct RowCells<'u, 'a> {
    ui: &'u mut Ui,
    /// Unique per row (the header is `usize::MAX`), so widgets in different rows get different ids.
    key: usize,
    y: (f32, f32),
    xs: &'a [(f32, f32)],
    clips: &'a [(f32, f32)],
    i: usize,
}

impl RowCells<'_, '_> {
    fn col(&mut self, add: impl FnOnce(&mut Ui)) {
        let i = self.i;
        self.i += 1;
        let ((x0, x1), (c0, c1)) = (self.xs[i], self.clips[i]);
        if c1 - c0 < 1.0 {
            return; // scrolled out of view
        }
        let rect =
            Rect::from_x_y_ranges((x0 + CELL_PAD)..=(x1 - CELL_PAD).max(x0 + CELL_PAD + 1.0), self.y.0..=self.y.1);
        let clip = Rect::from_x_y_ranges(c0..=c1, self.y.0..=self.y.1).intersect(self.ui.clip_rect());
        let mut child = self.ui.new_child(
            UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)).id_salt(("cell", self.key, i)),
        );
        child.set_clip_rect(clip);
        add(&mut child);
    }
}

fn status_cell(ui: &mut Ui, p: &Palette, st: &St, now: i64, offset: i32) {
    match st {
        St::Open { pid, since, port } => {
            // two lines: Open, then the port automation tools connect to (click to copy)
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    widgets::dot(ui, p.ok, true);
                    ui.label(RichText::new("Open").color(p.ok))
                        .on_hover_text(format!("Open since {} (pid {pid})", timefmt::since(now, *since, offset)));
                });
                if *port > 0 {
                    let addr = format!("127.0.0.1:{port}");
                    let r = ui
                        .add(
                            egui::Label::new(RichText::new(format!("Port {port}")).size(11.5).color(p.accent_hover))
                                .sense(Sense::click()),
                        )
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .on_hover_text(format!(
                            "DevTools port: Puppeteer, Playwright and Selenium connect to {addr}\n\
                             (ws:// address: http://{addr}/json/version). Click to copy {addr}"
                        ));
                    if r.clicked() {
                        ui.ctx().copy_text(addr);
                    }
                }
            });
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
