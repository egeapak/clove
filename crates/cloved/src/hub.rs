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
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use clove_ipc::hub::{codes, frame, peer_is_this_user, recv_frame, send_frame, Hello, Welcome};
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
/// How long a detach or shutdown waits for a slot's teardown.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// The hub's shared web listener.
struct WebSite {
    web: clove_web::HubWeb,
    addr: SocketAddr,
}

/// A cheap handle to the hub; clones share it.
#[derive(Clone)]
pub struct Hub(Arc<Inner>);

type SlotCell = Arc<OnceCell<Arc<Slot>>>;

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

    /// The slot for `clove_dir`, loading it first when `load` is set.
    pub async fn attach(&self, clove_dir: &str, load: bool) -> Result<Arc<Slot>, LoadError> {
        let slot = self.attach_once(clove_dir, load).await?;
        if !slot.cancel.is_cancelled() {
            return Ok(slot);
        }
        // Caught mid-teardown (an idle eviction, say). A probe reports it gone;
        // a load waits for the teardown to finish and loads the project afresh.
        let unloaded = || {
            refused(
                codes::NOT_LOADED,
                format!("the daemon is not serving {clove_dir}"),
            )
        };
        if !load {
            return Err(unloaded());
        }
        let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
        let slot = self.attach_once(clove_dir, load).await?;
        if slot.cancel.is_cancelled() {
            return Err(unloaded());
        }
        Ok(slot)
    }

    async fn attach_once(&self, clove_dir: &str, load: bool) -> Result<Arc<Slot>, LoadError> {
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
        let cell = {
            let mut table = self.table();
            if table.exiting {
                return Err(refused(
                    codes::SHUTTING_DOWN,
                    "the daemon is shutting down; start it again once it has exited",
                ));
            }
            if load {
                table.slots.entry(key.clone()).or_default().clone()
            } else {
                table.slots.get(&key).cloned().ok_or_else(not_loaded)?
            }
        };
        let slot = if load {
            let loaded = cell
                .get_or_try_init(|| {
                    let key = key.clone();
                    let cancel = self.0.shutdown.child_token();
                    async move {
                        tokio::task::spawn_blocking(move || slot::open(&key, cancel))
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
            match loaded {
                Ok(slot) => slot,
                Err(e) => {
                    let mut table = self.table();
                    if table
                        .slots
                        .get(&key)
                        .is_some_and(|c| Arc::ptr_eq(c, &cell) && c.get().is_none())
                    {
                        table.slots.remove(&key);
                    }
                    return Err(e);
                }
            }
        } else {
            // Still loading counts as not loaded: a probe must not wait on it.
            cell.get().cloned().ok_or_else(not_loaded)?
        };
        if !slot.started.swap(true, Ordering::SeqCst) {
            self.start(&slot).await;
        }
        // Detached (or the hub drained) while it was loading: nothing tracks it
        // any more, so tear it down rather than let it serve unlisted.
        let registered = self
            .table()
            .slots
            .get(&key)
            .is_some_and(|c| Arc::ptr_eq(c, &cell));
        if !registered {
            slot.cancel.cancel();
            return Err(not_loaded());
        }
        Ok(slot)
    }

    /// The project-scoped half of a call: the caller's project, resolved to
    /// its slot's dispatcher.
    async fn dispatcher(&self, project: &Project) -> Result<Dispatcher, RpcError> {
        Ok(self
            .attach(&project.clove_dir, project.load)
            .await?
            .dispatcher
            .clone())
    }

    /// Mount the slot on the web listener and start its supervised tasks.
    async fn start(&self, slot: &Arc<Slot>) {
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
            let slug = site.web.mount(&slot.repo_root, app);
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
        self.forget(&slot);
        slot.done.cancel();
    }

    /// Drop a torn-down slot from the table and the web listener.
    fn forget(&self, slot: &Arc<Slot>) {
        {
            let mut table = self.table();
            let current = table
                .slots
                .get(&slot.clove_dir)
                .and_then(|cell| cell.get())
                .is_some_and(|s| Arc::ptr_eq(s, slot));
            if current {
                table.slots.remove(&slot.clove_dir);
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

    /// Stop serving `project`. Returns once its teardown has run (its
    /// `daemon.lock` released), so a reload right after cannot find it held.
    /// When nothing is left, the hub decides — under the table lock, so no
    /// concurrent load can slip in between — to exit.
    pub async fn detach(&self, project: &Project) -> Result<Detached, RpcError> {
        let key = self.key(&project.clove_dir).await;
        let slot = match &key {
            Ok(key) => self.table().slots.get(key).and_then(|c| c.get().cloned()),
            Err(e) if e.code == codes::BAD_PROJECT => return Err(e.clone().into()),
            Err(_) => None,
        };
        let detached = match slot {
            Some(slot) => {
                slot.cancel.cancel();
                let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
                true
            }
            None => false,
        };
        let hub_exiting = {
            let mut table = self.table();
            if table.slots.is_empty() {
                table.exiting = true;
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
        })
    }

    /// The hub and every project it serves.
    pub fn status(&self) -> HubStatus {
        let loaded: Vec<Arc<Slot>> = self
            .table()
            .slots
            .values()
            .filter_map(|cell| cell.get().cloned())
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
        }
    }

    /// Tear every slot down (hub shutdown). Admits nothing new from here on.
    pub async fn unload_all(&self) {
        let cells: Vec<SlotCell> = {
            let mut table = self.table();
            table.exiting = true;
            table.slots.drain().map(|(_, cell)| cell).collect()
        };
        for slot in cells.iter().filter_map(|cell| cell.get()) {
            slot.cancel.cancel();
        }
        for slot in cells.iter().filter_map(|cell| cell.get()) {
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
        // Defence in depth behind the private runtime directory: serve only
        // this user's processes.
        if !peer_is_this_user(&stream).unwrap_or(false) {
            return;
        }
        let mut framed = frame(stream);
        let hello = match tokio::time::timeout(HELLO_TIMEOUT, recv_frame::<Hello>(&mut framed))
            .await
        {
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

    async fn attach(self, _: Context, project: Project) -> Result<(), RpcError> {
        let slot = self.0.attach(&project.clove_dir, project.load).await?;
        // The project heartbeat: count it and reset the idle window.
        if let Ok(mut state) = slot.dispatcher.state.lock() {
            state.record_ping();
        }
        Ok(())
    }

    async fn detach(self, _: Context, project: Project) -> Result<Detached, RpcError> {
        self.0.detach(&project).await
    }

    async fn status(self, _: Context, project: Project) -> Result<StatusResponse, RpcError> {
        Ok(self.0.dispatcher(&project).await?.status().await)
    }

    async fn change_generation(self, _: Context, project: Project) -> Result<u64, RpcError> {
        Ok(self.0.dispatcher(&project).await?.change_generation().await)
    }

    async fn query(
        self,
        _: Context,
        project: Project,
        req: QueryRequest,
    ) -> Result<QueryListResponse, RpcError> {
        self.0.dispatcher(&project).await?.query(req).await
    }

    async fn graph(
        self,
        _: Context,
        project: Project,
        req: GraphRequest,
    ) -> Result<GraphResponse, RpcError> {
        self.0.dispatcher(&project).await?.graph(req).await
    }

    async fn reindex(self, _: Context, project: Project) -> Result<ReindexDone, RpcError> {
        self.0.dispatcher(&project).await?.reindex().await
    }

    async fn create(self, _: Context, project: Project, spec: NewSpec) -> Result<Value, RpcError> {
        self.0.dispatcher(&project).await?.create(spec).await
    }

    async fn set_status(
        self,
        _: Context,
        project: Project,
        id: String,
        status: ItemStatus,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .set_status(id, status)
            .await
    }

    async fn edit(
        self,
        _: Context,
        project: Project,
        id: String,
        assignments: Vec<String>,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .edit(id, assignments)
            .await
    }

    async fn apply_edit(
        self,
        _: Context,
        project: Project,
        id: String,
        req: EditRequest,
    ) -> Result<Value, RpcError> {
        self.0.dispatcher(&project).await?.apply_edit(id, req).await
    }

    async fn add_comment(
        self,
        _: Context,
        project: Project,
        id: String,
        author: String,
        body: String,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .add_comment(id, author, body)
            .await
    }

    async fn dep_add(
        self,
        _: Context,
        project: Project,
        id: String,
        dep_id: String,
    ) -> Result<Value, RpcError> {
        self.0.dispatcher(&project).await?.dep_add(id, dep_id).await
    }

    async fn dep_remove(
        self,
        _: Context,
        project: Project,
        id: String,
        dep_id: String,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .dep_remove(id, dep_id)
            .await
    }

    async fn set_parent(
        self,
        _: Context,
        project: Project,
        id: String,
        parent: Option<String>,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .set_parent(id, parent)
            .await
    }

    async fn show(self, _: Context, project: Project, id: String) -> Result<Value, RpcError> {
        self.0.dispatcher(&project).await?.show(id).await
    }

    async fn stats(
        self,
        _: Context,
        project: Project,
        top: u32,
        include_epics: bool,
    ) -> Result<Value, RpcError> {
        self.0
            .dispatcher(&project)
            .await?
            .stats(top, include_epics)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().join(".clove")).unwrap();
        std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
        (dir, clove_dir)
    }

    /// One project's background task panicking unloads that project — and only
    /// it: the hub keeps running, the other project keeps serving, and the
    /// failed project's lock is free for a fresh load.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_whose_task_panics_is_unloaded_alone() {
        let hub = Hub::new(false, Duration::from_secs(60));
        let (_a_tmp, a) = store();
        let (_b_tmp, b) = store();
        let alpha = hub.attach(a.as_str(), true).await.unwrap();

        // Load beta by hand so its task set can carry a faulty task.
        let key = slot::canonical_key(b.as_str()).unwrap();
        let beta = Arc::new(slot::open(&key, hub.shutdown().child_token()).unwrap());
        beta.started.store(true, Ordering::SeqCst);
        let cell: SlotCell = Arc::default();
        assert!(cell.set(Arc::clone(&beta)).is_ok());
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
        hub.attach(b.as_str(), true)
            .await
            .expect("beta's lock was released, so it loads afresh");
    }

    /// A project path is the caller's own, absolute; a relative one would be
    /// resolved against the hub's working directory, which is nobody's.
    #[tokio::test]
    async fn a_relative_project_path_is_refused() {
        let hub = Hub::new(false, Duration::from_secs(60));
        for load in [false, true] {
            let err = hub.attach(".clove", load).await.err().expect("refused");
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
        hub.attach(a.as_str(), true).await.unwrap();
        let detached = hub
            .detach(&Project {
                clove_dir: a.to_string(),
                load: false,
            })
            .await
            .unwrap();
        assert!(detached.hub_exiting);
        let err = hub.attach(b.as_str(), true).await.err().expect("refused");
        assert_eq!(err.code, codes::SHUTTING_DOWN);
    }
}
