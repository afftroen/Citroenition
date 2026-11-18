//! Worker threads. Every blocking backend call (open, close, proxy check, CSV write, kernel install ...) runs on its
//! own short-lived thread and reports back through one channel that the UI drains once per frame, so the frame loop
//! never waits for the backend.

use crate::backend::{
    BResult, Backend, HubStatus, KernelStatus, LogLine, Notice, OpenError, ProxyCheck, Row, RunInfo, TeamInfo,
};
use citro_core::profile::{Group, Profile};
use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Which cached data a finished operation invalidates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    pub rows: bool,
    pub groups: bool,
    pub teams: bool,
    pub kernel: bool,
}

impl Refresh {
    pub const NONE: Refresh = Refresh { rows: false, groups: false, teams: false, kernel: false };
    pub const ROWS: Refresh = Refresh { rows: true, ..Refresh::NONE };
    pub const ROWS_GROUPS: Refresh = Refresh { rows: true, groups: true, ..Refresh::NONE };
    pub const TEAMS: Refresh = Refresh { rows: true, teams: true, ..Refresh::NONE };
}

/// A finished fire-and-forget operation (delete, move, remark, force unlock ...).
#[derive(Debug)]
pub struct OpDone {
    /// Profiles that were marked busy while it ran.
    pub ids: Vec<String>,
    /// Toast text on success.
    pub ok: String,
    pub result: Result<(), String>,
    pub refresh: Refresh,
}

pub enum Msg {
    Rows(BResult<Vec<Row>>),
    Groups(BResult<Vec<Group>>),
    Opened {
        id: String,
        name: String,
        result: Result<RunInfo, OpenError>,
    },
    Closed {
        id: String,
        name: String,
        result: BResult<()>,
    },
    Op(OpDone),
    Saved {
        result: BResult<Profile>,
        created: bool,
    },
    GroupCreated(BResult<Group>),
    FullProfile {
        id: String,
        result: BResult<Profile>,
    },
    ProxyChecked {
        token: u64,
        result: BResult<ProxyCheck>,
    },
    /// The editor's fingerprint, read from the browser.
    Probed {
        key: String,
        result: BResult<citro_core::probe::Probe>,
    },
    /// The cookies of an open browser, for the editor.
    LiveCookies {
        id: String,
        result: BResult<String>,
    },
    ProxyDialogLoaded {
        id: String,
        result: BResult<citro_core::profile::Profile>,
    },
    ProxyDialogChecked {
        token: u64,
        result: BResult<ProxyCheck>,
    },
    /// A proxy checked from the profile list.
    RowProxyChecked {
        id: String,
        result: BResult<ProxyCheck>,
    },
    Exported {
        path: String,
        rows: usize,
        result: Result<(), String>,
    },
    Logs {
        token: u64,
        result: BResult<Vec<LogLine>>,
    },
    Kernel(BResult<KernelStatus>),
    KernelProgress {
        done: u64,
        total: u64,
    },
    KernelInstalled(BResult<KernelStatus>),
    Teams(BResult<Vec<TeamInfo>>),
    Hub(HubStatus),
    HubConnected(BResult<HubStatus>),
    HubDisconnected(BResult<HubStatus>),
    /// The sentence a pull of the team's profiles produced.
    Pulled(BResult<String>),
    /// Things that happened by themselves (a team browser closed by its window and uploaded, a lost lock ...).
    Notices(Vec<Notice>),
    /// The team browsers are closed and uploaded: the app may quit.
    QuitDone(Vec<String>),
    Extensions(BResult<Vec<crate::backend::Extension>>),
    Plugins(BResult<Vec<citro_core::plugins::Plugin>>),
    TeamPeople(Result<Option<citro_sync::People>, String>),
    HubUpgrade(BResult<Option<crate::backend::HubUpgrade>>),
    /// A token to hand to someone: a fresh invite (`invited`) or a replacement.
    MemberToken {
        name: String,
        invited: bool,
        result: BResult<String>,
    },
    PluginFetched(BResult<crate::backend::PluginPreview>),
    PluginInstalled(BResult<citro_core::plugins::Plugin>),
    PluginNewest {
        id: String,
        result: BResult<Option<(String, String)>>,
    },
    DataMoved(BResult<()>),
    Tags(BResult<Vec<crate::backend::Tag>>),
    ImportRead(Result<citro_core::import::Parsed, String>),
    ImportProgress(usize, usize),
    Imported(BResult<citro_core::import::Report>),
    ExtensionAdded(BResult<crate::backend::Extension>),
    UpdateChecked(Result<Option<crate::updater::AppRelease>, String>),
    UpdateProgress {
        done: u64,
        total: u64,
    },
    UpdateDone(Result<String, String>),
}

/// What a worker closure gets: the backend and a way to answer.
#[derive(Clone)]
pub struct Worker {
    pub backend: Arc<dyn Backend>,
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Worker {
    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

struct Guard {
    all: Arc<AtomicUsize>,
    loud: Option<Arc<AtomicUsize>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.all.fetch_sub(1, Ordering::SeqCst);
        if let Some(l) = &self.loud {
            l.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

pub struct Tasks {
    worker: Worker,
    rx: Receiver<Msg>,
    all: Arc<AtomicUsize>,
    loud: Arc<AtomicUsize>,
}

impl Tasks {
    pub fn new(ctx: egui::Context, backend: Arc<dyn Backend>) -> Tasks {
        let (tx, rx) = channel();
        Tasks {
            worker: Worker { backend, tx, ctx },
            rx,
            all: Arc::new(AtomicUsize::new(0)),
            loud: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// `loud` tasks light up the activity spinner in the status bar; background polls do not.
    pub fn spawn(&self, loud: bool, f: impl FnOnce(&Worker) + Send + 'static) {
        let w = self.worker.clone();
        self.all.fetch_add(1, Ordering::SeqCst);
        if loud {
            self.loud.fetch_add(1, Ordering::SeqCst);
        }
        let guard = Guard { all: self.all.clone(), loud: loud.then(|| self.loud.clone()) };
        // when the thread cannot be started the closure (and the guard in it) is dropped and the counters recover
        let _ = std::thread::Builder::new().name("citro-task".into()).spawn(move || {
            let _guard = guard;
            f(&w);
        });
    }

    pub fn drain(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }

    /// Something the user started is still running.
    pub fn busy(&self) -> bool {
        self.loud.load(Ordering::SeqCst) > 0
    }

    /// Nothing at all is running (used by the screenshot harness).
    pub fn idle(&self) -> bool {
        self.all.load(Ordering::SeqCst) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Query;
    use crate::backend::mock::MockBackend;
    use std::time::{Duration, Instant};

    #[test]
    fn a_worker_answers_through_the_channel_and_the_counters_recover() {
        let backend: Arc<dyn Backend> = Arc::new(MockBackend::demo().with_latency(Duration::ZERO));
        let tasks = Tasks::new(egui::Context::default(), backend);
        tasks.spawn(true, |w| w.send(Msg::Rows(w.backend.list_profiles(&Query::default()))));
        assert!(tasks.busy() || tasks.idle());
        let start = Instant::now();
        let mut got = None;
        while got.is_none() && start.elapsed() < Duration::from_secs(5) {
            got = tasks.drain().into_iter().next();
            std::thread::sleep(Duration::from_millis(5));
        }
        let Some(Msg::Rows(Ok(rows))) = got else { panic!("no rows message") };
        assert!(rows.len() >= 10);
        while !tasks.idle() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(tasks.idle() && !tasks.busy());
    }
}
