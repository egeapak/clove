//! The hub (DESIGN §8.1): one daemon serving every project of a user.
//!
//! Projects are [`Slot`]s, keyed by their canonical `.clove/` directory. A slot
//! is loaded when a client attaches with `load: true`, evicted when it idles
//! out, detached on request, and unloaded — alone — when any of its tasks ends
//! or panics. The hub exits once it has served nothing for a grace period.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_ipc::hub::{codes, frame, recv_frame, send_frame, Detached, Hello, Welcome};
use clove_ipc::{
    transport_from_framed, CloveRpc, HubRpc, HubStatus, ProjectInfo, RpcError, PROTOCOL_VERSION,
};
use futures::StreamExt;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel};
use tokio::sync::OnceCell;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

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

struct Inner {
    /// One cell per project being loaded or served. A cell is inserted before
    /// the load starts, so concurrent attaches of one project share one load
    /// instead of racing for its lock.
    slots: Mutex<HashMap<Utf8PathBuf, SlotCell>>,
    /// Whether the hub serves the web UI at all (`CLOVED_DISABLE_WEB` unset).
    web_enabled: bool,
    /// The one web listener, bound when the first web-enabled project loads.
    /// `Some(None)` once a bind has failed: the hub then serves no web UI.
    web: OnceCell<Option<WebSite>>,
    started: Instant,
    shutdown: CancellationToken,
    grace: Duration,
}

impl Hub {
    pub fn new(web_enabled: bool, grace: Duration) -> Hub {
        Hub(Arc::new(Inner {
            slots: Mutex::new(HashMap::new()),
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

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<Utf8PathBuf, SlotCell>> {
        self.0.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The slot for `clove_dir`, loading it first when `load` is set.
    pub async fn attach(&self, clove_dir: &str, load: bool) -> Result<Arc<Slot>, LoadError> {
        let slot = self.attach_once(clove_dir, load).await?;
        if !slot.cancel.is_cancelled() {
            return Ok(slot);
        }
        // Caught mid-teardown (an idle eviction, say). A probe reports it gone;
        // a load waits for the teardown to finish and loads the project afresh.
        let unloaded = LoadError {
            code: codes::NOT_LOADED,
            message: format!("the daemon is not serving {clove_dir}"),
        };
        if !load {
            return Err(unloaded);
        }
        let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
        let slot = self.attach_once(clove_dir, load).await?;
        if slot.cancel.is_cancelled() {
            return Err(unloaded);
        }
        Ok(slot)
    }

    async fn attach_once(&self, clove_dir: &str, load: bool) -> Result<Arc<Slot>, LoadError> {
        if self.0.shutdown.is_cancelled() {
            return Err(LoadError {
                code: codes::SHUTTING_DOWN,
                message: "the daemon is shutting down".to_owned(),
            });
        }
        let not_loaded = || LoadError {
            code: codes::NOT_LOADED,
            message: format!("the daemon is not serving {clove_dir}"),
        };
        let key = match slot::canonical_key(clove_dir) {
            Ok(key) => key,
            Err(_) if !load => return Err(not_loaded()),
            Err(e) => return Err(e),
        };
        let cell = {
            let mut slots = self.slots();
            if load {
                slots.entry(key.clone()).or_default().clone()
            } else {
                slots.get(&key).cloned().ok_or_else(not_loaded)?
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
                                Err(LoadError {
                                    code: codes::LOAD_FAILED,
                                    message: format!("loading the project panicked: {e}"),
                                })
                            })
                            .map(Arc::new)
                    }
                })
                .await
                .cloned();
            match loaded {
                Ok(slot) => slot,
                Err(e) => {
                    let mut slots = self.slots();
                    if slots
                        .get(&key)
                        .is_some_and(|c| Arc::ptr_eq(c, &cell) && c.get().is_none())
                    {
                        slots.remove(&key);
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
            .slots()
            .get(&key)
            .is_some_and(|c| Arc::ptr_eq(c, &cell));
        if !registered {
            slot.cancel.cancel();
            return Err(not_loaded());
        }
        // An attach is the liveness probe that used to be a `ping`: count it as
        // one, which also resets the project's idle window.
        if let Ok(mut state) = slot.dispatcher.state.lock() {
            state.record_ping();
        }
        Ok(slot)
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
        slot.checkpoint();
        slot.release_lock();
        self.forget(&slot);
        slot.done.cancel();
    }

    /// Drop a torn-down slot from the table and the web listener.
    fn forget(&self, slot: &Arc<Slot>) {
        {
            let mut slots = self.slots();
            let current = slots
                .get(&slot.clove_dir)
                .and_then(|cell| cell.get())
                .is_some_and(|s| Arc::ptr_eq(s, slot));
            if current {
                slots.remove(&slot.clove_dir);
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

    /// Stop serving `clove_dir`. Returns once its teardown has run (its
    /// `daemon.lock` released), so a reload right after cannot find it held.
    pub async fn detach(&self, clove_dir: &str) -> Detached {
        let key = slot::canonical_key(clove_dir).unwrap_or_else(|_| Utf8PathBuf::from(clove_dir));
        let cell = self.slots().remove(&key);
        let detached = match cell.and_then(|c| c.get().cloned()) {
            Some(slot) => {
                slot.cancel.cancel();
                let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
                true
            }
            None => false,
        };
        let hub_exiting = self.slots().is_empty();
        if hub_exiting {
            // Let the reply reach the client before the runtime winds down.
            let shutdown = self.0.shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                shutdown.cancel();
            });
        }
        Detached {
            detached,
            hub_exiting,
        }
    }

    /// The hub and every project it serves.
    pub fn status(&self) -> HubStatus {
        let loaded: Vec<Arc<Slot>> = self
            .slots()
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

    /// Tear every slot down (hub shutdown).
    pub async fn unload_all(&self) {
        let cells: Vec<SlotCell> = self.slots().drain().map(|(_, cell)| cell).collect();
        for slot in cells.iter().filter_map(|cell| cell.get()) {
            slot.cancel.cancel();
        }
        for slot in cells.iter().filter_map(|cell| cell.get()) {
            let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, slot.done.cancelled()).await;
        }
    }

    /// Resolve once the hub has served no project for the grace period.
    pub async fn idle_exit(&self) {
        let tick = (self.0.grace / 4).max(Duration::from_millis(25));
        let mut empty_since: Option<Instant> = None;
        loop {
            tokio::time::sleep(tick).await;
            if self.slots().is_empty() {
                let since = *empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.0.grace {
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

    /// Run the handshake, then the service it selected, until either side
    /// closes — or the attached slot is torn down, which closes the connection
    /// so its client re-probes rather than talking to an unloaded project.
    async fn serve_connection(self, stream: Stream) {
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
        match hello {
            Hello::Attach {
                clove_dir, load, ..
            } => {
                let slot = match self.attach(&clove_dir, load).await {
                    Ok(slot) => slot,
                    Err(e) => {
                        let _ = send_frame(&mut framed, &refusal(e.code, &e.message)).await;
                        return;
                    }
                };
                if send_frame(&mut framed, &ok).await.is_err() {
                    return;
                }
                let dispatcher = slot.dispatcher.clone();
                let cancel = slot.cancel.clone();
                drop(slot);
                let serve = BaseChannel::with_defaults(transport_from_framed(framed))
                    .execute(dispatcher.serve())
                    .for_each(|response| async move {
                        tokio::spawn(response);
                    });
                tokio::select! {
                    _ = serve => {},
                    _ = cancel.cancelled() => {},
                }
            }
            Hello::Control { .. } => {
                if send_frame(&mut framed, &ok).await.is_err() {
                    return;
                }
                let shutdown = self.0.shutdown.clone();
                let serve = BaseChannel::with_defaults(transport_from_framed(framed))
                    .execute(Control(self).serve())
                    .for_each(|response| async move {
                        tokio::spawn(response);
                    });
                tokio::select! {
                    _ = serve => {},
                    _ = shutdown.cancelled() => {},
                }
            }
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

/// The [`HubRpc`] control service.
#[derive(Clone)]
struct Control(Hub);

impl HubRpc for Control {
    async fn ping(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    async fn hub_status(self, _: Context) -> HubStatus {
        self.0.status()
    }

    async fn detach(self, _: Context, clove_dir: String) -> Result<Detached, RpcError> {
        Ok(self.0.detach(&clove_dir).await)
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
        hub.slots().insert(key, cell);
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
}
