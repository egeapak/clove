//! Shared field-mapping helpers used by every importer (DESIGN.md §11).
//!
//! This is the single coercion point so all importers agree on `task → chore`,
//! status/priority coercion, and label normalization, and the file store only
//! ever receives canonical, valid items. It also owns the `external_ref`
//! idempotency index ([`build_external_ref_index`]) that every importer reuses.

use std::collections::HashMap;

use clove_core::{normalize_label, CloveId, ItemStatus, ItemStore, ItemType, Priority};

use crate::error::ImportError;

/// Coerce a source "type" string to a clove [`ItemType`].
///
/// Notably maps tk/Beads `task` → [`ItemType::Chore`] (DESIGN §11.1/§11.2).
/// Unknown values fall back to [`ItemType`]'s default ([`ItemType::Feature`]).
pub fn tk_type(raw: &str) -> ItemType {
    match raw.trim().to_lowercase().as_str() {
        "task" | "chore" => ItemType::Chore,
        "bug" | "defect" => ItemType::Bug,
        "feature" | "enhancement" => ItemType::Feature,
        "docs" | "doc" | "documentation" => ItemType::Docs,
        "epic" => ItemType::Epic,
        _ => ItemType::default(),
    }
}

/// Coerce a source "status" string to a clove [`ItemStatus`].
///
/// Unrecognized statuses default to [`ItemStatus::Open`]. (Beads' `deferred →
/// open + label "deferred"` special case is handled in the Beads importer, not
/// here, because it also injects a label.)
pub fn coerce_status(raw: &str) -> ItemStatus {
    match raw.trim().to_lowercase().as_str() {
        "in_progress" | "in-progress" | "inprogress" | "started" | "doing" => {
            ItemStatus::InProgress
        }
        "closed" | "done" | "resolved" | "completed" => ItemStatus::Closed,
        _ => ItemStatus::Open,
    }
}

/// Coerce a numeric source priority into a valid clove [`Priority`], clamping to
/// the `0..=Priority::MAX` range rather than erroring.
pub fn coerce_priority(raw: i64) -> Priority {
    let clamped = raw.clamp(0, i64::from(Priority::MAX)) as u8;
    Priority(clamped)
}

/// Normalize a single label via the one canonicalization point in
/// `clove-core` (passthrough, surfacing [`ImportError`] on an empty label).
pub fn map_label(raw: &str) -> Result<String, ImportError> {
    Ok(normalize_label(raw)?)
}

/// Normalize a list of labels, dropping nothing and preserving order.
pub fn map_labels<I, S>(raw: I) -> Result<Vec<String>, ImportError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    raw.into_iter().map(|l| map_label(l.as_ref())).collect()
}

/// Scan the store and build the `external_ref → CloveId` index used by every
/// importer for idempotency: an incoming item whose `external_ref` is already a
/// key here is skipped on re-import (DESIGN §11.3, "skip items where
/// `external_ref` matches an existing item").
///
/// Items without an `external_ref` contribute no entry. On a duplicate
/// `external_ref` (should not happen in a healthy store) the first one scanned
/// wins; per-file parse failures are ignored here (they surface through the
/// normal `doctor`/scan paths).
pub fn build_external_ref_index(
    store: &ItemStore,
) -> Result<HashMap<String, CloveId>, ImportError> {
    let (frontmatters, _scan_errors) = store.scan_frontmatter()?;
    let mut index = HashMap::new();
    for fm in frontmatters {
        if let Some(external_ref) = fm.external_ref {
            index.entry(external_ref).or_insert(fm.id);
        }
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_maps_to_chore() {
        assert_eq!(tk_type("task"), ItemType::Chore);
        assert_eq!(tk_type("TASK"), ItemType::Chore);
        assert_eq!(tk_type("bug"), ItemType::Bug);
        assert_eq!(tk_type("nonsense"), ItemType::default());
    }

    #[test]
    fn status_coercion() {
        assert_eq!(coerce_status("in_progress"), ItemStatus::InProgress);
        assert_eq!(coerce_status("done"), ItemStatus::Closed);
        assert_eq!(coerce_status("whatever"), ItemStatus::Open);
    }

    #[test]
    fn priority_clamps() {
        assert_eq!(coerce_priority(-3), Priority(0));
        assert_eq!(coerce_priority(2), Priority(2));
        assert_eq!(coerce_priority(99), Priority(Priority::MAX));
    }

    #[test]
    fn label_passthrough_normalizes() {
        assert_eq!(map_label("  AREA:IOS  ").unwrap(), "area:ios");
        assert!(map_label("   ").is_err());
    }
}
