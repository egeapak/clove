//! clove importers/exporters (M2+).
//!
//! File-based importers (beads, tk) use only `clove-core`; the GitHub
//! importer/exporter uses `tokio` + `octocrab` behind the `github` cargo
//! feature. This crate provides the shared scaffolding — the [`Importer`] trait,
//! the [`ImportPlan`]/[`ImportReport`] planning types, the field-mapping helpers
//! in [`map`], and the `external_ref` idempotency index — that every concrete
//! importer reuses. See DESIGN.md §11.

pub mod beads;
pub mod beads_export;
pub mod error;
pub mod export;
pub mod github;
pub mod map;
pub mod merge;
pub mod plan;
pub mod render;
pub mod restore;
pub mod sync;
#[cfg(feature = "github")]
pub mod sync_net;
pub mod tk;

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use clove_core::ItemStore;
use clove_types::{CloveId, ItemStatus, Priority};

pub use beads::BeadsImporter;
pub use beads_export::{build_beads_object, export_beads};
pub use error::ImportError;
pub use map::{build_external_ref_index, build_store_id_set, ExistingItem};
pub use plan::{ConflictItem, ImportPlan, ImportReport, PlanItem, SkipItem};
pub use restore::{
    apply_restore, parse_export_json, parse_export_jsonl, plan_restore, RestorePlan, RestoreReport,
    EXPORT_FORMAT_VERSION,
};
pub use sync::{
    plan_sync, ConflictPolicy, Direction, SyncConflict, SyncPlan, SyncReport, SyncState,
    SyncSummary,
};
pub use tk::TkImporter;

/// Shared context handed to [`Importer::plan`].
///
/// Holds the prebuilt `external_ref → CloveId` idempotency index (so the plan
/// can decide which incoming items to skip without rescanning) and the
/// `dry_run` flag (informational at plan time; the CLI also uses it to decide
/// whether to call [`Importer::apply`]).
#[derive(Debug)]
pub struct ImportCtx {
    /// `external_ref → existing item`, built once via
    /// [`build_external_ref_index`] and reused by every importer. The
    /// [`ExistingItem`] carries the existing item's key fields so re-imports can
    /// report field-level `conflicts` (DESIGN §11.3).
    pub external_refs: HashMap<String, ExistingItem>,
    /// The set of all existing clove ids in the store, used to flag dangling
    /// dependency targets on import (M4): a `deps`/`parent`/`relates` id present
    /// in neither this set nor the current import batch is reported.
    pub store_ids: HashSet<CloveId>,
    /// Whether this is a dry run (plan only, no writes).
    pub dry_run: bool,
}

impl ImportCtx {
    /// Build a context by scanning `store` for existing `external_ref`s and ids.
    pub fn new(store: &ItemStore, dry_run: bool) -> Result<Self, ImportError> {
        Ok(Self {
            external_refs: build_external_ref_index(store)?,
            store_ids: build_store_id_set(store)?,
            dry_run,
        })
    }

    /// Whether an incoming `external_ref` already maps to an existing item.
    pub fn is_imported(&self, external_ref: &str) -> bool {
        self.external_refs.contains_key(external_ref)
    }

    /// The already-imported item for `external_ref`, if any.
    pub fn existing(&self, external_ref: &str) -> Option<&ExistingItem> {
        self.external_refs.get(external_ref)
    }

    /// Field-level conflicts (DESIGN §11.3) between an incoming item and the
    /// already-imported item sharing `external_ref`. Empty when the ref is new
    /// or the compared fields (status, priority, title) all match.
    pub fn conflicts_for(
        &self,
        external_ref: &str,
        source_id: &str,
        status: ItemStatus,
        priority: Priority,
        title: &str,
    ) -> Vec<ConflictItem> {
        match self.external_refs.get(external_ref) {
            Some(existing) => existing.conflicts_with(source_id, status, priority, title),
            None => Vec::new(),
        }
    }
}

/// A source-specific importer.
///
/// [`plan`](Importer::plan) is pure (no writes) and drives `--dry-run`;
/// [`apply`](Importer::apply) performs the writes through the existing
/// [`ItemStore`] atomic write path. The CLI runs `plan` always, and additionally
/// `apply` when not in dry-run.
pub trait Importer {
    /// Compute the write-free plan for importing from `src`.
    fn plan(&self, src: &Utf8Path, ctx: &ImportCtx) -> Result<ImportPlan, ImportError>;

    /// Apply a previously computed `plan`, writing items into `store`.
    fn apply(&self, plan: ImportPlan, store: &ItemStore) -> Result<ImportReport, ImportError>;
}
