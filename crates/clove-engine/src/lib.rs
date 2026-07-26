//! clove-engine: the read tier, once.
//!
//! Every clove read has three possible answers — a running `cloved`, the local
//! SQLite index, or a scan of the files — and before this crate each surface
//! chose between them for itself. The CLI's `ls`/`ready`/`blocked`/`query` each
//! re-implemented the same three-branch cascade with slightly different
//! fallback conditions; the MCP server always read files, so it paid a full
//! store scan per tool call while a hot daemon sat idle beside it; the web
//! always read files *and* rebuilt the whole dependency graph per request.
//!
//! [`Engine`] owns that decision **once per method**. A surface parses its
//! arguments, calls one method, and renders what comes back — including
//! [`ListAnswer::source`], which names the tier that actually answered so a
//! caller never has to guess (`_meta.source` on the CLI and web, `source` on the
//! MCP page).
//!
//! # What the engine decides, and what it does not
//!
//! It decides **where** rows come from, and it applies the window before it
//! returns, so a caller cannot re-page an already-paged answer. It does not
//! decide **shape**: the CLI renders a lean five-column row, the MCP tools
//! render full item objects, and the web renders those plus graph terms, so the
//! rows come back typed ([`Rows`]) and each surface serializes them its own way.
//! That split is deliberate — the tiering is the part that was duplicated four
//! times, and the rendering is the part that legitimately differs.
//!
//! # The tiers
//!
//! - **daemon** — a live `cloved` answers from its hot index and cached graph.
//!   Skipped by [`Tiers::daemon`] (`--no-index`, and the daemon-hosted web,
//!   which *is* the daemon and must not call itself).
//! - **index** — `.clove/index.db`, freshened inline when it is lightly stale
//!   and bypassed when it is too far behind, broken, or cannot *shape* the
//!   query (see [`query_too_complex`]).
//! - **files** — always correct, always available, and the only tier `search`
//!   has at all (read-path roadmap §6.1).

use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use clove_core::view::{Filters, Order, Page, SearchOrder};
use clove_core::{ops, GraphStore, ItemStore};
use clove_index::{Index, ItemListRow, QueryMode};
use clove_ipc::{DaemonClient, GraphRequest, GraphResponse, QueryKind, QueryRequest};
use clove_types::{CloveError, CloveId, ItemFrontmatter};
use rayon::prelude::*;
use serde_json::Value;

/// Above this many out-of-date items, skip the incremental refresh and fall back
/// to a file scan (DESIGN §6.4).
pub const STALE_REFRESH_LIMIT: usize = 20;

/// Above this many items in a page, hydrate in parallel. Mirrors
/// `ItemStore::scan_frontmatter`'s own threshold, so a hydrated page and a
/// scanned store parse the same way.
const PARALLEL_HYDRATE_THRESHOLD: usize = 500;

/// The keys a lean row carries — the columns `clove ls` renders, and the only
/// fields the index and daemon tiers can answer from without reading files.
pub const LEAN_FIELDS: &[&str] = &["id", "status", "type", "priority", "title"];

/// Whether the lean projection can satisfy a `--fields`/`fields` request.
///
/// It cannot serve what it does not select: `--fields id,created` against the
/// index returned `[{"id": …}]` with `created` silently absent, while the same
/// command with `--no-index` returned both — so the answer depended on whether
/// `.clove/index.db` happened to exist.
pub fn lean_can_serve(fields: Option<&[String]>) -> bool {
    match fields {
        None => true,
        Some(requested) => requested.iter().all(|k| LEAN_FIELDS.contains(&k.as_str())),
    }
}

/// Which tier answered a read. Reported verbatim as `_meta.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A running `cloved`.
    Daemon,
    /// The local `.clove/index.db`.
    Index,
    /// A scan of `.clove/issues/`.
    Files,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Daemon => "daemon",
            Source::Index => "index",
            Source::Files => "files",
        }
    }
}

/// A lean tier answer: the rows a tier returned (the `offset + limit` prefix,
/// or everything when a residue forced an unlimited fetch), the pre-window match
/// count, and any warnings.
type LeanAnswer = (Vec<LeanRow>, usize, Vec<String>);

/// A lean list row: the five columns a list renders, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanRow {
    pub id: String,
    pub status: String,
    pub item_type: String,
    pub priority: u8,
    pub title: String,
}

impl From<&ItemListRow> for LeanRow {
    fn from(r: &ItemListRow) -> Self {
        LeanRow {
            id: r.id.as_str().to_owned(),
            status: r.status.as_str().to_owned(),
            item_type: r.item_type.as_str().to_owned(),
            priority: r.priority,
            title: r.title.clone(),
        }
    }
}

impl From<&clove_ipc::LeanRow> for LeanRow {
    fn from(r: &clove_ipc::LeanRow) -> Self {
        LeanRow {
            id: r.id.clone(),
            status: r.status.clone(),
            item_type: r.item_type.clone(),
            priority: r.priority,
            title: r.title.clone(),
        }
    }
}

/// A full list row: the item's frontmatter, plus `blocked_by` for the one query
/// (`blocked`) whose whole point is that list.
#[derive(Debug, Clone)]
pub struct FullRow {
    pub frontmatter: ItemFrontmatter,
    /// Unclosed hard deps then dangling ones — `Some` only for `blocked`.
    pub blocked_by: Option<Vec<String>>,
}

impl FullRow {
    fn bare(frontmatter: ItemFrontmatter) -> FullRow {
        FullRow {
            frontmatter,
            blocked_by: None,
        }
    }
}

/// The rows of a list answer, in whichever shape the answering tier produced.
#[derive(Debug, Clone)]
pub enum Rows {
    /// The lean five columns — an index or daemon answer the caller accepted as
    /// lean (see [`Projection::Lean`]).
    Lean(Vec<LeanRow>),
    /// Full frontmatter, from a file scan or hydrated per page from a tier
    /// answer.
    Full(Vec<FullRow>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Rows::Lean(rows) => rows.len(),
            Rows::Full(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A windowed list answer: the page, the pre-window match count, the tier that
/// produced it, and any warnings to surface.
pub struct ListAnswer {
    pub source: Source,
    /// The requested page — already windowed, so a caller must not re-page it.
    pub rows: Rows,
    /// Match count **before** the window.
    pub total: usize,
    pub warnings: Vec<String>,
    /// The whole-store dependency graph, when the answering tier built one.
    ///
    /// Only [`Source::Files`] ever does: the index and daemon tiers answer in
    /// SQL and never construct it, which is most of what they save. The web
    /// renders `ready`/`blocked_by`/`dangling_deps` on every row and derives
    /// them from this when it is present — so the file tier stays a *single*
    /// scan-and-build rather than the engine building a graph for the ranks and
    /// the caller scanning and building a second one.
    pub graph: Option<GraphStore>,
}

/// A single-value answer (`show`, `comments`, `dep_tree`, `stats`) and the tier
/// that produced it.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    pub source: Source,
    pub value: T,
}

/// What the caller needs each row to carry — which is what decides whether a
/// tier answer is usable at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// The lean five columns suffice, so an index or daemon answer is returned
    /// as-is with no file reads at all. This is `clove ls`'s fast path.
    Lean,
    /// Full frontmatter is required. A tier still answers the *query* — the
    /// filtering, ordering, and counting happen in SQL — and only the page's
    /// items are then read from disk. This is what lets the MCP tools and the
    /// web keep their full-object output while gaining the tiers.
    Full,
    /// Full frontmatter is required **and** no tier may answer.
    ///
    /// This is a caller policy, not a property of the store: `clove ls --fields
    /// id,created` uses it because the CLI's contract is that a field outside
    /// the lean row falls back to the files rather than silently arriving from
    /// a different projection.
    Files,
}

impl Projection {
    fn allows_tiers(self) -> bool {
        !matches!(self, Projection::Files)
    }
}

/// Which tiers this engine may use.
#[derive(Debug, Clone, Copy)]
pub struct Tiers {
    /// May probe and route to a running `cloved`.
    ///
    /// The daemon-hosted web sets this `false`: it *is* the daemon, so the RPC
    /// would be a self-call across its own two-worker runtime for an answer it
    /// already has locally.
    pub daemon: bool,
    /// May read `.clove/index.db`.
    pub index: bool,
    /// Use the thorough per-file staleness pass (`--deep`) instead of the fast
    /// O(readdir) one.
    pub deep: bool,
    /// Freshen a lightly-stale index inline (`.clove/config`'s
    /// `index.auto_refresh`).
    pub auto_refresh: bool,
}

impl Default for Tiers {
    /// Every tier, fast staleness check, auto-refresh on — what a surface with
    /// no `--no-index` equivalent wants.
    fn default() -> Self {
        Tiers {
            daemon: true,
            index: true,
            deep: false,
            auto_refresh: true,
        }
    }
}

impl Tiers {
    /// Files only — the `--no-index` shape, and the safe default for a caller
    /// that has not decided.
    pub fn files_only() -> Tiers {
        Tiers {
            daemon: false,
            index: false,
            deep: false,
            auto_refresh: false,
        }
    }
}

/// The read engine: one store, one optional index, one cached daemon client.
///
/// Cheap to clone (the daemon client is shared behind an `Arc`), so a
/// long-lived server holds one and hands clones to its request handlers.
#[derive(Clone)]
pub struct Engine {
    store: ItemStore,
    issues_dir: Utf8PathBuf,
    clove_dir: Utf8PathBuf,
    db_path: Utf8PathBuf,
    tiers: Tiers,
    /// The cached daemon client. Probing builds a tokio runtime, connects, and
    /// pings — far too heavy to repeat per call on a long-lived server — so the
    /// client is kept and only its (cheap, on the open connection) `ping` is
    /// repeated; a dead or restarted daemon drops the cache and re-probes.
    daemon: Arc<Mutex<Option<DaemonClient>>>,
}

impl Engine {
    /// Build an engine for the repository whose `.clove/` is `clove_dir`.
    pub fn new(store: ItemStore, clove_dir: Utf8PathBuf, tiers: Tiers) -> Engine {
        let issues_dir = clove_dir.join("issues");
        let db_path = clove_dir.join("index.db");
        Engine {
            store,
            issues_dir,
            clove_dir,
            db_path,
            tiers,
            daemon: Arc::new(Mutex::new(None)),
        }
    }

    /// The file store this engine reads (surfaces still need it for writes and
    /// for their own detail rendering).
    pub fn store(&self) -> &ItemStore {
        &self.store
    }

    /// The `.clove/` directory.
    pub fn clove_dir(&self) -> &Utf8Path {
        &self.clove_dir
    }

    /// The `.clove/issues/` directory.
    pub fn issues_dir(&self) -> &Utf8Path {
        &self.issues_dir
    }

    /// The tiers in force.
    pub fn tiers(&self) -> Tiers {
        self.tiers
    }

    // ---- Lists ---------------------------------------------------------------

    /// All items matching `filters` (`clove ls`, `clove query`, `clove_list`,
    /// `GET /api/v1/items`).
    pub fn list(
        &self,
        filters: &Filters,
        order: Order,
        window: Page,
        projection: Projection,
    ) -> Result<ListAnswer, CloveError> {
        self.lean_query(
            QueryKind::List,
            QueryMode::List,
            filters,
            order,
            window,
            projection,
            || {
                let (rows, graph) = ops::list_rows_with_graph(&self.store, filters, order)?;
                Ok((
                    rows.into_iter().map(FullRow::bare).collect(),
                    Vec::new(),
                    graph,
                ))
            },
        )
    }

    /// Items eligible to start now (`clove ready`, `clove_ready`,
    /// `GET /api/v1/items?mode=ready`).
    pub fn ready(
        &self,
        filters: &Filters,
        order: Order,
        window: Page,
        projection: Projection,
    ) -> Result<ListAnswer, CloveError> {
        self.lean_query(
            QueryKind::Ready,
            QueryMode::Ready,
            filters,
            order,
            window,
            projection,
            || {
                let (rows, warnings, graph) =
                    ops::ready_rows_with_graph(&self.store, filters, order)?;
                Ok((
                    rows.into_iter().map(FullRow::bare).collect(),
                    warnings,
                    graph,
                ))
            },
        )
    }

    /// Items held up by an unclosed or missing hard dependency (`clove blocked`,
    /// `clove_blocked`, `GET /api/v1/items?mode=blocked`).
    ///
    /// Always returns [`Rows::Full`], whatever `projection` asks for: the whole
    /// point of this list is `blocked_by`, which no lean row carries. A
    /// `projection` of [`Projection::Files`] still pins it to the file scan.
    ///
    /// The daemon tier answers from its cached graph (ordered ids); the index
    /// tier — new in read-path §4, this was the one list that could not answer
    /// from SQL at all — runs [`QueryMode::Blocked`], the exact complement of
    /// the `ready` clause. Both are then hydrated for the page only.
    pub fn blocked(
        &self,
        filters: &Filters,
        order: Order,
        window: Page,
        projection: Projection,
    ) -> Result<ListAnswer, CloveError> {
        if projection.allows_tiers() {
            // Daemon: the *set* comes from its cached graph and the *order* from
            // its index, so the ids arrive already sorted. Filtering preserves
            // that order.
            if let Some(ids) = self.blocked_ids_via_daemon(order) {
                let matched: Vec<ItemFrontmatter> = self
                    .hydrate(ids.iter().map(String::as_str))
                    .into_iter()
                    .filter(|fm| filters.matches(fm))
                    .collect();
                let total = matched.len();
                let (page, _) = window.apply(matched);
                return Ok(ListAnswer {
                    source: Source::Daemon,
                    rows: Rows::Full(self.with_blocked_by(page)?),
                    total,
                    warnings: Vec::new(),
                    graph: None,
                });
            }
            if let Some((rows, total, warnings)) =
                self.lean_via_index(QueryMode::Blocked, filters, order, window)?
            {
                let (page, _) = window.apply(rows);
                let hydrated = self.hydrate(page.iter().map(|row| row.id.as_str()));
                return Ok(ListAnswer {
                    source: Source::Index,
                    rows: Rows::Full(self.with_blocked_by(hydrated)?),
                    total,
                    warnings,
                    graph: None,
                });
            }
        }

        let (rows, graph) = ops::blocked_rows_with_graph(&self.store, filters, order)?;
        let total = rows.len();
        let (page, _) = window.apply(rows);
        Ok(ListAnswer {
            source: Source::Files,
            rows: Rows::Full(
                page.into_iter()
                    .map(|(frontmatter, blocked_by)| FullRow {
                        frontmatter,
                        blocked_by: Some(blocked_by),
                    })
                    .collect(),
            ),
            total,
            warnings: Vec::new(),
            graph: Some(graph),
        })
    }

    /// Case-insensitive substring search over title/labels/body.
    ///
    /// **There is deliberately no index or daemon tier here**, and `projection`
    /// is therefore irrelevant: FTS matched whole ASCII-folded tokens where
    /// `view::match_class` matches Unicode substrings, so an index tier answered
    /// a narrower question than the file scan it stood in for. Index schema 6
    /// dropped the FTS and IPC v6 dropped the `search` RPC; the measurements
    /// that decided it are in read-path roadmap §6.1. [`Source::Files`] is
    /// therefore always honest here rather than merely a fallback.
    pub fn search(
        &self,
        text: &str,
        order: SearchOrder,
        window: Page,
    ) -> Result<ListAnswer, CloveError> {
        let rows = ops::search_rows(&self.store, text, order)?;
        let total = rows.len();
        let (page, _) = window.apply(rows);
        Ok(ListAnswer {
            source: Source::Files,
            rows: Rows::Full(page.into_iter().map(FullRow::bare).collect()),
            total,
            warnings: Vec::new(),
            // Search ranks its own hits and needs no graph unless `--sort rank`
            // asked for one, which `ops::search_rows` builds and consumes.
            graph: None,
        })
    }

    // ---- Single-value reads --------------------------------------------------

    /// Full item detail: frontmatter + body + `comment_count` + `ready`/
    /// `blocked_by`, the `clove show --format json` shape.
    ///
    /// The daemon's `show` RPC calls the same `ops::show`, so a daemon answer is
    /// byte-identical to a local one; there is no index tier because the index
    /// stores neither the body nor the comment thread.
    pub fn show(&self, id: &CloveId) -> Result<Answer<Value>, CloveError> {
        if self.tiers.daemon {
            if let Some(value) = self.with_daemon(|d| d.show(id.to_string())) {
                return Ok(Answer {
                    source: Source::Daemon,
                    value: value?,
                });
            }
        }
        Ok(Answer {
            source: Source::Files,
            value: ops::show(&self.store, id)?,
        })
    }

    /// An item's comment thread, windowed from the newest end.
    ///
    /// Files only: comments live in per-item directories that neither the index
    /// nor the daemon mirrors, so there is nothing to tier.
    pub fn comments(&self, id: &CloveId, window: Page) -> Result<Answer<Value>, CloveError> {
        Ok(Answer {
            source: Source::Files,
            value: ops::comments(&self.store, id, window)?,
        })
    }

    /// The dependency tree rooted at `id`, to `depth` (`usize::MAX` for all).
    pub fn dep_tree(&self, id: &CloveId, depth: usize) -> Result<Answer<Value>, CloveError> {
        if self.tiers.daemon {
            if let Some(Ok(GraphResponse::Tree { node: Some(node) })) = self.with_daemon(|d| {
                d.graph(GraphRequest::Tree {
                    root: id.to_string(),
                    depth,
                })
            }) {
                return Ok(Answer {
                    source: Source::Daemon,
                    value: tree_to_json(&node),
                });
            }
        }
        Ok(Answer {
            source: Source::Files,
            value: ops::dep_tree(&self.store, id, depth)?,
        })
    }

    /// The work-item analytics report.
    pub fn stats(
        &self,
        top: usize,
        include_epics: bool,
        now: DateTime<Utc>,
    ) -> Result<Answer<Value>, CloveError> {
        if self.tiers.daemon {
            if let Some(value) = self.with_daemon(|d| d.stats(top as u32, include_epics)) {
                return Ok(Answer {
                    source: Source::Daemon,
                    value: value?,
                });
            }
        }
        Ok(Answer {
            source: Source::Files,
            value: ops::stats(&self.store, top, include_epics, now)?,
        })
    }

    // ---- The cascade ---------------------------------------------------------

    /// The shared daemon → index → files cascade for the three lean-capable
    /// lists (`list`, `ready`, and — through [`Self::blocked`] — the file half of
    /// `blocked`).
    ///
    /// `file_rows` is the file tier, passed as a closure so this function owns
    /// the tier *choice* while each list keeps its own file-path definition.
    #[allow(clippy::too_many_arguments)]
    fn lean_query(
        &self,
        kind: QueryKind,
        mode: QueryMode,
        filters: &Filters,
        order: Order,
        window: Page,
        projection: Projection,
        file_rows: impl FnOnce() -> Result<(Vec<FullRow>, Vec<String>, GraphStore), CloveError>,
    ) -> Result<ListAnswer, CloveError> {
        if projection.allows_tiers() {
            if let Some((rows, total, warnings)) =
                self.lean_via_daemon(kind, filters, order, window)
            {
                return self.finish_lean(Source::Daemon, rows, total, warnings, window, projection);
            }
            if let Some((rows, total, warnings)) =
                self.lean_via_index(mode, filters, order, window)?
            {
                return self.finish_lean(Source::Index, rows, total, warnings, window, projection);
            }
        }
        let (rows, warnings, graph) = file_rows()?;
        let total = rows.len();
        let (page, _) = window.apply(rows);
        Ok(ListAnswer {
            source: Source::Files,
            rows: Rows::Full(page),
            total,
            warnings,
            graph: Some(graph),
        })
    }

    /// Window a tier's lean rows and, for [`Projection::Full`], read the page's
    /// item files.
    ///
    /// Hydration happens **after** the window, which is the whole reason a
    /// full-object surface can afford a tier at all: the filtering, ordering,
    /// and counting stay in SQL and only the page touches the disk.
    fn finish_lean(
        &self,
        source: Source,
        rows: Vec<LeanRow>,
        total: usize,
        warnings: Vec<String>,
        window: Page,
        projection: Projection,
    ) -> Result<ListAnswer, CloveError> {
        // The tier returns the `offset + limit` prefix (or, when a residue
        // forced an unlimited fetch, everything), so the offset is applied here.
        let (page, _) = window.apply(rows);
        let rows = match projection {
            Projection::Lean => Rows::Lean(page),
            // `Files` never reaches here (`allows_tiers` is false), but treating
            // it as `Full` keeps this total rather than a panic.
            Projection::Full | Projection::Files => Rows::Full(
                self.hydrate(page.iter().map(|row| row.id.as_str()))
                    .into_iter()
                    .map(FullRow::bare)
                    .collect(),
            ),
        };
        Ok(ListAnswer {
            source,
            rows,
            total,
            warnings,
            graph: None,
        })
    }

    /// Try to satisfy a lean list query via a running daemon.
    ///
    /// The daemon keeps its own index fresh, so this path skips the CLI's
    /// staleness scan. Returns `None` — so the caller falls through to the index
    /// — for a disabled tier, no daemon, or any IPC error.
    fn lean_via_daemon(
        &self,
        kind: QueryKind,
        filters: &Filters,
        order: Order,
        window: Page,
    ) -> Option<LeanAnswer> {
        if !self.tiers.daemon {
            return None;
        }
        let request = query_request(kind, filters, order, window);
        let response = self.with_daemon(|d| d.query_list(request.clone()))?.ok()?;
        Some((
            response.rows.iter().map(LeanRow::from).collect(),
            response.total as usize,
            response.warnings,
        ))
    }

    /// Ask a running daemon for the blocked ids, already in `order`.
    fn blocked_ids_via_daemon(&self, order: Order) -> Option<Vec<String>> {
        if !self.tiers.daemon {
            return None;
        }
        match self.with_daemon(|d| d.graph(GraphRequest::Blocked { order }))? {
            Ok(GraphResponse::Blocked { ids }) => Some(ids),
            _ => None,
        }
    }

    /// Try to satisfy a lean list query from the local index.
    ///
    /// Returns `None` to fall back to the files: a disabled tier, a missing or
    /// broken index, one too stale to refresh cheaply, or a query SQLite cannot
    /// *shape* (see [`query_too_complex`]).
    fn lean_via_index(
        &self,
        mode: QueryMode,
        filters: &Filters,
        order: Order,
        window: Page,
    ) -> Result<Option<LeanAnswer>, CloveError> {
        if !self.tiers.index || !self.db_path.exists() {
            return Ok(None);
        }
        let mut index = match Index::open_or_rebuild(&self.db_path, &self.issues_dir) {
            Ok(index) => index,
            // A broken index is non-fatal: fall back to files.
            Err(_) => return Ok(None),
        };

        if self.tiers.auto_refresh {
            let report = if self.tiers.deep {
                index.check_staleness(&self.issues_dir)
            } else {
                index.check_staleness_fast(&self.issues_dir)
            }
            .map_err(|e| index_error(e, &self.db_path))?;

            if report.change_count() > STALE_REFRESH_LIMIT {
                // Too far behind to refresh inline — use the files instead.
                return Ok(None);
            }
            if !report.is_clean() {
                index
                    .apply_staleness(&report, &self.issues_dir)
                    .map_err(|e| index_error(e, &self.db_path))?;
            }
        }

        // The exhaustive push-down: SQL for what SQLite can express, an
        // in-memory residue for what it cannot.
        let (mut filter, residue) = clove_index::push_down(filters);
        filter.mode = mode;
        // The SQL `ORDER BY` must match the file path's comparator exactly: with
        // no residue the limit is pushed into SQL, so a mismatched order returns
        // the wrong *rows*, not just the wrong sequence.
        filter.order = order;
        // `query_filtered` owns the limit/count decision, because a residue
        // changes both. A query the index cannot *shape* falls back to the files
        // rather than failing: each AND-ed label is its own `EXISTS` subquery
        // and SQLite caps expression depth, so ~997 repeated `--label` values
        // error out where `--no-index` answers normally. The index is a cache;
        // "this query is too big for SQLite" is a reason to use the other path,
        // not to refuse.
        match index.query_filtered(&filter, residue.as_ref(), window) {
            Ok((rows, total)) => Ok(Some((
                rows.iter().map(LeanRow::from).collect(),
                total,
                Vec::new(),
            ))),
            Err(e) if query_too_complex(&e) => Ok(None),
            Err(e) => Err(index_error(e, &self.db_path)),
        }
    }

    /// Run `call` against the daemon, if one is reachable.
    ///
    /// `None` → no daemon, so the caller falls through to the next tier. The
    /// call itself is attempted exactly once; only the *liveness check*
    /// re-probes.
    fn with_daemon<T>(
        &self,
        call: impl FnOnce(&mut DaemonClient) -> Result<T, clove_ipc::ClientError>,
    ) -> Option<Result<T, CloveError>> {
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
        Some(call(client).map_err(CloveError::from))
    }

    // ---- Hydration -----------------------------------------------------------

    /// Read the frontmatter of each id, in order, skipping any that is gone.
    ///
    /// This is what makes a tier affordable for a full-object surface: the
    /// filtering, ordering, and counting happened in SQL, so only these files
    /// are touched. It reads **frontmatter only** — no bodies — and in parallel
    /// above the same threshold `ItemStore::scan_frontmatter` uses, so hydrating
    /// a page is never dearer per file than the whole-store scan it replaces.
    ///
    /// A tier answer can name an item the files no longer have (the daemon's
    /// graph or the index can be a moment ahead of a deletion), and a read is
    /// not the place to fail over that — the row is simply dropped, which is
    /// what the file scan would have done.
    fn hydrate<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Vec<ItemFrontmatter> {
        let paths: Vec<Utf8PathBuf> = ids
            .filter_map(|id| CloveId::new(id).ok())
            .map(|id| self.store.path_for(&id))
            .collect();
        let read = |path: &Utf8PathBuf| clove_core::parse_frontmatter_file(path).ok();
        if paths.len() > PARALLEL_HYDRATE_THRESHOLD {
            paths.par_iter().filter_map(read).collect()
        } else {
            paths.iter().filter_map(read).collect()
        }
    }

    /// Attach `blocked_by` to each row of a hydrated page.
    ///
    /// `ops::graph_terms` answers from the item's own dependency closure, so
    /// this is O(page) rather than a second whole-store graph build — and it is
    /// the same helper `clove show` and the MCP tools use, so the ids cannot
    /// drift between surfaces.
    fn with_blocked_by(&self, page: Vec<ItemFrontmatter>) -> Result<Vec<FullRow>, CloveError> {
        page.into_iter()
            .map(|fm| {
                let (_, blocked_by) = ops::graph_terms(&self.store, &fm)?;
                Ok(FullRow {
                    frontmatter: fm,
                    blocked_by: Some(blocked_by),
                })
            })
            .collect()
    }
}

/// Build the `query` RPC payload.
///
/// Split out because the window it carries has **no observable effect on the
/// answer** — the engine applies the window again to whatever comes back, so a
/// request that asked for the whole store still renders the right page. The only
/// symptom of dropping it is bytes on the socket, which no end-to-end test can
/// see, so it is pinned here instead.
fn query_request(kind: QueryKind, filters: &Filters, order: Order, window: Page) -> QueryRequest {
    QueryRequest {
        kind,
        // The whole shared filter set rides the wire as one field, so a filter
        // added to `view::Filters` cannot be dropped in a per-field translation
        // on the way to the daemon.
        filters: filters.clone(),
        order,
        offset: window.offset,
        limit: window.limit,
    }
}

/// Whether an index error is SQLite refusing the *shape* of the query rather
/// than reporting a broken store. These are recoverable by asking the files.
pub fn query_too_complex(e: &clove_index::IndexError) -> bool {
    let msg = e.to_string();
    msg.contains("Expression tree is too large")
        || msg.contains("too many SQL variables")
        || msg.contains("parser stack overflow")
}

/// Surface a `clove-index` error through `CloveError` so a caller's exit-code
/// mapping applies (index errors map to the I/O class).
fn index_error(err: clove_index::IndexError, path: &Utf8Path) -> CloveError {
    CloveError::Io {
        path: path.to_owned(),
        source: std::io::Error::other(err.to_string()),
    }
}

/// Render a daemon-returned dep-tree node in the same JSON shape `ops::dep_tree`
/// produces, so the two tiers are indistinguishable to a caller.
fn tree_to_json(node: &clove_core::graph::DepTreeNode) -> Value {
    serde_json::json!({
        "id": node.id.as_str(),
        "title": node.title,
        "status": node.status.as_str(),
        "ready": node.ready,
        "cycle_ref": node.cycle_ref,
        "repeat_ref": node.repeat_ref,
        "children": node.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_core::view::SortField;

    /// The window, the filters, and the order all ride the `query` request.
    ///
    /// A daemon that is not told the window answers with the whole match set,
    /// and the engine then windows it locally — so every end-to-end assertion
    /// still passes while `clove ls --limit 1` drags the store across a socket.
    /// That is the request-side twin of the response-side truncation pinned by
    /// `a_residue_does_not_ship_the_whole_match_set` (crates/cloved).
    #[test]
    fn the_query_request_carries_the_window_the_filters_and_the_order() {
        let filters =
            Filters::parse_multi(&["open".into()], &[], &[], None, &[], Some("x")).unwrap();
        let order = Order {
            field: SortField::Updated,
            descending: true,
        };
        let req = query_request(QueryKind::Ready, &filters, order, Page::new(20, Some(5), 0));
        assert_eq!(req.kind, QueryKind::Ready);
        assert_eq!(req.offset, 20, "the daemon must not be asked from row 0");
        assert_eq!(req.limit, Some(5), "…nor for more rows than the page needs");
        assert_eq!(req.order, order);
        assert_eq!(req.filters, filters);

        // `--limit 0` is unlimited, and that must reach the wire as `None`
        // rather than as a zero-row request.
        let all = query_request(
            QueryKind::List,
            &Filters::default(),
            Order::default(),
            Page::new(0, Some(0), 100),
        );
        assert_eq!(all.limit, None);
    }
}
