//! clove importers/exporters (M2+).
//!
//! File-based importers (beads, tk) use only `clove-core`; the GitHub
//! importer/exporter uses `tokio` + `octocrab` behind the `github` cargo
//! feature. This crate provides the shared scaffolding — the [`Importer`] trait,
//! the [`ImportPlan`]/[`ImportReport`] planning types, the field-mapping helpers
//! in [`map`], and the `external_ref` idempotency index — that every concrete
//! importer reuses. See `docs/M2_PLAN.md` §2 and DESIGN.md §11.

pub mod beads;
pub mod error;
pub mod export;
pub mod map;
pub mod merge;
pub mod plan;
pub mod tk;

use std::collections::HashMap;

use camino::Utf8Path;
use clove_core::{CloveId, ItemStore};

pub use beads::BeadsImporter;
pub use error::ImportError;
pub use map::build_external_ref_index;
pub use plan::{ConflictItem, ImportPlan, ImportReport, PlanItem, SkipItem};
pub use tk::TkImporter;

/// Shared context handed to [`Importer::plan`].
///
/// Holds the prebuilt `external_ref → CloveId` idempotency index (so the plan
/// can decide which incoming items to skip without rescanning) and the
/// `dry_run` flag (informational at plan time; the CLI also uses it to decide
/// whether to call [`Importer::apply`]).
#[derive(Debug)]
pub struct ImportCtx {
    /// `external_ref → existing CloveId`, built once via
    /// [`build_external_ref_index`] and reused by every importer.
    pub external_refs: HashMap<String, CloveId>,
    /// Whether this is a dry run (plan only, no writes).
    pub dry_run: bool,
}

impl ImportCtx {
    /// Build a context by scanning `store` for existing `external_ref`s.
    pub fn new(store: &ItemStore, dry_run: bool) -> Result<Self, ImportError> {
        Ok(Self {
            external_refs: build_external_ref_index(store)?,
            dry_run,
        })
    }

    /// Whether an incoming `external_ref` already maps to an existing item.
    pub fn is_imported(&self, external_ref: &str) -> bool {
        self.external_refs.contains_key(external_ref)
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
