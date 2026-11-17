//! The Logs page. Two views: `Citroenition` (what the app did: opened, closed, exported, failed ...) and `Browser`
//! (the tail of one profile's kernel log). Lines are fetched on a worker thread every ~1.5 s while the page is
//! open; the filters run in the backend so a long log stays cheap, and "Clear view" only hides what is on screen.

use super::app::{CitroApp, ToastKind};
use super::tasks::Msg;
use super::theme::Palette;
use super::widgets::{self, small_dim};
use crate::backend::{Level, LogKind, LogLine, LogQuery, Row};
use crate::timefmt::{self, DateTime};
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, CornerRadius, FontId, Frame, Layout, Margin, Painter, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextEdit, Ui, pos2, vec2,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const FETCH_EVERY: Duration = Duration::from_millis(1500);
const LIMIT: usize = 2000;
const ROW_H: f32 = 21.0;
const TIME_W: f32 = 116.0;
const CHIP_W: f32 = 50.0;
const PROFILE_W: f32 = 150.0;

pub struct State {
    kind: LogKind,
    level: Level,
    text: String,
    /// The Browser tab's profile; on the Citroenition tab it is an optional filter.
    profile: Option<String>,
    lines: Vec<LogLine>,
    /// The query of the newest request: a different one means "fetch now".
    sent: Option<LogQuery>,
    token: u64,
    dirty: bool,
    inflight: bool,
    last_fetch: Option<Instant>,
    error: Option<String>,
    follow: bool,
    /// `Clear view`: hide everything up to this point (`ts`, how many lines with that `ts` were already shown).
    cleared: Option<(i64, usize)>,
}

impl Default for State {
    fn default() -> State {
        State {
            kind: LogKind::Citro,
            level: Level::Info,
            text: String::new(),
            profile: None,
            lines: Vec::new(),
            sent: None,
            token: 0,
            dirty: true,
            inflight: false,
            last_fetch: None,
            error: None,
            follow: true,
            cleared: None,
        }
    }
}

impl State {
    /// Switch to the Browser tab; the page picks a profile when none is set.
    pub fn show_browser(&mut self) {
        self.kind = LogKind::Browser;
        self.dirty = true;
    }

    pub fn show_browser_for(&mut self, id: &str) {
        self.kind = LogKind::Browser;
        self.profile = Some(id.to_owned());
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// A finished fetch; answers to superseded requests are dropped.
    pub fn apply(&mut self, token: u64, r: Result<Vec<LogLine>, String>) {
        if token != self.token {
            return;
        }
        self.inflight = false;
        match r {
            Ok(lines) => {
                self.lines = lines;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn query(&self) -> LogQuery {
        LogQuery {
            kind: self.kind,
            profile_id: self.profile.clone(),
            level_min: self.level,
            text: self.text.trim().to_owned(),
            limit: LIMIT,
        }
    }

    /// The fetched lines minus what `Clear view` hid.
    fn visible(&self) -> Vec<&LogLine> {
        let Some((t, n)) = self.cleared else { return self.lines.iter().collect() };
        let mut skip = n;
        let keep = |l: &&LogLine| {
            if l.ts < t {
                false
            } else if l.ts == t && skip > 0 {
                skip -= 1;
                false
            } else {
                true
            }
        };
        self.lines.iter().filter(keep).collect()
    }

    fn clear_view(&mut self) {
        self.cleared = self.lines.last().map(|last| (last.ts, self.lines.iter().filter(|l| l.ts == last.ts).count()));
    }
}

fn level_text(l: Level) -> &'static str {
    match l {
        Level::Trace => "Everything",
        Level::Debug => "Debug and up",
        Level::Info => "Info and up",
        Level::Warn => "Warnings and errors",
        Level::Error => "Errors only",
    }
}

fn level_color(p: &Palette, l: Level) -> Color32 {
    match l {
        Level::Error => p.err,
        Level::Warn => p.warn,
        Level::Info => p.accent,
        Level::Debug | Level::Trace => p.idle,
    }
}

fn message_color(p: &Palette, l: Level) -> Color32 {
    match l {
        Level::Error => p.err,
        Level::Warn => p.warn,
        Level::Info => p.text,
        Level::Debug | Level::Trace => p.dim,
    }
}

/// `11-16 09:32:01`, empty when the line has no time.
fn stamp(ts: i64, offset: i32) -> String {
    if ts <= 0 {
        return String::new();
    }
    let d = DateTime::from_unix(ts, offset);
    format!("{:02}-{:02} {:02}:{:02}:{:02}", d.month, d.day, d.hour, d.minute, d.second)
}

/// One line as plain text for the clipboard.
fn plain(l: &LogLine, profile: Option<&str>, offset: i32) -> String {
    let mut out = String::new();
    if l.ts > 0 {
        out.push_str(&timefmt::absolute(l.ts, offset));
        out.push(' ');
    }
    out.push_str(&format!("{:<5} ", l.level.label()));
    if let Some(name) = profile {
        out.push_str(&format!("[{name}] "));
    }
    out.push_str(&l.message);
    out
}

/// Draw `text` in one row of `w` pixels, cut with an ellipsis. Returns `true` when it was cut.
fn text_cell(g: &Painter, x: f32, w: f32, cy: f32, text: &str, font: FontId, color: Color32) -> bool {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(w.max(12.0));
    let galley = g.layout_job(job);
    let cut = galley.elided;
    let pos: Pos2 = pos2(x, cy - galley.size().y / 2.0);
    g.galley(pos, galley, color);
    cut
}

struct Look<'a> {
    p: &'a Palette,
    offset: i32,
    time: bool,
    profile: bool,
}

fn paint_line(ui: &mut Ui, look: &Look, idx: usize, l: &LogLine, name: Option<&str>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = look.p;
    let g = ui.painter().with_clip_rect(rect);
    if resp.hovered() {
        g.rect_filled(rect, 0.0, p.raised);
    } else if idx % 2 == 1 {
        g.rect_filled(rect, 0.0, p.raised.gamma_multiply(0.35));
    }
    let cy = rect.center().y;
    let mut x = rect.left() + 10.0;
    let mono = FontId::monospace(12.5);
    if look.time {
        g.text(pos2(x, cy), Align2::LEFT_CENTER, stamp(l.ts, look.offset), mono.clone(), p.dim);
        x += TIME_W;
    }
    let col = level_color(p, l.level);
    let chip = Rect::from_min_size(pos2(x, cy - 8.0), vec2(CHIP_W - 6.0, 16.0));
    g.rect_filled(chip, CornerRadius::same(8), col.gamma_multiply(0.2));
    g.text(chip.center(), Align2::CENTER_CENTER, l.level.label(), FontId::proportional(10.5), col);
    x += CHIP_W + 6.0;
    if look.profile {
        if let Some(name) = name {
            text_cell(&g, x, PROFILE_W - 8.0, cy, name, FontId::proportional(13.0), p.dim);
        }
        x += PROFILE_W;
    }
    let cut = text_cell(&g, x, rect.right() - x - 10.0, cy, &l.message, mono, message_color(p, l.level));
    let tip = if cut || l.ts > 0 {
        let when = if l.ts > 0 { format!("{}\n", timefmt::absolute(l.ts, look.offset)) } else { String::new() };
        Some(format!("{when}{}", l.message))
    } else {
        None
    };
    match tip {
        Some(t) => resp.on_hover_text(t),
        None => resp,
    }
}

impl CitroApp {
    pub fn logs_page(&mut self, ui: &mut Ui) {
        let p = self.pal;
        if self.logs_ui.kind == LogKind::Browser && self.logs_ui.profile.is_none() {
            self.logs_ui.profile = self.default_browser_profile();
        }
        if let Some(id) = &self.logs_ui.profile
            && self.rows_loaded
            && self.row(id).is_none()
        {
            self.logs_ui.profile = None;
        }
        self.logs_toolbar(ui, &p);
        self.logs_fetch();
        ui.add_space(8.0);
        self.logs_body(ui, &p);
    }

    /// The profile the Browser tab opens on: the selected one, else a running one, else the first.
    fn default_browser_profile(&self) -> Option<String> {
        self.selected_ids()
            .first()
            .cloned()
            .or_else(|| self.rows.iter().find(|r| r.run.is_open()).map(|r| r.id().to_owned()))
            .or_else(|| self.rows.first().map(|r| r.id().to_owned()))
    }

    fn logs_toolbar(&mut self, ui: &mut Ui, p: &Palette) {
        let mut sorted: Vec<&Row> = self.rows.iter().collect();
        sorted.sort_by_key(|r| r.profile.name.to_lowercase());
        let st = &mut self.logs_ui;
        let (mut copy, mut clear, mut refresh) = (false, false, false);
        let shown = st.visible().len();
        let kinds = [LogKind::Citro, LogKind::Browser];
        let labels = [kinds[0].label(), kinds[1].label()];
        let active = kinds.iter().position(|k| *k == st.kind).unwrap_or(0);
        let more = if st.lines.len() >= LIMIT { format!(" (newest {LIMIT})") } else { String::new() };
        let count = format!("{shown} line{}{more}", if shown == 1 { "" } else { "s" });
        if let Some(i) = widgets::tab_header(ui, p, &labels, active, |ui| {
            ui.label(small_dim(p, count));
        }) && st.kind != kinds[i]
        {
            st.kind = kinds[i];
            st.dirty = true;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let level_box = ComboBox::from_id_salt("logs-level").width(170.0).icon(widgets::chevron_icon);
            level_box.selected_text(level_text(st.level)).show_ui(ui, |ui| {
                for l in Level::ALL {
                    ui.selectable_value(&mut st.level, l, level_text(l));
                }
            });
            ui.add(TextEdit::singleline(&mut st.text).hint_text("Filter lines\u{2026}").desired_width(230.0));
            if !st.text.is_empty()
                && ui.add(egui::Button::new("\u{2715}").frame(false)).on_hover_text("Clear").clicked()
            {
                st.text.clear();
            }
            let current = st.profile.as_deref().and_then(|id| sorted.iter().find(|r| r.id() == id));
            let label = match (current, st.kind) {
                (Some(r), _) => r.profile.name.clone(),
                (None, LogKind::Citro) => "All profiles".to_owned(),
                (None, LogKind::Browser) => "Choose a profile".to_owned(),
            };
            let profile_box = ComboBox::from_id_salt("logs-profile").width(210.0).icon(widgets::chevron_icon);
            profile_box.selected_text(label).show_ui(ui, |ui| {
                if st.kind == LogKind::Citro {
                    ui.selectable_value(&mut st.profile, None, "All profiles");
                }
                for r in &sorted {
                    ui.selectable_value(&mut st.profile, Some(r.id().to_owned()), &r.profile.name);
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = widgets::button(ui, "Refresh", true).clicked();
                clear = widgets::button(ui, "Clear view", shown > 0).clicked();
                copy = widgets::button(ui, "Copy", shown > 0).on_hover_text("Copy the shown lines").clicked();
                ui.checkbox(&mut st.follow, "Follow");
            });
        });
        if refresh {
            st.dirty = true;
        }
        if clear {
            st.clear_view();
        }
        if copy {
            self.logs_copy();
        }
    }

    fn logs_copy(&mut self) {
        let names: HashMap<&str, &str> = self.rows.iter().map(|r| (r.id(), r.profile.name.as_str())).collect();
        let st = &self.logs_ui;
        let text: Vec<String> = st
            .visible()
            .into_iter()
            .map(|l| plain(l, l.profile_id.as_deref().and_then(|id| names.get(id).copied()), self.offset))
            .collect();
        let n = text.len();
        self.ctx.copy_text(text.join("\n"));
        self.toast(ToastKind::Info, format!("Copied {n} line{}", if n == 1 { "" } else { "s" }));
    }

    fn logs_fetch(&mut self) {
        let st = &mut self.logs_ui;
        let q = st.query();
        let same_source = st.sent.as_ref().is_some_and(|s| s.kind == q.kind && s.profile_id == q.profile_id);
        if st.sent.as_ref() != Some(&q) {
            st.dirty = true;
            st.cleared = None;
            if !same_source {
                st.lines.clear();
                st.error = None;
            }
        }
        if st.kind == LogKind::Browser && st.profile.is_none() {
            st.sent = Some(q);
            st.dirty = false;
            return;
        }
        let due = st.last_fetch.is_none_or(|t| t.elapsed() >= FETCH_EVERY);
        if !(st.dirty || (due && !st.inflight)) {
            return;
        }
        st.token += 1;
        let token = st.token;
        (st.inflight, st.dirty, st.last_fetch, st.sent) = (true, false, Some(Instant::now()), Some(q.clone()));
        self.tasks.spawn(false, move |w| w.send(Msg::Logs { token, result: w.backend.logs(&q) }));
    }

    fn logs_body(&mut self, ui: &mut Ui, p: &Palette) {
        let names: HashMap<&str, &str> = self.rows.iter().map(|r| (r.id(), r.profile.name.as_str())).collect();
        let st = &self.logs_ui;
        let visible = st.visible();
        let look =
            Look { p, offset: self.offset, time: visible.iter().any(|l| l.ts > 0), profile: st.kind == LogKind::Citro };
        let avail = ui.available_height();
        let mut copied: Option<String> = None;
        Frame::NONE
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(2, 6))
            .show(ui, |ui| {
                ui.set_min_size(vec2(ui.available_width(), (avail - 16.0).max(120.0)));
                if let Some(e) = &st.error {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("  Could not read the log: {e}")).color(p.err));
                    return;
                }
                if visible.is_empty() {
                    self.logs_empty(ui, p, st);
                    return;
                }
                ui.spacing_mut().item_spacing.y = 0.0;
                ScrollArea::vertical()
                    .id_salt("logs-scroll")
                    .auto_shrink([false, false])
                    .max_height((avail - 16.0).max(120.0))
                    .stick_to_bottom(st.follow)
                    .show_rows(ui, ROW_H, visible.len(), |ui, range| {
                        for i in range {
                            let l = visible[i];
                            let name = l.profile_id.as_deref().and_then(|id| names.get(id).copied());
                            let r = paint_line(ui, &look, i, l, name);
                            r.context_menu(|ui| {
                                if ui.button("Copy line").clicked() {
                                    copied = Some(plain(l, name, look.offset));
                                    ui.close();
                                }
                            });
                        }
                    });
            });
        if let Some(t) = copied {
            self.ctx.copy_text(t);
            self.toast(ToastKind::Info, "Copied the line");
        }
    }

    fn logs_empty(&self, ui: &mut Ui, p: &Palette, st: &State) {
        let filtered = !st.text.trim().is_empty() || st.level != Level::Trace || st.profile.is_some();
        let (glyph, title, body) = match st.kind {
            LogKind::Browser if st.profile.is_none() => (
                "\u{1f5a5}",
                "Choose a profile",
                "The Browser view shows the kernel log of one profile. Pick it in the list above.",
            ),
            LogKind::Browser if filtered && !st.lines.is_empty() => {
                ("\u{1f50d}", "No line matches", "Lower the level or change the filter text.")
            }
            LogKind::Browser => (
                "\u{1f5a5}",
                "No browser output yet",
                "The kernel writes its log while a profile runs. Open the profile and this view fills in live.",
            ),
            LogKind::Citro if !st.text.trim().is_empty() || st.profile.is_some() || st.level != Level::Info => (
                "\u{1f50d}",
                "No event matches",
                "Lower the level, pick \u{201c}All profiles\u{201d} or change the filter text.",
            ),
            LogKind::Citro => (
                "\u{1f4dc}",
                "Nothing has happened yet",
                "Opening and closing profiles, exports, proxy checks and kernel installs are logged here.",
            ),
        };
        widgets::empty_state(ui, p, glyph, title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ts: i64, msg: &str) -> LogLine {
        LogLine { ts, level: Level::Info, profile_id: None, message: msg.to_owned() }
    }

    #[test]
    fn clear_view_hides_only_what_was_shown() {
        let mut st = State { lines: vec![line(10, "a"), line(10, "b"), line(11, "c")], ..State::default() };
        st.clear_view();
        assert!(st.visible().is_empty());
        // the same second gets another line, and a later one arrives
        st.lines = vec![line(10, "a"), line(10, "b"), line(11, "c"), line(11, "d"), line(12, "e")];
        let shown: Vec<&str> = st.visible().iter().map(|l| l.message.as_str()).collect();
        assert_eq!(shown, ["d", "e"]);
    }

    #[test]
    fn clear_view_works_when_lines_have_no_time() {
        let mut st = State { lines: vec![line(0, "x"), line(0, "y")], ..State::default() };
        st.clear_view();
        st.lines.push(line(0, "z"));
        assert_eq!(st.visible().len(), 1);
    }

    #[test]
    fn a_stale_answer_is_ignored() {
        let mut st = State { token: 5, inflight: true, ..State::default() };
        st.apply(4, Ok(vec![line(1, "old")]));
        assert!(st.lines.is_empty() && st.inflight);
        st.apply(5, Ok(vec![line(1, "new")]));
        assert_eq!(st.lines.len(), 1);
        assert!(!st.inflight);
        st.apply(5, Err("boom".into()));
        assert_eq!(st.error.as_deref(), Some("boom"));
    }

    #[test]
    fn clipboard_text_names_the_profile_and_level() {
        let l = LogLine { ts: 0, level: Level::Warn, profile_id: Some("k1".into()), message: "slow gpu".into() };
        assert_eq!(plain(&l, Some("Shop FR"), 0), "WARN  [Shop FR] slow gpu");
        assert_eq!(stamp(0, 0), "");
        assert_eq!(stamp(1_700_000_000, 0), "11-14 22:13:20");
    }
}
