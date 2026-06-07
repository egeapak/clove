//! The MCP tool engine: one sync method per tool, decoupled from rmcp.
//!
//! Topology B: **writes** prefer the single `cloved` daemon (which serializes
//! them and keeps its index/graph coherent) and fall back to direct `clove-core`
//! ops when no daemon is reachable; **reads** compute directly from the file
//! store via `clove-core::ops` (always correct, no daemon needed). Every method
//! returns the §7.4 JSON shape or a human-readable error string for the tool
//! result. Methods are blocking and meant to run on a blocking task.

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::ops;
use clove_core::{Filters, ItemStore};
use clove_ipc::DaemonClient;
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
}

impl Engine {
    fn store(&self) -> ItemStore {
        ItemStore::new(self.repo_root.clone())
    }

    /// A live daemon write coordinator, if one is running for this repo.
    fn daemon(&self) -> Option<DaemonClient> {
        DaemonClient::probe(&self.clove_dir)
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
        match self.daemon() {
            Some(mut d) => d.create(spec).map_err(stringify),
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
        match self.daemon() {
            Some(mut d) => d.set_status(a.id, status).map_err(stringify),
            None => {
                let id = parse_id(&a.id)?;
                ops::transition(&self.store(), &id, status, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn edit(&self, a: EditArgs) -> Result<Value, String> {
        let tokens = a.to_tokens();
        if tokens.is_empty() {
            return Err("no fields to edit".to_owned());
        }
        match self.daemon() {
            Some(mut d) => d.edit(a.id, tokens).map_err(stringify),
            None => {
                let id = parse_id(&a.id)?;
                ops::edit(&self.store(), &id, &tokens, Utc::now()).map_err(stringify)
            }
        }
    }

    pub fn comment(&self, a: CommentArgs) -> Result<Value, String> {
        let author = author();
        match self.daemon() {
            Some(mut d) => d.add_comment(a.id, author, a.message).map_err(stringify),
            None => {
                let id = parse_id(&a.id)?;
                ops::comment(&self.store(), &id, &author, &a.message).map_err(stringify)
            }
        }
    }

    pub fn dep_add(&self, a: DepAddArgs) -> Result<Value, String> {
        match self.daemon() {
            Some(mut d) => d.dep_add(a.id, a.dep_id).map_err(stringify),
            None => {
                let id = parse_id(&a.id)?;
                let dep = parse_id(&a.dep_id)?;
                ops::dep_add(&self.store(), &id, &dep, Utc::now()).map_err(stringify)
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
