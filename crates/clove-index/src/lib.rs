//! clove optional SQLite index (M1).
//!
//! Mirrors the file store into a SQLite cache for fast filtered/sorted list
//! queries at scale. Fully rebuildable from the files and `.gitignore`d —
//! deleting it loses nothing; the files remain the single source of truth
//! (DESIGN §6).
//!
//! **Not full-text search.** Schemas 1–5 carried an FTS5 mirror that
//! `clove search` used as a prefilter; schema 6 removed it, because FTS5 matches
//! whole ASCII-folded tokens where every other clove surface matches Unicode
//! substrings, so `clove search X` answered differently depending on whether an
//! index existed. Search is a parallel file scan on every surface now — see
//! `docs/READ_PATH_ROADMAP.md` §6.1.
//!
//! Layers:
//! - [`db`] — schema, connection lifecycle, row types ([`Index`], [`ItemRow`]).
//! - [`write`] — the single encapsulated upsert path that keeps the side
//!   tables in sync.
//! - [`stale`] — two-level staleness detection and incremental resync.
//! - [`reindex`] — atomic full rebuild (tmp file + rename, advisory lock).
//! - [`query`] — index-path `ready`/`ls`/`query` reads.
//!
//! The CLI-facing wrappers (the `clove reindex` command, the read-path
//! `with_index` guard, and the `doctor` divergence check) are layered on top once
//! the M0 command surface exists.

pub mod db;
pub mod derive;
pub mod query;
pub mod reindex;
pub mod stale;
pub mod stats_store;
pub mod write;

pub use db::{Index, IndexError, ItemListRow, ItemRow, SCHEMA_VERSION};
pub use query::{
    count_items, push_down, query_filtered, query_items, query_list, Filter, PostFilter, QueryMode,
};
pub use reindex::{reindex, ReindexReport};
pub use stale::{apply_staleness, check_staleness, check_staleness_fast, StalenessReport};
pub use stats_store::StatsSnapshot;
pub use write::upsert_item;
