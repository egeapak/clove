//! Import planning types (DESIGN.md §11.3).
//!
//! An [`ImportPlan`] is the pure, write-free result of [`crate::Importer::plan`]:
//! it is what `--dry-run` serializes verbatim as the `{would_create, would_skip,
//! conflicts}` envelope. [`ImportReport`] summarizes what
//! [`crate::Importer::apply`] actually wrote.

use serde::Serialize;

/// An item the importer would create (it has no matching `external_ref` yet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanItem {
    /// The id the item would be created under (the incoming/source id).
    pub id: String,
    /// The item title.
    pub title: String,
}

/// An incoming item the importer would skip, with a machine-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkipItem {
    /// The incoming/source id that was skipped.
    pub id: String,
    /// Why it was skipped (e.g. `"already_imported"`).
    pub reason: String,
}

/// A field-level conflict between an incoming item and an existing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictItem {
    /// The incoming/source id in conflict.
    pub id: String,
    /// The field whose values disagree (e.g. `"status"`).
    pub field: String,
    /// The value currently stored.
    pub existing: String,
    /// The value the import would set.
    pub incoming: String,
}

/// The write-free plan an importer produces; the `--dry-run` payload.
///
/// Serializes to the DESIGN §11.3 shape:
/// `{ "would_create": [...], "would_skip": [...], "conflicts": [...] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportPlan {
    /// Items that would be newly created.
    pub would_create: Vec<PlanItem>,
    /// Items that would be skipped (already imported, etc.).
    pub would_skip: Vec<SkipItem>,
    /// Field-level conflicts detected against existing items.
    pub conflicts: Vec<ConflictItem>,
}

impl ImportPlan {
    /// An empty plan.
    pub fn new() -> Self {
        Self::default()
    }
}

/// A summary of what an [`crate::Importer::apply`] run actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Number of items created.
    pub created: usize,
    /// Number of incoming items skipped (already imported, etc.).
    pub skipped: usize,
    /// Number of field-level conflicts encountered.
    pub conflicts: usize,
}
