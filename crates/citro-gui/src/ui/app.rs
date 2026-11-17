//! The application shell: state, message pump, sidebar, top bar, keyboard shortcuts and the screenshot harness.

use super::dialogs::{Confirm, MoveDialog, OpenFailure, RemarkDialog};
use super::shot::Shot;
use super::tasks::{Msg, OpDone, Refresh, Tasks};
use super::theme::{self, Palette};
use super::widgets::Icon;
use super::{edit, export, extensions, groups, kernel, logs, profiles, settings, teams, update};
use crate::backend::{
    BResult, Backend, BackendInfo, BackendKind, HubStatus, KernelStatus, OpenOpts, Query, Row, RunState, TeamInfo,
};
use crate::cli::StartPage;
use crate::settings::Settings;
use crate::timefmt;
use citro_core::profile::Group;
use eframe::egui::{self, CornerRadius, Frame, Key, Margin, Modifiers, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

const POLL_ROWS: Duration = Duration::from_secs(2);
const POLL_SLOW: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    #[default]
    Profiles,
    Groups,
    Logs,
    Kernel,
    Extensions,
    Teams,
    Settings,
}

impl Page {
    pub fn label(self) -> &'static str {
        match self {
            Page::Profiles => "Profiles",
            Page::Groups => "Groups",
            Page::Logs => "Logs",
            Page::Kernel => "Kernel",
            Page::Extensions => "Extensions",
            Page::Teams => "Teams",
            Page::Settings => "Settings",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Page::Profiles => Icon::Profiles,
            Page::Groups => Icon::Groups,
            Page::Logs => Icon::Logs,
            Page::Kernel => Icon::Kernel,
            Page::Extensions => Icon::Puzzle,
            Page::Teams => Icon::Teams,
            Page::Settings => Icon::Settings,
        }
    }

    pub fn from_start(s: StartPage) -> Page {
        match s {
            StartPage::Groups => Page::Groups,
            StartPage::Logs | StartPage::LogsBrowser => Page::Logs,
            StartPage::Kernel => Page::Kernel,
            StartPage::Extensions => Page::Extensions,
            StartPage::Teams => Page::Teams,
            StartPage::Settings => Page::Settings,
            _ => Page::Profiles,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub born: Instant,
}

impl Toast {
    pub fn life(&self) -> Duration {
        Duration::from_secs(match self.kind {
            ToastKind::Err => 9,
            ToastKind::Warn => 6,
            _ => 4,
        })
    }
}

/// Where the app is on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    Idle,
    /// Team browsers are being closed and their data uploaded: the window stays until that is done.
    Working,
    Done,
}

/// What the UI is doing to a profile right now (the backend only knows open / closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Opening,
    Closing,
    Working,
}

pub struct AppOptions {
    pub backend: Arc<dyn Backend>,
    pub settings: Settings,
    /// `None` = demo: settings live in memory only.
    pub settings_path: Option<PathBuf>,
    pub start: StartPage,
    pub screenshot: Option<PathBuf>,
    /// Demo profiles whose browser fails to start, so a screenshot can show the red error state.
    pub demo_failing: Vec<String>,
}

pub struct CitroApp {
    pub ctx: egui::Context,
    pub backend: Arc<dyn Backend>,
    pub info: BackendInfo,
    pub tasks: Tasks,
    pub settings: Settings,
    pub saved_settings: Settings,
    pub settings_path: Option<PathBuf>,
    pub pal: Palette,
    pub page: Page,
    pub now: i64,
    pub offset: i32,
    pub home: Option<PathBuf>,

    // cached backend data -----------------------------------------------------------------------------------
    pub rows: Vec<Row>,
    pub rows_version: u64,
    pub rows_loaded: bool,
    pub rows_error: Option<String>,
    pub(super) rows_inflight: bool,
    pub(super) last_poll: Instant,
    /// Quitting while team browsers are open waits for their data to reach the hub.
    pub quit: Quit,
    pub(super) last_slow: Instant,
    pub groups: Vec<Group>,
    pub groups_version: u64,
    pub kernel: Option<KernelStatus>,
    pub kernel_error: Option<String>,
    pub teams: Option<Vec<TeamInfo>>,
    pub teams_error: Option<String>,
    pub hub: HubStatus,

    // transient UI state ------------------------------------------------------------------------------------
    pub busy: HashMap<String, Busy>,
    pub last_error: HashMap<String, String>,
    pub toasts: Vec<Toast>,
    pub failures: Vec<OpenFailure>,
    pub quiet_failures: bool,
    pub confirm: Option<Confirm>,
    pub move_dlg: Option<MoveDialog>,
    pub remark_dlg: Option<RemarkDialog>,
    pub edit: Option<edit::EditDialog>,
    pub export: Option<export::ExportDialog>,

    // pages -------------------------------------------------------------------------------------------------
    pub profiles: profiles::State,
    pub groups_ui: groups::State,
    pub logs_ui: logs::State,
    pub kernel_ui: kernel::State,
    pub teams_ui: teams::State,
    pub settings_ui: settings::State,
    pub extensions_ui: extensions::State,
    pub update: update::State,

    pub(super) scenario: Option<StartPage>,
    pub(super) demo_failing: Vec<String>,
    pub(super) shot: Option<Shot>,
}

impl CitroApp {
    pub fn new(cc: &eframe::CreationContext<'_>, o: AppOptions) -> CitroApp {
        let ctx = cc.egui_ctx.clone();
        theme::install_fonts(&ctx);
        theme::apply(&ctx, o.settings.theme);
        o.backend.set_kernel_override(o.settings.kernel_override());
        let (tx, rx) = channel();
        let info = o.backend.info();
        let mut app = CitroApp {
            tasks: Tasks::new(ctx.clone(), o.backend.clone()),
            ctx,
            info,
            backend: o.backend,
            saved_settings: o.settings.clone(),
            pal: theme::palette(o.settings.theme),
            settings: o.settings,
            settings_path: o.settings_path,
            page: Page::from_start(o.start),
            now: timefmt::now_ts(),
            offset: timefmt::detect_local_offset(),
            home: crate::util::home_dir(),
            rows: Vec::new(),
            rows_version: 0,
            rows_loaded: false,
            rows_error: None,
            rows_inflight: false,
            last_poll: Instant::now(),
            quit: Quit::Idle,
            last_slow: Instant::now(),
            groups: Vec::new(),
            groups_version: 0,
            kernel: None,
            kernel_error: None,
            teams: None,
            teams_error: None,
            hub: HubStatus::default(),
            busy: HashMap::new(),
            last_error: HashMap::new(),
            toasts: Vec::new(),
            failures: Vec::new(),
            quiet_failures: false,
            confirm: None,
            move_dlg: None,
            remark_dlg: None,
            edit: None,
            export: None,
            profiles: profiles::State::default(),
            groups_ui: groups::State::default(),
            logs_ui: logs::State::default(),
            kernel_ui: kernel::State::default(),
            teams_ui: teams::State::default(),
            settings_ui: settings::State::default(),
            extensions_ui: extensions::State::default(),
            update: update::State::default(),
            scenario: Some(o.start),
            demo_failing: o.demo_failing,
            shot: o.screenshot.map(|out| Shot { out, frame: 0, started: Instant::now(), requested: false, tx, rx }),
        };
        if o.start == StartPage::LogsBrowser {
            app.logs_ui.show_browser();
        }
        app.refresh_all();
        app.check_updates(false);
        app
    }

    // ----------------------------------------------------------------------------------------------------------
    // data refresh

    pub fn refresh_rows(&mut self) {
        if self.rows_inflight {
            return;
        }
        self.rows_inflight = true;
        self.tasks.spawn(false, |w| {
            w.send(Msg::Rows(w.backend.list_profiles(&Query::default())));
            let notices = w.backend.take_notices();
            if !notices.is_empty() {
                w.send(Msg::Notices(notices));
            }
        });
    }

    pub fn refresh_groups(&mut self) {
        self.tasks.spawn(false, |w| w.send(Msg::Groups(w.backend.groups())));
    }

    pub fn refresh_kernel(&mut self) {
        self.tasks.spawn(false, |w| w.send(Msg::Kernel(w.backend.kernel_status())));
    }

    pub fn refresh_teams(&mut self) {
        self.tasks.spawn(false, |w| {
            let hub = w.backend.hub_status();
            let connected = hub.connected;
            w.send(Msg::Hub(hub));
            if connected {
                w.send(Msg::Teams(w.backend.teams()));
            }
        });
    }

    pub fn refresh_all(&mut self) {
        self.refresh_rows();
        self.refresh_groups();
        self.refresh_kernel();
        self.refresh_teams();
        self.last_slow = Instant::now();
    }

    pub fn apply_refresh(&mut self, r: Refresh) {
        if r.rows {
            self.refresh_rows();
        }
        if r.groups {
            self.refresh_groups();
        }
        if r.teams {
            self.refresh_teams();
        }
        if r.kernel {
            self.refresh_kernel();
        }
    }

    pub fn go(&mut self, page: Page) {
        self.page = page;
        match page {
            Page::Kernel => self.refresh_kernel(),
            Page::Extensions => self.refresh_extensions(),
            Page::Teams => self.refresh_teams(),
            Page::Logs => self.logs_ui.mark_dirty(),
            _ => {}
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // small helpers

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { kind, text: text.into(), born: Instant::now() });
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
        self.ctx.request_repaint();
    }

    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.id() == id)
    }

    pub fn name_of(&self, id: &str) -> String {
        self.row(id).map(|r| r.profile.name.clone()).unwrap_or_else(|| id.to_owned())
    }

    pub fn open_opts(&self) -> OpenOpts {
        OpenOpts { headless: self.settings.headless, sandbox: self.settings.sandbox }
    }

    pub fn open_count(&self) -> usize {
        self.rows.iter().filter(|r| r.run.is_open()).count()
    }

    /// A fire-and-forget backend call that ends in a toast and a refresh.
    pub fn run_op<F>(&mut self, ids: Vec<String>, ok: String, refresh: Refresh, f: F)
    where
        F: FnOnce(&dyn Backend) -> BResult<()> + Send + 'static,
    {
        for id in &ids {
            self.busy.insert(id.clone(), Busy::Working);
        }
        self.tasks.spawn(true, move |w| {
            let result = f(w.backend.as_ref()).map_err(|e| e.to_string());
            w.send(Msg::Op(OpDone { ids, ok, result, refresh }));
        });
    }

    pub fn open_profiles(&mut self, ids: Vec<String>) {
        for id in ids {
            let Some(row) = self.row(&id) else { continue };
            let name = row.profile.name.clone();
            match &row.run {
                RunState::Open { .. } | RunState::Opening => continue,
                RunState::LockedElsewhere { user, device, since } => {
                    let when = timefmt::since(self.now, *since, self.offset);
                    let text = format!("{name} is open on {device} by {user} since {when}");
                    self.toast(ToastKind::Warn, text);
                    continue;
                }
                _ => {}
            }
            if self.busy.contains_key(&id) {
                continue;
            }
            self.busy.insert(id.clone(), Busy::Opening);
            self.last_error.remove(&id);
            let opts = self.open_opts();
            self.tasks.spawn(true, move |w| {
                let result = w.backend.open(&id, &opts);
                w.send(Msg::Opened { id, name, result });
            });
        }
    }

    pub fn close_profiles(&mut self, ids: Vec<String>) {
        for id in ids {
            let Some(row) = self.row(&id) else { continue };
            if !row.run.is_open() || self.busy.contains_key(&id) {
                continue;
            }
            let name = row.profile.name.clone();
            self.busy.insert(id.clone(), Busy::Closing);
            self.tasks.spawn(true, move |w| {
                let result = w.backend.close(&id);
                w.send(Msg::Closed { id, name, result });
            });
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // messages from the worker threads

    pub(super) fn pump(&mut self) {
        self.now = timefmt::now_ts();
        for m in self.tasks.drain() {
            self.handle(m);
        }
        if self.last_poll.elapsed() >= POLL_ROWS {
            self.last_poll = Instant::now();
            self.refresh_rows();
            self.refresh_groups();
        }
        if self.last_slow.elapsed() >= POLL_SLOW {
            self.last_slow = Instant::now();
            self.refresh_kernel();
            self.refresh_teams();
            self.check_updates(false);
        }
        self.toasts.retain(|t| t.born.elapsed() < t.life());
        if self.rows_loaded
            && let Some(sc) = self.scenario.take()
        {
            self.run_scenario(sc);
        }
        self.ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// The page a screenshot (or `--page`) asks for, beyond just selecting the sidebar entry.
    fn run_scenario(&mut self, sc: StartPage) {
        let demo = self.info.kind == BackendKind::Demo;
        let shooting = self.shot.is_some();
        match sc {
            StartPage::Profiles if shooting && demo => {
                self.quiet_failures = true;
                for id in std::mem::take(&mut self.demo_failing) {
                    self.open_profiles(vec![id]);
                }
                let ids: Vec<String> = self.rows.iter().skip(1).take(2).map(|r| r.id().to_owned()).collect();
                for id in ids {
                    self.profiles.selection.toggle(&id);
                }
            }
            StartPage::Edit => {
                let target = self
                    .rows
                    .iter()
                    .find(|r| !r.profile.user_proxy_config.is_direct())
                    .or(self.rows.first())
                    .map(|r| r.id().to_owned());
                if let Some(id) = target {
                    self.open_edit(Some(id));
                }
            }
            StartPage::New => self.open_edit(None),
            StartPage::Export => {
                self.refresh_view();
                let ids: Vec<String> = self.rows.iter().take(3).map(|r| r.id().to_owned()).collect();
                for id in ids {
                    self.profiles.selection.toggle(&id);
                }
                self.open_export();
            }
            StartPage::Error => {
                if let Some(id) = self.demo_failing.first().cloned() {
                    self.open_profiles(vec![id]);
                }
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------------------------------------------------
    // shell

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        let modal = self.edit.is_some()
            || self.export.is_some()
            || self.confirm.is_some()
            || self.move_dlg.is_some()
            || self.remark_dlg.is_some()
            || self.update.dialog
            || !self.failures.is_empty();
        if modal {
            return;
        }
        let (new, find, refresh) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::N),
                i.consume_key(Modifiers::COMMAND, Key::F),
                i.consume_key(Modifiers::NONE, Key::F5),
            )
        });
        if new {
            self.go(Page::Profiles);
            self.open_edit(None);
        }
        if find {
            self.go(Page::Profiles);
            self.profiles.focus_search = true;
        }
        if refresh {
            self.refresh_all();
            self.logs_ui.mark_dirty();
        }
        if self.page == Page::Profiles && !ctx.egui_wants_keyboard_input() {
            self.profile_shortcuts(ctx);
        }
    }
}

impl eframe::App for CitroApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump();
        self.shortcuts(&ctx);
        self.sidebar(ui);
        self.top_bar(ui);
        let p = self.pal;
        egui::CentralPanel::default_margins()
            .frame(Frame::NONE.fill(p.bg).inner_margin(Margin { left: 12, right: 12, top: 0, bottom: 12 }))
            .show(ui, |ui| {
                // one big rounded card holds the page
                Frame::NONE
                    .fill(p.surface)
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin { left: 24, right: 24, top: 6, bottom: 14 })
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        if self.edit.is_some() {
                            self.edit_page(ui);
                            return;
                        }
                        match self.page {
                            Page::Profiles => self.profiles_page(ui),
                            Page::Groups => self.groups_page(ui),
                            Page::Logs => self.logs_page(ui),
                            Page::Kernel => self.kernel_page(ui),
                            Page::Extensions => self.extensions_page(ui),
                            Page::Teams => self.teams_page(ui),
                            Page::Settings => self.settings_page(ui),
                        }
                    });
            });
        self.guard_quit(&ctx);
        self.dialogs(&ctx);
        self.toasts_ui(&ctx);
        self.tick_shot(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // a safety net: whatever team browser is still supervised is closed and uploaded before the process ends
        // (instant when there is none)
        if self.quit != Quit::Done {
            for line in self.backend.shutdown() {
                self.backend.record(crate::backend::Level::Info, None, &format!("quit: {line}"));
            }
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // A screenshot must not show a stray hover highlight from wherever the X pointer happens to rest.
        if self.shot.is_some() {
            raw_input.events.push(egui::Event::PointerGone);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.pal.bg;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}
