//! OpenMaker Server's Admin Console: administering a running server from a
//! browser, with FileMaker Server's tabs, URLs, page structure and visual
//! system (`docs/admin-console.md`; FileMaker Server's own console is
//! described in `docs/re/admin-console.md`).
//!
//! The crate does not know `om-server`. It reads the server's state from a
//! [`Backend`] and sends it the administrator's actions; [`MockBackend`] is
//! an in-memory one for tests and design work.
//!
//! [`Console::start`] serves the console on a thread of its own with a
//! small tokio runtime, so a host's own threads are untouched, and returns
//! a [`ConsoleHandle`] that stops it. It serves HTTPS only (HTTP/1.1):
//!
//! - sign-in for one administrator account, with a lockout per address
//!   after repeated failures (`auth`);
//! - sessions in an `HttpOnly`, `Secure`, `SameSite=Strict` cookie, a CSRF
//!   token in every form and an `Origin` check on every state-changing
//!   request (`session`, `app`);
//! - pages rendered on the server with `maud`, with a style sheet, a small
//!   script and icons of OpenMaker's own (`pages`, `assets`, `icons`), and
//!   charts drawn as SVG (`chart`). Every action is a form post, so the
//!   console works without the script;
//! - the Dashboard's system statistics, which a sampler on the console's
//!   runtime reads from the operating system once a minute (`stats`);
//! - FileMaker Server's Admin API v2 under `/fmi/admin/api/v2`, with bearer
//!   tokens opened by the administrator's credentials (`api`).

mod api;
mod app;
mod assets;
mod auth;
mod backend;
mod certificate;
mod chart;
mod components;
mod icons;
mod listener;
mod mock;
mod nav;
mod pages;
mod session;
mod stats;
mod views;

use std::fmt;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use log::{error, info};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::watch;

pub use auth::PasswordDigest;
pub use backend::{
    Alert, Backend, BackendError, Backup, BackupPlan, Backups, Client, ClientFile, Database,
    DatabaseChange, DatabaseStatus, Folders, LogEntries, LogEntry, LogFile, LogPage, Notice,
    Schedule, ScheduleKind, ScheduleRun, ServerInfo, Severity,
};
pub use mock::{MockBackend, MockState};

/// What the console needs to run.
pub struct ConsoleConfig {
    /// Where it listens (`om-server --admin-bind`).
    pub bind: SocketAddr,
    /// The server certificate chain it presents, leaf first.
    pub certificates: Vec<CertificateDer<'static>>,
    /// The certificate's private key.
    pub key: PrivateKeyDer<'static>,
    /// The administrator's user name (`--admin-user`).
    pub user: String,
    /// The administrator's password, as its digest (`"secret".into()`): the
    /// console never holds the text.
    pub password: PasswordDigest,
}

impl fmt::Debug for ConsoleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsoleConfig")
            .field("bind", &self.bind)
            .field("certificates", &self.certificates.len())
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// The Admin Console.
pub struct Console;

impl Console {
    /// Bind `config.bind` and serve the console on a thread of its own until
    /// [`ConsoleHandle::shutdown`]. Binding and the TLS configuration are
    /// checked before this returns.
    pub fn start(config: ConsoleConfig, backend: Arc<dyn Backend>) -> io::Result<ConsoleHandle> {
        let tls = listener::tls_config(config.certificates, config.key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let tcp = TcpListener::bind(config.bind)?;
        tcp.set_nonblocking(true)?;
        let addr = tcp.local_addr()?;
        let state = app::AppState::new(backend.clone(), &config.user, config.password);
        let statistics = state.statistics().clone();
        let router = app::router(state);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("om-admin")
            .enable_io()
            .enable_time()
            .build()?;
        let (stop, stopped) = watch::channel(false);
        runtime.spawn(stats::run(statistics, backend, stopped.clone()));
        let thread = std::thread::Builder::new()
            .name("om-admin".into())
            .spawn(move || {
                let served = runtime.block_on(async move {
                    let tcp = tokio::net::TcpListener::from_std(tcp)?;
                    info!("Admin Console listening on https://{addr}/admin-console");
                    listener::serve(tcp, tls, router, stopped).await;
                    io::Result::Ok(())
                });
                if let Err(e) = served {
                    error!("Admin Console: {e}");
                }
                // `serve` has already ended its connections.
                runtime.shutdown_timeout(Duration::from_millis(100));
            })?;
        Ok(ConsoleHandle {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

/// A running console.
pub struct ConsoleHandle {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl ConsoleHandle {
    /// The address the console listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting connections and wait for the console's thread. Idle
    /// connections close at once; requests being answered get a second to
    /// finish before their connections close too.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ConsoleHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
