//! Running the hub: bind, serve, flush deferred writes, shut down gracefully.
//!
//! * [`run_blocking`] is what `citro-hub serve` calls: it serves until Ctrl-C / SIGTERM.
//! * [`Hub`] is the same server on its own thread, with the address it really got (`127.0.0.1:0` works) and a
//!   `stop()` that returns once the data directory is free again. The tests, and anything that embeds a hub, use it.

use super::http::router;
use super::store::{Config, Store};
use crate::model::LogLevel;
use std::future::Future;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::{Notify, oneshot};

/// After a shutdown request, connections that are still busy (an upload) get this long to finish.
const GRACE: Duration = Duration::from_secs(10);

fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    let l = TcpListener::bind(addr).map_err(|e| {
        io::Error::new(e.kind(), format!("cannot listen on {addr}: {e} (is another citro-hub or program using it?)"))
    })?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Serve `store` on `listener` until `shutdown` completes, then write the state to disk.
pub async fn serve(
    store: Arc<Store>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let app = router(store.clone()).into_make_service_with_connect_info::<SocketAddr>();

    // Lease extensions are only marked dirty by the state layer; this is what writes them out.
    let flusher = {
        let (store, every) = (store.clone(), store.config().flush_interval.max(Duration::from_millis(50)));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                let s = store.clone();
                match tokio::task::spawn_blocking(move || s.flush()).await {
                    Ok(Err(e)) => tracing::error!("cannot write state.json: {e}"),
                    Err(e) => tracing::error!("flush task failed: {e}"),
                    Ok(Ok(())) => {}
                }
            }
        })
    };

    let stop = Arc::new(Notify::new());
    let on_stop = stop.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app).with_graceful_shutdown(async move { on_stop.notified().await }).await
    });
    let result = tokio::select! {
        r = &mut server => r.map_err(io::Error::other).and_then(|r| r),
        () = shutdown => {
            stop.notify_one();
            match tokio::time::timeout(GRACE, &mut server).await {
                Ok(r) => r.map_err(io::Error::other).and_then(|r| r),
                Err(_) => {
                    tracing::warn!("connections still busy after {GRACE:?}; dropping them");
                    server.abort();
                    Ok(())
                }
            }
        }
    };
    flusher.abort();
    let s = store.clone();
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || s.flush()).await {
        tracing::error!("cannot write state.json at shutdown: {e}");
    }
    result
}

async fn signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
}

/// Open the data directory and serve until Ctrl-C or SIGTERM. Returns once everything is on disk.
pub fn run_blocking(dir: &Path, cfg: Config) -> io::Result<()> {
    let store = Arc::new(Store::open(dir, cfg)?);
    let listener = bind(store.config().bind)?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let addr = listener.local_addr()?;
        tracing::info!("citro-hub {} serves {} on http://{addr}", env!("CARGO_PKG_VERSION"), store.dir().display());
        let s = store.clone();
        let msg = format!("citro-hub {} started on {addr}", env!("CARGO_PKG_VERSION"));
        let _ = tokio::task::spawn_blocking(move || s.log_server(LogLevel::Info, None, &msg)).await;
        let r = serve(store, listener, signal()).await;
        tracing::info!("citro-hub stopped");
        r
    })
}

/// A hub running on its own thread. Dropping it (or [`Hub::stop`]) shuts it down gracefully and releases the data
/// directory, so another hub can be started on the same directory afterwards.
pub struct Hub {
    addr: SocketAddr,
    store: Arc<Store>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Hub {
    /// Open `dir` and start serving on `cfg.bind` (port 0 picks a free port; see [`Hub::addr`]).
    pub fn start(dir: &Path, cfg: Config) -> io::Result<Hub> {
        let store = Arc::new(Store::open(dir, cfg)?);
        let listener = bind(store.config().bind)?;
        let addr = listener.local_addr()?;
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
        let st = store.clone();
        let thread = std::thread::Builder::new().name("citro-hub".to_owned()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                if let Err(e) = serve(st, listener, async move {
                    let _ = stop_rx.await;
                })
                .await
                {
                    tracing::error!("the hub stopped with an error: {e}");
                }
            });
        })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Hub { addr, store, stop: Some(stop_tx), thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::other("the hub thread died while starting")),
        }
    }

    /// The address the hub really listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:PORT`, what a client is pointed at.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Shut down gracefully and wait until the state is on disk and the data directory is free.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
