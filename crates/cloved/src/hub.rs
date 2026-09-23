//! The hub (DESIGN §8.1): one daemon serving every project of a user, tied to
//! none of them.
//!
//! Projects are [`Slot`]s, keyed by their canonical `.clove/` directory. Every
//! project-scoped call names its caller's own project; the hub resolves it to
//! a slot per call, loading it when the call allows. A slot is evicted when it
//! idles out, detached on request, and unloaded — alone — when any of its tasks
//! ends or panics. The hub exits once it has served nothing for a grace period.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use clove_ipc::hub::{client_is_this_user, codes, frame, recv_frame, send_frame, Hello, Welcome};
use clove_ipc::{
    transport_from_framed, CloveRpc, Detached, GraphRequest, GraphResponse, HubStatus, Project,
    ProjectInfo, QueryListResponse, QueryRequest, ReindexDone, RpcError, StatusResponse,
    PROTOCOL_VERSION,
};
use clove_types::{EditRequest, ItemStatus, NewSpec};
use futures::StreamExt;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use serde_json::Value;
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel};
use tokio::sync::OnceCell;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::ipc::Dispatcher;
use crate::slot::{self, LoadError, Slot, SlotTask};

/// How long a fresh connection may take to send its [`Hello`].
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long hub shutdown waits for each slot's teardown.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long before a call's deadline the hub stops waiting, so its answer
/// still reaches the caller in time.
const REPLY_MARGIN: Duration = Duration::from_millis(250);

/// The hub's shared web listener.
struct WebSite {
    web: clove_web::HubWeb,
    addr: SocketAddr,
}

/// A cheap handle to the hub; clones share it.
#[derive(Clone)]
pub struct Hub(Arc<Inner>);

/// One project in the table: its slot once loaded, and whether it has been
/// asked to stop — set by a detach even while the load is still under way,
/// so the load tears itself down when it completes.
#[derive(Default)]
struct Entry {
    slot: OnceCell<Arc<Slot>>,
    stopping: AtomicBool,
}

type SlotCell = Arc<Entry>;

/// `deadline`, less the time the answer needs to get back.
fn answer_by(deadline: Instant) -> tokio::time::Instant {
    tokio::time::Instant::from_std(deadline.checked_sub(REPLY_MARGIN).unwrap_or(deadline))
}

/// The slot table, and whether the hub has decided to exit. Both live under
/// one lock so that deciding to exit and admitting a new project cannot
/// interleave: once `exiting` is set no load is admitted, and it is only set
/// while the table is empty.
#[derive(Default)]
struct Table {
    /// One cell per project being loaded or served. A cell is inserted before
    /// the load starts, so concurrent calls for one project share one load
    /// instead of racing for its lock.
    slots: HashMap<Utf8PathBuf, SlotCell>,
    exiting: bool,
    /// The last project was stopped while its teardown was still running:
    /// exit as soon as the table is empty, unless a project is admitted first.
    exit_when_empty: bool,
}

struct Inner {
    table: Mutex<Table>,
    /// Whether the hub serves the web UI at all (`CLOVED_DISABLE_WEB` unset).
    web_enabled: bool,
    /// The one web listener, bound when the first web-enabled project loads.
    /// `Some(None)` once a bind has failed: the hub then serves no web UI.
    web: OnceCell<Option<WebSite>>,
    started: Instant,
    shutdown: CancellationToken,
    grace: Duration,
    /// Extra time every load takes once it holds the project's lock — a test
    /// knob standing in for a large project (`CLOVED_LOAD_DELAY_MS`).
    load_delay: Duration,
}

fn refused(code: &'static str, message: impl Into<String>) -> LoadError {
    LoadError {
        code,
        message: message.into(),
    }
}

impl From<LoadError> for RpcError {
    fn from(e: LoadError) -> RpcError {
        RpcError::new(e.code, e.message)
    }
}

impl Hub {
    pub fn new(web_enabled: bool, grace: Duration) -> Hub {
        Hub(Arc::new(Inner {
            table: Mutex::new(Table::default()),
            web_enabled,
            web: OnceCell::new(),
            started: Instant::now(),
            shutdown: CancellationToken::new(),
            grace,
            load_delay: Duration::ZERO,
        }))
    }

    /// Make every load take `delay` longer, holding the project's lock.
    pub fn with_load_delay(self, delay: Duration) -> Hub {
        let inner = Arc::try_unwrap(self.0).unwrap_or_else(|_| panic!("configured after use"));
        Hub(Arc::new(Inner {
            load_delay: delay,
            ..inner
        }))
    }

    /// Cancelled when the hub should exit.
    pub fn shutdown(&self) -> &CancellationToken {
        &self.0.shutdown
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.0.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The hub's key for a project path: the path itself when it already names
    /// a slot (clients send canonical paths), else its canonical form —
    /// resolved on the blocking pool, off the two async workers.
    async fn key(&self, clove_dir: &str) -> Result<Utf8PathBuf, LoadError> {
        if !Utf8Path::new(clove_dir).is_absolute() {
            return Err(refused(
                codes::BAD_PROJECT,
                format!("project path {clove_dir:?} is not absolute"),
            ));
        }
        let as_given = Utf8PathBuf::from(clove_dir);
        if self.table().slots.contains_key(&as_given) {
            return Ok(as_given);
        }
        let owned = clove_dir.to_owned();
        tokio::task::spawn_blocking(move || slot::canonical_key(&owned))
            .await
            .unwrap_or_else(|e| Err(refused(codes::LOAD_FAILED, e.to_string())))
    }

    /// Admit a call for the project at `key` only with that project's token.
    async fn authorize(&self, key: &Utf8Path, token: &str) -> Result<(), LoadError> {
        let (key, token) = (key.to_owned(), token.to_owned());
        tokio::task::spawn_blocking(move || crate::token::check(&key, &token))
            .await
            .unwrap_or_else(|e| Err(refused(codes::BAD_TOKEN, e.to_string())))
    }

    /// The caller's project's slot, loading it first when the call allows.
    ///
    /// A slot caught mid-teardown (a detach, an idle eviction) is gone for a
    /// probe; a load waits for the teardown — however long it takes, up to the
    /// caller's own `deadline` — and then loads the project afresh.
    pub async fn attach(
        &self,
        project: &Project,
        deadline: Instant,
    ) -> Result<Arc<Slot>, LoadError> {
        let clove_dir = project.clove_dir.as_str();
        loop {
            let slot = self.attach_once(project).await?;
            if !slot.cancel.is_cancelled() {
                return Ok(slot);
            }
            if !project.load {
                return Err(refused(
                    codes::NOT_LOADED,
                    format!("the daemon is not serving {clove_dir}"),
                ));
            }
            let torn_down = tokio::time::timeout_at(answer_by(deadline), async {
                slot.done.cancelled().await;
                // Past `done` the slot is out of the table; give the loop a
                // beat rather than spin should it still be handed back.
                tokio::time::sleep(Duration::from_millis(10)).await;
            })
            .await;
            if torn_down.is_err() {
                return Err(refused(
                    codes::NOT_LOADED,
                    format!(
                        "the daemon is still stopping {clove_dir} (flushing its index); \
                         try again once it has"
                    ),
                ));
            }
        }
    }

    async fn attach_once(&self, project: &Project) -> Result<Arc<Slot>, LoadError> {
        let (clove_dir, load) = (project.clove_dir.as_str(), project.load);
        let not_loaded = || {
            refused(
                codes::NOT_LOADED,
                format!("the daemon is not serving {clove_dir}"),
            )
        };
        let key = match self.key(clove_dir).await {
            Ok(key) => key,
            Err(e) if e.code == codes::BAD_PROJECT => return Err(e),
            Err(_) if !load => return Err(not_loaded()),
            Err(e) => return Err(e),
        };
        self.authorize(&key, &project.token).await?;
        let cell: SlotCell = {
            let mut table = self.table();
            if table.exiting {
                return Err(refused(
                    codes::SHUTTING_DOWN,
                    "the daemon is shutting down; start it again once it has exited",
                ));
            }
            if load {
                table.exit_when_empty = false;
                table.slots.entry(key.clone()).or_default().clone()
            } else {
                table.slots.get(&key).cloned().ok_or_else(not_loaded)?
            }
        };
        // Whether this call came to a project already asked to stop (and so
        // waits it out) or was itself cut off by that stop.
        let stopping_on_arrival = cell.stopping.load(Ordering::SeqCst);
        let slot = if load {
            // On its own task: a caller that goes away mid-load (its call
            // cancelled) must not abort the load halfway, leaving the
            // project's lock held by an orphaned open that the next load
            // then finds taken. Every load runs to completion and is started.
            let loading = tokio::spawn(self.clone().load(key.clone(), cell.clone()));
            loading.await.unwrap_or_else(|e| {
                Err(refused(
                    codes::LOAD_FAILED,
                    format!("loading the project panicked: {e}"),
                ))
            })?
        } else {
            // Still loading counts as not loaded: a probe must not wait on it.
            let slot = cell.slot.get().cloned().ok_or_else(not_loaded)?;
            slot.started.get_or_init(|| self.start(&slot)).await;
            slot
        };
        // The hub drained while it was loading: nothing tracks it any more, so
        // tear it down rather than let it serve unlisted.
        let registered = self
            .table()
            .slots
            .get(&key)
            .is_some_and(|c| Arc::ptr_eq(c, &cell));
        if !registered {
            slot.cancel.cancel();
        }
        // Stopped while this call was loading it: the stop came later, so it
        // wins, and this call says so rather than load the project again.
        if load && !stopping_on_arrival && cell.stopping.load(Ordering::SeqCst) {
            return Err(refused(
                codes::NOT_LOADED,
                format!("{clove_dir} was stopped while it was loading"),
            ));
        }
        Ok(slot)
    }

    /// Load `key` into `cell` — or wait for the load already under way — and
    /// start it.
    async fn load(self, key: Utf8PathBuf, cell: SlotCell) -> Result<Arc<Slot>, LoadError> {
        let loaded = cell
            .slot
            .get_or_try_init(|| {
                let key = key.clone();
                let cancel = self.0.shutdown.child_token();
                let delay = self.0.load_delay;
                async move {
                    tokio::task::spawn_blocking(move || {
                        let opened = slot::open(&key, cancel);
                        if opened.is_ok() && !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        opened
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(refused(
                            codes::LOAD_FAILED,
                            format!("loading the project panicked: {e}"),
                        ))
                    })
                    .map(Arc::new)
                }
            })
            .await
            .cloned();
        let slot = match loaded {
            Ok(slot) => slot,
            Err(e) => {
                eprintln!("cloved: could not serve {key}: {}", e.message);
                let mut table = self.table();
                if table
                    .slots
                    .get(&key)
                    .is_some_and(|c| Arc::ptr_eq(c, &cell) && c.slot.get().is_none())
                {
                    table.slots.remove(&key);
                }
                return Err(e);
            }
        };
        // Asked to stop while loading: started only to be torn down at once,
        // which releases the lock and takes it out of the table.
        if cell.stopping.load(Ordering::SeqCst) {
            slot.cancel.cancel();
        }
        slot.started.get_or_init(|| self.start(&slot)).await;
        Ok(slot)
    }

    /// The project-scoped half of a call: the caller's project, resolved to
    /// its slot's dispatcher.
    async fn dispatcher(
        &self,
        project: &Project,
        deadline: Instant,
    ) -> Result<Dispatcher, RpcError> {
        Ok(self.attach(project, deadline).await?.dispatcher.clone())
    }

    /// Mount the slot on the web listener and start its supervised tasks.
    async fn start(&self, slot: &Arc<Slot>) {
        eprintln!("cloved: serving {}", slot.clove_dir);
        let site = match slot.settings.web_enabled && self.0.web_enabled {
            true => self.web_site(slot.settings.web_port).await,
            false => None,
        };
        if let Some(site) = site {
            let state = Arc::clone(&slot.dispatcher.state);
            let heartbeat: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Ok(mut s) = state.lock() {
                    s.mark_event();
                }
            });
            let app = clove_web::AppState::new(
                clove_core::ItemStore::new(slot.repo_root.clone()),
                slot.dispatcher.issues_dir.clone(),
                slot.settings.id_prefix.clone(),
                "daemon",
                true,
                slot.settings.default_type,
            )
            .with_heartbeat(heartbeat);
            // Mounting takes the web registry's lock; the web watcher is
            // armed on a thread of its own, so the load does not wait for it.
            let (web, root) = (site.web.clone(), slot.repo_root.clone());
            let Ok(slug) = tokio::task::spawn_blocking(move || web.mount(&root, app)).await else {
                tokio::spawn(self.clone().supervise(Arc::clone(slot), slot.tasks()));
                return;
            };
            if let Ok(mut s) = slot.dispatcher.state.lock() {
                s.set_web(
                    Some(site.addr.to_string()),
                    Some(format!("http://{}/p/{slug}/", site.addr)),
                );
            }
            *slot.web_slug.lock().unwrap_or_else(|e| e.into_inner()) = Some(slug);
        }
        tokio::spawn(self.clone().supervise(Arc::clone(slot), slot.tasks()));
    }

    /// The shared web listener, binding it on first use.
    ///
    /// The port is `CLOVED_WEB_PORT`, else the `[web] port` of the first project
    /// that needs it — a per-user daemon has no config of its own, and for the
    /// common single-project user that keeps `[web] port` meaning what it says.
    /// A taken port falls back to a free one, which each project's `STATUS`
    /// advertises.
    async fn web_site(&self, project_port: u16) -> Option<&WebSite> {
        self.0
            .web
            .get_or_init(|| async move {
                let port = std::env::var("CLOVED_WEB_PORT")
                    .ok()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(project_port);
                let wanted: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
                let listener = match clove_web::bind_or_free_port(wanted).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        eprintln!("cloved: web server bind error ({wanted}): {e}");
                        return None;
                    }
                };
                let addr = listener.local_addr().unwrap_or(wanted);
                if port != 0 && addr.port() != port {
                    eprintln!("cloved: web UI port {wanted} in use; serving on {addr}");
                }
                let web = clove_web::HubWeb::new();
                let server = web.clone();
                tokio::spawn(async move {
                    if let Err(e) = server.serve(listener).await {
                        eprintln!("cloved: web server error ({addr}): {e}");
                    }
                });
                Some(WebSite { web, addr })
            })
            .await
            .as_ref()
    }

    /// Run until the slot is cancelled or one of its tasks ends, then tear down
    /// that slot only. A panic in one project's watcher is that project's
    /// problem; every other slot keeps serving.
    async fn supervise(self, slot: Arc<Slot>, mut tasks: JoinSet<SlotTask>) {
        let ended = tokio::select! {
            _ = slot.cancel.cancelled() => None,
            joined = tasks.join_next() => joined,
        };
        match ended {
            None => {}
            Some(Ok(SlotTask::Idle)) => {
                eprintln!("cloved: {} idle; unloading it", slot.clove_dir);
            }
            Some(Ok(task)) => {
                eprintln!(
                    "cloved: {}: {task:?} stopped; unloading the project",
                    slot.clove_dir
                );
            }
            Some(Err(e)) => {
                eprintln!(
                    "cloved: {}: a background task failed ({e}); unloading the project",
                    slot.clove_dir
                );
            }
        }
        slot.cancel.cancel();
        tasks.shutdown().await;
        // The WAL checkpoint is SQLite I/O: keep it off the async workers.
        let flushing = Arc::clone(&slot);
        let _ = tokio::task::spawn_blocking(move || {
            flushing.checkpoint();
            flushing.release_lock();
        })
        .await;
        // Unmounting takes the web registry's lock (the web watcher is
        // stopped on a thread of its own).
        let (hub, forgotten) = (self.clone(), Arc::clone(&slot));
        let _ = tokio::task::spawn_blocking(move || hub.forget(&forgotten)).await;
        eprintln!("cloved: stopped serving {}", slot.clove_dir);
        slot.done.cancel();
    }

    /// Drop a torn-down slot from the table and the web listener.
    fn forget(&self, slot: &Arc<Slot>) {
        {
            let mut table = self.table();
            let current = table
                .slots
                .get(&slot.clove_dir)
                .and_then(|cell| cell.slot.get())
                .is_some_and(|s| Arc::ptr_eq(s, slot));
            if current {
                table.slots.remove(&slot.clove_dir);
            }
            if table.exit_when_empty && table.slots.is_empty() {
                table.exiting = true;
                self.0.shutdown.cancel();
            }
        }
        let slug = slot
            .web_slug
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let (Some(Some(site)), Some(slug)) = (self.0.web.get(), slug) {
            site.web.unmount(&slug);
        }
    }

    /// Stop serving `project` — also one still loading, whose load then tears
    /// itself down when it completes. Returns once the teardown has run (its
    /// `daemon.lock` released), or, when that takes longer than the caller
    /// waits (`deadline`), says it is still stopping. When nothing is left,
    /// the hub decides — under the table lock, so no concurrent load can slip
    /// in between — to exit.
    pub async fn detach(&self, project: &Project, deadline: Instant) -> Result<Detached, RpcError> {
        let key = self.key(&project.clove_dir).await;
        let entry = match &key {
            Ok(key) => {
                self.authorize(key, &project.token).await?;
                let entry = self.table().slots.get(key).cloned();
                if let Some(entry) = &entry {
                    entry.stopping.store(true, Ordering::SeqCst);
                }
                entry
            }
            Err(e) if e.code == codes::BAD_PROJECT => return Err(e.clone().into()),
            Err(_) => None,
        };
        let (detached, stopping) = match entry {
            None => (false, false),
            Some(entry) => {
                let finished = tokio::time::timeout_at(answer_by(deadline), async {
                    // A load under way sees `stopping` and tears itself down;
                    // wait for it first. (An init that fails at once just
                    // means no load is running.)
                    let slot = entry
                        .slot
                        .get_or_try_init(|| async { Err(()) })
                        .await
                        .ok()
                        .cloned();
                    if let Some(slot) = slot {
                        slot.cancel.cancel();
                        slot.done.cancelled().await;
                    }
                })
                .await;
                (true, finished.is_err())
            }
        };
        let hub_exiting = {
            let mut table = self.table();
            if table.slots.is_empty() {
                table.exiting = true;
            } else if stopping
                && table
                    .slots
                    .values()
                    .all(|e| e.stopping.load(Ordering::SeqCst))
            {
                // Only stopping projects left: the hub goes once they are down.
                table.exit_when_empty = true;
            }
            table.exiting
        };
        if hub_exiting {
            // Let the reply reach the client before the runtime winds down.
            let shutdown = self.0.shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                shutdown.cancel();
            });
        }
        Ok(Detached {
            detached,
            hub_exiting,
            stopping,
        })
    }

    /// The hub and every project it serves.
    pub fn status(&self) -> HubStatus {
        let loaded: Vec<Arc<Slot>> = self
            .table()
            .slots
            .values()
            .filter_map(|cell| cell.slot.get().cloned())
            .collect();
        let mut projects: Vec<ProjectInfo> = loaded
            .iter()
            .filter_map(|slot| {
                let status = slot.dispatcher.state.lock().ok()?.snapshot();
                Some(ProjectInfo {
                    clove_dir: slot.clove_dir.to_string(),
                    status,
                })
            })
            .collect();
        projects.sort_by(|a, b| a.clove_dir.cmp(&b.clove_dir));
        HubStatus {
            pid: std::process::id(),
            uptime_s: self.0.started.elapsed().as_secs(),
            web_addr: self
                .0
                .web
                .get()
                .and_then(Option::as_ref)
                .map(|site| site.addr.to_string()),
            projects,
            token_records: clove_core::daemon_token::records_dir()
                .ok()
                .map(|dir| clove_ipc::absolute(&dir).into_string()),
        }
    }

    /// Tear every slot down (hub shutdown). Admits nothing new from here on.
    pub async fn unload_all(&self) {
        let cells: Vec<SlotCell> = {
            let mut table = self.table();
            table.exiting = true;
            table.slots.drain().map(|(_, cell)| cell).collect()
        };
        for slot in cells.iter().filter_map(|cell| cell.slot.get()) {
            slot.cancel.cancel();
        }
        for slot in cells.iter().filter_map(|cell| cell.slot.get()) {
            let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
        }
    }

    /// Resolve once the hub has served no project for the grace period — the
    /// decision taken under the table lock, like a detach's.
    pub async fn idle_exit(&self) {
        let tick = (self.0.grace / 4).max(Duration::from_millis(25));
        let mut empty_since: Option<Instant> = None;
        loop {
            tokio::time::sleep(tick).await;
            let mut table = self.table();
            if table.slots.is_empty() {
                let since = *empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.0.grace {
                    table.exiting = true;
                    return;
                }
            } else {
                empty_since = None;
            }
        }
    }

    /// Accept connections forever, each on its own task.
    pub async fn accept_loop(&self, listener: Listener) {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    tokio::spawn(self.clone().serve_connection(stream));
                }
                Err(e) => {
                    eprintln!("cloved: accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    /// Check the peer, run the version handshake, then serve calls until
    /// either side closes or the hub exits.
    async fn serve_connection(self, stream: Stream) {
        let mut framed = frame(stream);
        let hello = tokio::time::timeout(HELLO_TIMEOUT, recv_frame::<Hello>(&mut framed)).await;
        // Defence in depth behind the private runtime directory: serve only
        // this user's processes. Checked once the first frame is in — Windows
        // can tell a pipe client's user only after reading from it — and
        // before anything is sent back.
        if !client_is_this_user(framed.get_ref()).unwrap_or(false) {
            return;
        }
        let hello = match hello {
            Ok(Ok(Some(hello))) => hello,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                let _ = send_frame(&mut framed, &refusal(codes::BAD_HELLO, &e.to_string())).await;
                return;
            }
            _ => return,
        };
        if hello.protocol() != PROTOCOL_VERSION {
            let message = format!(
                "client protocol {} != daemon protocol {PROTOCOL_VERSION}",
                hello.protocol()
            );
            let _ = send_frame(&mut framed, &refusal(codes::PROTOCOL_MISMATCH, &message)).await;
            return;
        }
        let ok = Welcome::Ok {
            protocol: PROTOCOL_VERSION,
        };
        if send_frame(&mut framed, &ok).await.is_err() {
            return;
        }
        let shutdown = self.0.shutdown.clone();
        let serve = BaseChannel::with_defaults(transport_from_framed(framed))
            .execute(Service(self).serve())
            .for_each(|response| async move {
                tokio::spawn(response);
            });
        tokio::select! {
            _ = serve => {},
            _ = shutdown.cancelled() => {},
        }
    }
}

fn refusal(code: &str, message: &str) -> Welcome {
    Welcome::Err {
        protocol: PROTOCOL_VERSION,
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

/// The RPC service: resolves each call's project and hands it to that
/// project's [`Dispatcher`].
#[derive(Clone)]
struct Service(Hub);

impl CloveRpc for Service {
    async fn ping(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    async fn hub_status(self, _: Context) -> HubStatus {
        self.0.status()
    }

    async fn attach(self, ctx: Context, project: Project) -> Result<(), RpcError> {
        let slot = self.0.attach(&project, ctx.deadline).await?;
        // The project heartbeat: count it and reset the idle window.
        if let Ok(mut state) = slot.dispatcher.state.lock() {
            state.record_ping();
        }
        Ok(())
    }

    async fn detach(self, ctx: Context, project: Project) -> Result<Detached, RpcError> {
        self.0.detach(&project, ctx.deadline).await
    }

    async fn status(self, ctx: Context, project: Project) -> Result<StatusResponse, RpcError> {
        Ok(self
            .0
            .dispatcher(&project, ctx.deadline)
            .await?
            .status()
            .await)
    }

    async fn change_generation(self, ctx: Context, project: Project) -> Result<u64, RpcError> {
        Ok(self
            .0
            .dispatcher(&project, ctx.deadline)
            .await?
            .change_generation()
            .await)
    }

    async fn query(
        self,
        ctx: Context,
        project: Project,
        req: QueryRequest,
    ) -> Result<QueryListResponse, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .query(req)
            .await
    }

    async fn graph(
        self,
        ctx: Context,
        project: Project,
        req: GraphRequest,
    ) -> Result<GraphResponse, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .graph(req)
            .await
    }

    async fn reindex(self, ctx: Context, project: Project) -> Result<ReindexDone, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .reindex()
            .await
    }

    async fn create(
        self,
        ctx: Context,
        project: Project,
        spec: NewSpec,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .create(spec)
            .await
    }

    async fn set_status(
        self,
        ctx: Context,
        project: Project,
        id: String,
        status: ItemStatus,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .set_status(id, status)
            .await
    }

    async fn edit(
        self,
        ctx: Context,
        project: Project,
        id: String,
        assignments: Vec<String>,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .edit(id, assignments)
            .await
    }

    async fn apply_edit(
        self,
        ctx: Context,
        project: Project,
        id: String,
        req: EditRequest,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .apply_edit(id, req)
            .await
    }

    async fn add_comment(
        self,
        ctx: Context,
        project: Project,
        id: String,
        author: String,
        body: String,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .add_comment(id, author, body)
            .await
    }

    async fn dep_add(
        self,
        ctx: Context,
        project: Project,
        id: String,
        dep_id: String,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .dep_add(id, dep_id)
            .await
    }

    async fn dep_remove(
        self,
        ctx: Context,
        project: Project,
        id: String,
        dep_id: String,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .dep_remove(id, dep_id)
            .await
    }

    async fn set_parent(
        self,
        ctx: Context,
        project: Project,
        id: String,
        parent: Option<String>,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .set_parent(id, parent)
            .await
    }

    async fn show(self, ctx: Context, project: Project, id: String) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .show(id)
            .await
    }

    async fn stats(
        self,
        ctx: Context,
        project: Project,
        top: u32,
        include_epics: bool,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project, ctx.deadline)
            .await?
            .stats(top, include_epics)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Utf8PathBuf) {
        clove_core::daemon_token::use_records_dir(Utf8PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/test-clove-home/daemon-tokens"
        )));
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().join(".clove")).unwrap();
        std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
        (dir, clove_dir)
    }

    /// A deadline no test reaches.
    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    /// A call for `clove_dir` as its own client makes it: with its token.
    fn call(clove_dir: &Utf8Path, load: bool) -> Project {
        // The token exists once any client has loaded the project.
        let mut project = clove_ipc::project(clove_dir, true).unwrap();
        project.load = load;
        project
    }

    /// One project's background task panicking unloads that project — and only
    /// it: the hub keeps running, the other project keeps serving, and the
    /// failed project's lock is free for a fresh load.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_whose_task_panics_is_unloaded_alone() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_a_tmp, a) = store();
        let (_b_tmp, b) = store();
        let alpha = hub.attach(&call(&a, true), far()).await.unwrap();

        // Load beta by hand so its task set can carry a faulty task.
        let key = slot::canonical_key(b.as_str()).unwrap();
        let beta = Arc::new(slot::open(&key, hub.shutdown().child_token()).unwrap());
        beta.started.set(()).unwrap();
        let cell: SlotCell = Arc::default();
        assert!(cell.slot.set(Arc::clone(&beta)).is_ok());
        hub.table().slots.insert(key, cell);
        let mut tasks = beta.tasks();
        tasks.spawn(async { panic!("injected fault") });
        tokio::spawn(hub.clone().supervise(Arc::clone(&beta), tasks));

        tokio::time::timeout(Duration::from_secs(5), beta.done.cancelled())
            .await
            .expect("beta torn down");
        let served: Vec<String> = hub
            .status()
            .projects
            .into_iter()
            .map(|p| p.clove_dir)
            .collect();
        assert_eq!(served, vec![alpha.clove_dir.to_string()]);
        assert!(!alpha.cancel.is_cancelled(), "alpha untouched");
        assert!(!hub.shutdown().is_cancelled(), "the hub keeps running");
        hub.attach(&call(&b, true), far())
            .await
            .expect("beta's lock was released, so it loads afresh");
    }

    /// Every caller that loads a project gets it back only once it is fully
    /// started — its web UI mounted — not just the first: `clove serve` reads
    /// the web URL right after its load returns (L4).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_loads_return_only_once_the_project_is_mounted() {
        let mut early = 0;
        for _ in 0..10 {
            let hub = Hub::new(true, Duration::from_secs(60));
            let (_tmp, a) = store();
            std::fs::write(a.join("config.toml"), "[web]\nport = 0\n").unwrap();
            let loads: Vec<_> = (0..4)
                .map(|_| {
                    let (hub, a) = (hub.clone(), a.clone());
                    tokio::spawn(async move { hub.attach(&call(&a, true), far()).await })
                })
                .collect();
            for load in loads {
                let slot = load.await.unwrap().unwrap();
                let web_url = slot.dispatcher.state.lock().unwrap().snapshot().web_url;
                early += usize::from(web_url.is_none());
            }
            hub.unload_all().await;
        }
        assert_eq!(early, 0, "loads returned before the web UI was mounted");
    }

    /// A client that gives up mid-load (its call cancelled — or cut off by a
    /// detach) must not leave the project's lock held by an orphaned load: the
    /// next load waits for it and succeeds (L2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_load_abandoned_by_its_caller_never_blocks_the_next() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_b_tmp, b) = store();
        hub.attach(&call(&b, true), far()).await.unwrap(); // keeps the hub from exiting
        let (_a_tmp, a) = store();
        let a_project = call(&a, false);
        let mut failures = Vec::new();
        for round in 0..16u64 {
            let loader = {
                let (hub, a) = (hub.clone(), a.clone());
                tokio::spawn(async move { hub.attach(&call(&a, true), far()).await })
            };
            tokio::time::sleep(Duration::from_micros(round * 400)).await;
            loader.abort();
            if let Err(e) = hub.attach(&call(&a, true), far()).await {
                failures.push(format!("round {round}: {}: {}", e.code, e.message));
            }
            let _ = hub.detach(&a_project, far()).await;
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// A teardown held up past the hub's old 5s wait — its index checkpoint
    /// stuck behind in-flight work — is reported honestly by the stop (still
    /// stopping, lock still held), and a start right after waits it out and
    /// succeeds (M-new).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_teardown_is_reported_and_waited_out() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_b_tmp, b) = store();
        let (_a_tmp, a) = store();
        hub.attach(&call(&b, true), far()).await.unwrap(); // keeps the hub alive
        let slot = hub.attach(&call(&a, true), far()).await.unwrap();

        // Stand in for a daemon-side reindex holding the index.
        let index = Arc::clone(&slot.dispatcher.index);
        let (held_tx, held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _busy = index.lock().unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(6500));
        });
        held.recv().unwrap();
        drop(slot);

        let stopped = hub
            .detach(
                &call(&a, false),
                Instant::now() + Duration::from_millis(600),
            )
            .await
            .unwrap();
        assert!(stopped.detached && stopped.stopping, "{stopped:?}");
        let lock = clove_core::fs_safe::open_lock_file(&clove_ipc::lock_path(&a)).unwrap();
        assert!(
            matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "the stop said 'still stopping' but the project's lock is free"
        );
        drop(lock);

        let restarted = hub.attach(&call(&a, true), far()).await;
        holder.join().unwrap();
        let restarted = restarted.expect("the start waited out the slow teardown");
        assert!(!restarted.cancel.is_cancelled());
    }

    /// Stopping the last project stops the hub — also when its teardown
    /// outlasted the stop's call: the hub exits once that teardown is done,
    /// not after its idle grace (item 3).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hub_exits_once_its_last_projects_slow_teardown_is_done() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_a_tmp, a) = store();
        let slot = hub.attach(&call(&a, true), far()).await.unwrap();
        let index = Arc::clone(&slot.dispatcher.index);
        let (held_tx, held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _busy = index.lock().unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(3000));
        });
        held.recv().unwrap();
        drop(slot);
        let stopped = hub
            .detach(
                &call(&a, false),
                Instant::now() + Duration::from_millis(500),
            )
            .await
            .unwrap();
        assert!(stopped.stopping && !stopped.hub_exiting, "{stopped:?}");
        let exited =
            tokio::time::timeout(Duration::from_secs(10), hub.shutdown().cancelled()).await;
        holder.join().unwrap();
        assert!(
            exited.is_ok(),
            "the hub lingered after its last project stopped"
        );
    }

    /// A stop that lands while the project is still loading wins: the load is
    /// torn down when it completes, the loading client is told so, and the
    /// project is not left served (L-new-1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_during_a_load_wins() {
        let hub =
            Hub::new(false, Duration::from_secs(60)).with_load_delay(Duration::from_millis(2000));
        let (_b_tmp, b) = store();
        let (_a_tmp, a) = store();
        hub.attach(&call(&b, true), far()).await.unwrap(); // keeps the hub from exiting
        let loader = {
            let (hub, a) = (hub.clone(), a.clone());
            tokio::spawn(async move { hub.attach(&call(&a, true), far()).await })
        };
        // In flight: A has an entry, and no slot yet.
        let key = slot::canonical_key(a.as_str()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !hub
            .table()
            .slots
            .get(&key)
            .is_some_and(|e| e.slot.get().is_none())
        {
            assert!(Instant::now() < deadline, "the load never got under way");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let detached = hub.detach(&call(&a, false), far()).await.unwrap();
        assert!(
            detached.detached,
            "the stop saw nothing to stop: {detached:?}"
        );
        let loaded = loader.await.unwrap();
        assert!(loaded.is_err(), "the load it raced still succeeded");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let served: Vec<String> = hub
            .status()
            .projects
            .into_iter()
            .map(|p| p.clove_dir)
            .collect();
        assert!(
            !served
                .iter()
                .any(|dir| dir.ends_with(a.as_str().trim_start_matches("/private"))),
            "A is served after its stop: {served:?}"
        );
    }

    /// A project path is the caller's own, absolute; a relative one would be
    /// resolved against the hub's working directory, which is nobody's.
    #[tokio::test]
    async fn a_relative_project_path_is_refused() {
        let hub = Hub::new(false, Duration::from_secs(60));
        for load in [false, true] {
            let relative = Project {
                clove_dir: ".clove".to_owned(),
                load,
                token: "0".repeat(64),
            };
            let err = hub.attach(&relative, far()).await.err().expect("refused");
            assert_eq!(err.code, codes::BAD_PROJECT);
        }
    }

    /// Once the hub has decided to exit, it admits no new project — the
    /// decision and the admission share one lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_the_last_detach_no_project_is_admitted() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_a_tmp, a) = store();
        let (_b_tmp, b) = store();
        hub.attach(&call(&a, true), far()).await.unwrap();
        let detached = hub.detach(&call(&a, false), far()).await.unwrap();
        assert!(detached.hub_exiting);
        let err = hub
            .attach(&call(&b, true), far())
            .await
            .err()
            .expect("refused");
        assert_eq!(err.code, codes::SHUTTING_DOWN);
    }

    /// A call is served only with its project's own token: a wrong one, or
    /// another project's, is refused before anything loads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_is_served_only_with_its_projects_token() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_a_tmp, a) = store();
        let (_b_tmp, b) = store();
        let a_call = call(&a, true);
        let b_call = call(&b, true);

        let wrong = Project {
            token: "f".repeat(64),
            ..a_call.clone()
        };
        let err = hub.attach(&wrong, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN);
        let borrowed = Project {
            token: a_call.token.clone(),
            ..b_call.clone()
        };
        let err = hub.attach(&borrowed, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN);
        assert!(
            hub.status().projects.is_empty(),
            "a refused call loaded something"
        );

        hub.attach(&a_call, far()).await.expect("A's own token");
        let err = hub.detach(&borrowed, far()).await.expect_err("refused");
        assert_eq!(err.code, codes::BAD_TOKEN);
        assert_eq!(
            hub.status().projects.len(),
            1,
            "A was detached by B's caller"
        );
    }

    /// A project whose `issues/` is a symlink is not loaded: serving it would
    /// report "serving" while every call it answers failed.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_symlinked_issues_directory_is_not_loaded() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (tmp, a) = store();
        // Named as a client would have before issues/ was swapped for a link
        // (a client checks too; this is the hub's own check).
        let named = call(&a, true);
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::remove_dir(a.join("issues")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, a.join("issues")).unwrap();
        let err = hub.attach(&named, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::LOAD_FAILED, "{}", err.message);
        assert!(hub.status().projects.is_empty());
    }

    /// A token replaced on disk takes effect at once; the old one stops working.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replaced_token_is_read_again() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_tmp, a) = store();
        let old = call(&a, true);
        hub.attach(&old, far()).await.unwrap();
        std::fs::remove_file(clove_core::daemon_token::token_path(&a)).unwrap();
        let new = call(&a, true);
        assert_ne!(new.token, old.token);
        hub.attach(&new, far()).await.expect("the new token");
        let err = hub.attach(&old, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN);
    }

    /// Rewritten in place — same size, same inode, its mtime put back — the
    /// file no longer holds the old token, and the hub goes by what it holds:
    /// the old token stops working (and the new value, which clove never
    /// issued, is not trusted either).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_token_rewritten_in_place_is_read_again() {
        use std::io::{Seek as _, Write as _};
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_tmp, a) = store();
        let old = call(&a, true);
        hub.attach(&old, far()).await.unwrap();
        let path = clove_core::daemon_token::token_path(&a);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let new_token: String = old.token.chars().rev().collect();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        writeln!(file, "{new_token}").unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        let new = Project {
            token: new_token,
            ..old.clone()
        };
        let err = hub.attach(&old, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN, "the old token still works");
        let err = hub.attach(&new, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN, "an unissued token was trusted");
    }

    /// A symlink planted as the token is not read through.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_token_is_refused_by_the_hub() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (tmp, a) = store();
        let elsewhere = tmp.path().join("elsewhere");
        let token = "0123456789abcdef0123456789abcdef";
        std::fs::write(&elsewhere, token).unwrap();
        std::os::unix::fs::symlink(&elsewhere, clove_core::daemon_token::token_path(&a)).unwrap();
        let planted = Project {
            clove_dir: a.to_string(),
            load: true,
            token: token.to_owned(),
        };
        let err = hub.attach(&planted, far()).await.err().expect("refused");
        assert_eq!(err.code, codes::BAD_TOKEN);
    }
}
