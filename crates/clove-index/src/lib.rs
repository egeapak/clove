//! clove optional SQLite index (M1).
//!
//! Mirrors the file store into an FTS5-backed SQLite cache for fast queries at
//! scale. Fully rebuildable from the files and `.gitignore`d — deleting it loses
//! nothing; the files remain the single source of truth (DESIGN §6).
//!
//! Layers:
//! - [`db`] — schema, connection lifecycle, row types ([`Index`], [`ItemRow`]).
//! - [`write`] — the single encapsulated upsert path that keeps FTS5 in sync.
//! - [`stale`] — two-level staleness detection and incremental resync.
//! - [`reindex`] — atomic full rebuild (tmp file + rename, advisory lock).
//! - [`query`] — index-path `ready`/`ls`/`query` reads.
//!
//! The CLI-facing wrappers (`clove reindex`/`search` commands, the read-path
//! `with_index` guard, and the `doctor` divergence check) are layered on top once
//! the M0 command surface exists (IMPLEMENTATION_PLAN T-S04/S05/S06/S08).

pub mod db;
pub mod derive;
pub mod query;
pub mod reindex;
pub mod stale;
pub mod stats_store;
pub mod write;

pub use db::{Index, IndexError, ItemListRow, ItemRow, SCHEMA_VERSION};
pub use query::{count_items, query_items, query_list, search, Filter, QueryMode};
pub use reindex::{reindex, ReindexReport};
pub use stale::{apply_staleness, check_staleness, check_staleness_fast, StalenessReport};
pub use stats_store::StatsSnapshot;
pub use write::upsert_item;
