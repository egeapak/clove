//! The MCP tool engine: one sync method per tool, decoupled from rmcp.
//!
//! Topology B: **writes** prefer the single `cloved` daemon (which serializes
//! them and keeps its index/graph coherent) and fall back to direct `clove-core`
//! ops when no daemon is reachable; **reads** compute directly from the file
//! store via `clove-core::ops` (always correct, no daemon needed). Every method
//! returns the §7.4 JSON shape or a human-readable error string for the tool
//! result. Methods are blocking and meant to run on a blocking task.

use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::ops;
use clove_core::{Filters, ItemStore};
use clove_ipc::{ClientError, DaemonClient};
use clove_types::{CloveId, ItemStatus, ItemType, NewSpec};
use serde_json::Value;

use crate::args::*;

/// Shared, cheap-to-clone context for the tools.
#[derive(Clone)]
pub struct Engine {
    /// The `.clove/` directory (for daemon probing).
    pub clove_dir: Utf8PathBuf,
    /// The repository root (for the file store).
    pub repo_root: Utf8PathBuf,
    /// Id prefix + default type for `create` (from `.clove/config`).
    pub id_prefix: String,
    pub default_type: ItemType,
    /// The cached daemon write coordinator. A probe builds a tokio runtime,
    /// connects, and pings — far too heavy to repeat on every tool call of a
    /// long-lived server — so the client is kept and only its (cheap, on the
    /// open connection) `ping` is repeated per call; a dead/restarted daemon
    /// drops the cache and re-probes.
    daemon: Arc<Mutex<Option<DaemonClient>>>,
}

impl Engine {
    pub fn new(
        clove_dir: Utf8PathBuf,
        repo_root: Utf8PathBuf,
        id_prefix: String,
        default_type: ItemType,
    ) -> Self {
        Self {
            clove_dir,
            repo_root,
            id_prefix,
            default_type,
            daemon: Arc::new(Mutex::new(None)),
        }
    }

    fn store(&self) -> ItemStore {
        ItemStore::new(self.repo_root.clone())
    }

    /// Run `call` against the daemon write coordinator, if one is reachable.
    ///
    /// `None` → no daemon (caller falls back to direct ops). The call itself
    /// is attempted exactly ONCE (a failed write RPC must surface, never be
    /// blindly retried — the daemon may have applied it before the response
    /// was lost); only the *liveness check* re-probes.
    fn with_daemon<T>(
        &self,
        call: impl FnOnce(&mut DaemonClient) -> Result<T, ClientError>,
    ) -> Option<Result<T, String>> {
        let mut guard = self.daemon.lock().unwrap_or_else(|e| e.into_inner());
        // Validate the cached connection first: one ping on the already-open
        // connection, no runtime construction.
        if let Some(client) = guard.as_mut() {
            if client.ping().is_err() {
                *guard = None;
            }
        }
        if guard.is_none() {
            *guard = DaemonClient::probe(&self.clove_dir);
        }
        let client = guard.as_mut()?;
        Some(call(client).map_err(stringify))
    }

    // ---- Read tools (file-based via ops) ------------------------------------

    pub fn ready(&self, a: FilterArgs) -> Result<Value, String> {
        let filters = a.to_filters()?;
        ops::ready(&self.store(), &filters, limit(a.limit, 50)).map_err(stringify)
    }

    pub fn blocked(&self, a: BlockedArgs) -> Result<Value, String> {
        let filters = a.filter.to_filters()?;
        ops::blocked(
            &self.store(),
            &filters,
            a.include_warnings.unwrap_or(false),
            limit(a.filter.limit, 50),
        )
        .map_err(stringify)
    }

    pub fn list(&self, a: ListArgs) -> Result<Value, String> {
        let filters = a.filter.to_filters()?;
        ops::list(
            &self.store(),
            &filters,
            a.offset.unwrap_or(0) as usize,
            limit(a.filter.limit, 50),
        )
        .map_err(stringify)
    }

    pub fn show(&self, a: IdArgs) -> Result<Value, String> {
        let id = parse_id(&a.id)?;
        ops::show(&self.store(), &id).map_err(stringify)
    }

    pub fn search(&self, a: SearchArgs) -> Result<Value, String> {
        ops::search(&self.store(), &a.text, limit(a.limit, 50)).map_err(stringify)
    }

    pub fn dep_tree(&self, a: DepTreeArgs) -> Result<Value, String> {
        let id = parse_id(&a.id)?;
        ops::dep_tree(&self.store(), &id, a.depth.unwrap_or(5) as usize).map_err(stringify)
    }

    pub fn stats(&self, a: StatsArgs) -> Result<Value, String> {
        ops::stats(
            &self.store(),
            a.top.unwrap_or(10) as usize,
            !a.no_epics.unwrap_or(false),
            Utc::now(),
        )
        .map_err(stringify)
    }

    // ---- Write tools (daemon-preferred, ops fallback) -----------------------

    pub fn create(&self, a: NewArgs) -> Result<Value, String> {
        let spec = NewSpec {
            title: a.title,
            item_type: a.item_type,
            priority: a.priority,
            labels: a.labels.unwrap_or_default(),
            deps: a.deps.unwrap_or_default(),
            parent: a.parent,
            assignee: a.assignee,
            body: a.body,
        };
        match self.with_daemon(|d| d.create(spec.clone())) {
            Some(result) => result,
            None => ops::create(
                &self.store(),
                &self.id_prefix,
                self.default_type,
                spec,
                Utc::now(),
            )
            .map_err(stringify),
        }
    }

    pub fn set_status(&self, a: StatusArgs) -> Result<Value, String> {
        let status = ItemStatus::parse(&a.status).map_err(stringify)?;
        match self.with_daemon(|d| d.set_status(a.id.clone(), status)) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                ops::transition(&self.store(), &id, status, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn edit(&self, a: EditArgs) -> Result<Value, String> {
        let req = a.to_request().map_err(stringify)?;
        if req.is_empty() {
            return Err("no fields to edit".to_owned());
        }
        match self.with_daemon(|d| d.apply_edit(a.id.clone(), req.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                clove_core::apply_edit(&self.store(), &id, &req, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn comment(&self, a: CommentArgs) -> Result<Value, String> {
        let author = author();
        match self.with_daemon(|d| d.add_comment(a.id.clone(), author.clone(), a.message.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                ops::comment(&self.store(), &id, &author, &a.message).map_err(stringify)
            }
        }
    }

    pub fn dep_add(&self, a: DepAddArgs) -> Result<Value, String> {
        match self.with_daemon(|d| d.dep_add(a.id.clone(), a.dep_id.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let dep = parse_id(&a.dep_id)?;
                ops::dep_add(&self.store(), &id, &dep, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn dep_remove(&self, a: DepAddArgs) -> Result<Value, String> {
        match self.with_daemon(|d| d.dep_remove(a.id.clone(), a.dep_id.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let dep = parse_id(&a.dep_id)?;
                ops::dep_remove(&self.store(), &id, &dep, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn set_parent(&self, a: SetParentArgs) -> Result<Value, String> {
        match self.with_daemon(|d| d.set_parent(a.id.clone(), a.parent.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let parent = match a.parent {
                    Some(p) => Some(parse_id(&p)?),
                    None => None,
                };
                ops::set_parent(&self.store(), &id, parent.as_ref(), Utc::now()).map_err(stringify)
            }
        }
    }
}

/// `--limit` semantics: absent → `default`; `0` → unlimited; `n` → `n`.
fn limit(arg: Option<u64>, default: usize) -> Option<usize> {
    match arg {
        None => Some(default),
        Some(0) => None,
        Some(n) => Some(n as usize),
    }
}

fn parse_id(raw: &str) -> Result<CloveId, String> {
    CloveId::new(raw).map_err(stringify)
}

fn stringify<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The comment author: `CLOVE_AUTHOR`, then `GIT_AUTHOR_EMAIL`, else `unknown`.
fn author() -> String {
    std::env::var("CLOVE_AUTHOR")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

impl FilterArgs {
    fn to_filters(&self) -> Result<Filters, String> {
        Filters::parse(
            self.status.as_deref(),
            self.item_type.as_deref(),
            self.label.as_deref(),
            self.assignee.as_deref(),
            self.priority,
        )
        .map_err(stringify)
    }
}
