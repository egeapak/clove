//! Shared field-mapping helpers used by every importer (DESIGN.md §11).
//!
//! This is the single coercion point so all importers agree on `task → chore`,
//! status/priority coercion, and label normalization, and the file store only
//! ever receives canonical, valid items. It also owns the `external_ref`
//! idempotency index ([`build_external_ref_index`]) that every importer reuses.

use std::collections::{HashMap, HashSet};

use clove_core::limits::{MAX_BODY_BYTES, MAX_DEP_ARRAY_LEN, MAX_FRONTMATTER_BYTES};
use clove_core::{normalize_label, CloveId, ItemStatus, ItemStore, ItemType, Priority};

use crate::error::ImportError;
use crate::plan::ConflictItem;

/// Maximum size, in bytes, of a single import source unit handed to a parser —
/// one tk `*.md` file, or one beads JSONL line. Reuses clove-core's own parse
/// ceiling (frontmatter + body budget plus a small slack) so importers never
/// feed an unbounded buffer to `serde_yaml_neo`/`serde_json` (M5). Foreign input
/// exceeding this is rejected cleanly rather than allocated/parsed.
pub const MAX_IMPORT_UNIT_BYTES: usize = MAX_FRONTMATTER_BYTES + MAX_BODY_BYTES + 4096;

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

/// Coerce a Beads `issue_type` (or a clove `type`) string to a clove
/// [`ItemType`]. Identical mapping to [`tk_type`] (notably `task → chore`,
/// DESIGN §11.2); kept as a named alias so each importer reads against its own
/// spec.
pub fn beads_type(raw: &str) -> ItemType {
    tk_type(raw)
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

/// Enforce [`MAX_DEP_ARRAY_LEN`] on a parsed dependency/relation array at map
/// time (M4). If `ids` exceeds the cap it is truncated to the first
/// `MAX_DEP_ARRAY_LEN` entries and a warning describing the truncation is pushed
/// onto `warnings`; otherwise `ids` is returned unchanged. Truncating (rather
/// than erroring) keeps a single over-long field from aborting an otherwise good
/// import, while guaranteeing the written file never violates the store's own
/// validation cap. `field` (e.g. `"deps"`) and `source_id` are only for the
/// warning text.
pub fn cap_dep_array(
    mut ids: Vec<CloveId>,
    field: &str,
    source_id: &str,
    warnings: &mut Vec<String>,
) -> Vec<CloveId> {
    if ids.len() > MAX_DEP_ARRAY_LEN {
        let original = ids.len();
        ids.truncate(MAX_DEP_ARRAY_LEN);
        warnings.push(format!(
            "item `{source_id}`: `{field}` has {original} entries, exceeding the cap of {MAX_DEP_ARRAY_LEN}; truncated to {MAX_DEP_ARRAY_LEN}"
        ));
    }
    ids
}

/// Collect the dangling dependency targets of one mapped item (M4): the
/// `parent`/`deps`/`relates` ids that reference neither an id present in
/// `known_ids` (the existing store ids plus the ids of every other item in this
/// same import batch) nor itself. A non-empty result is surfaced as a warning;
/// the write still proceeds (report-only policy). Uses set lookups, so it is
/// `O(deps)` per item.
pub fn dangling_targets<'a, I>(known_ids: &HashSet<CloveId>, targets: I) -> Vec<CloveId>
where
    I: IntoIterator<Item = &'a CloveId>,
{
    let mut dangling: Vec<CloveId> = targets
        .into_iter()
        .filter(|id| !known_ids.contains(*id))
        .cloned()
        .collect();
    dangling.sort();
    dangling.dedup();
    dangling
}

/// The key fields of an already-imported item, kept in the idempotency index so
/// re-imports can detect field-level conflicts (DESIGN §11.3) without rescanning
/// the store. Carries the existing `CloveId` plus the comparable fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingItem {
    /// The clove id of the existing item (the idempotency target).
    pub id: CloveId,
    /// The existing item's status.
    pub status: ItemStatus,
    /// The existing item's priority.
    pub priority: Priority,
    /// The existing item's title.
    pub title: String,
}

impl From<CloveId> for ExistingItem {
    /// Build an [`ExistingItem`] carrying only its id, with placeholder key
    /// fields. Convenient for tests/contexts that index purely by id and never
    /// exercise conflict comparison.
    fn from(id: CloveId) -> Self {
        ExistingItem {
            id,
            status: ItemStatus::Open,
            priority: Priority::DEFAULT,
            title: String::new(),
        }
    }
}

impl ExistingItem {
    /// Compare this existing item against the incoming mapped fields, emitting a
    /// [`ConflictItem`] for each of the DESIGN §11.3 key fields (status,
    /// priority, title) whose value diverges. `source_id` is the incoming id
    /// surfaced in the plan. The write is still skipped — this only reports.
    pub fn conflicts_with(
        &self,
        source_id: &str,
        status: ItemStatus,
        priority: Priority,
        title: &str,
    ) -> Vec<ConflictItem> {
        let mut conflicts = Vec::new();
        if self.status != status {
            conflicts.push(ConflictItem {
                id: source_id.to_owned(),
                field: "status".to_owned(),
                existing: self.status.as_str().to_owned(),
                incoming: status.as_str().to_owned(),
            });
        }
        if self.priority != priority {
            conflicts.push(ConflictItem {
                id: source_id.to_owned(),
                field: "priority".to_owned(),
                existing: self.priority.get().to_string(),
                incoming: priority.get().to_string(),
            });
        }
        if self.title != title {
            conflicts.push(ConflictItem {
                id: source_id.to_owned(),
                field: "title".to_owned(),
                existing: self.title.clone(),
                incoming: title.to_owned(),
            });
        }
        conflicts
    }
}

/// Scan the store and build the `external_ref → ExistingItem` index used by every
/// importer for idempotency: an incoming item whose `external_ref` is already a
/// key here is skipped on re-import (DESIGN §11.3, "skip items where
/// `external_ref` matches an existing item"). The stored [`ExistingItem`] also
/// carries the existing item's key fields so an importer can report field-level
/// `conflicts` when a re-import's mapped values diverge.
///
/// Items without an `external_ref` contribute no entry. On a duplicate
/// `external_ref` (should not happen in a healthy store) the first one scanned
/// wins; per-file parse failures are ignored here (they surface through the
/// normal `doctor`/scan paths).
pub fn build_external_ref_index(
    store: &ItemStore,
) -> Result<HashMap<String, ExistingItem>, ImportError> {
    let (frontmatters, _scan_errors) = store.scan_frontmatter()?;
    let mut index = HashMap::new();
    for fm in frontmatters {
        if let Some(external_ref) = fm.external_ref {
            index.entry(external_ref).or_insert(ExistingItem {
                id: fm.id,
                status: fm.status,
                priority: fm.priority,
                title: fm.title,
            });
        }
    }
    Ok(index)
}

/// Scan the store for the set of all existing clove ids, used by importers to
/// flag dangling dependency targets (M4): a dep/parent/relates id present in
/// neither this set nor the current import batch is reported as dangling.
/// Per-file parse failures are ignored here (they surface through the normal
/// scan/doctor paths).
pub fn build_store_id_set(store: &ItemStore) -> Result<HashSet<CloveId>, ImportError> {
    let (frontmatters, _scan_errors) = store.scan_frontmatter()?;
    Ok(frontmatters.into_iter().map(|fm| fm.id).collect())
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

    // M4: an over-cap dep array is truncated to MAX_DEP_ARRAY_LEN and warned,
    // never written at full (corrupt) length.
    #[test]
    fn cap_dep_array_truncates_and_warns() {
        let ids: Vec<CloveId> = (0..MAX_DEP_ARRAY_LEN + 5)
            .map(|_| CloveId::new("proj-AAAA1111").unwrap())
            .collect();
        let mut warnings = Vec::new();
        let capped = cap_dep_array(ids, "deps", "src-1", &mut warnings);
        assert_eq!(capped.len(), MAX_DEP_ARRAY_LEN);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("deps") && warnings[0].contains("truncated"));

        // Under the cap: returned unchanged, no warning.
        let mut warnings = Vec::new();
        let small = cap_dep_array(
            vec![CloveId::new("proj-AAAA1111").unwrap()],
            "relates",
            "src-2",
            &mut warnings,
        );
        assert_eq!(small.len(), 1);
        assert!(warnings.is_empty());
    }

    // M4: a target id absent from the known-id set is reported as dangling.
    #[test]
    fn dangling_targets_flags_absent_ids() {
        let present = CloveId::new("proj-AAAA1111").unwrap();
        let absent = CloveId::new("proj-ZZZZ9999").unwrap();
        let known: HashSet<CloveId> = [present.clone()].into_iter().collect();
        let targets = [present.clone(), absent.clone()];
        let dangling = dangling_targets(&known, targets.iter());
        assert_eq!(dangling, vec![absent]);
        // Everything present → no dangling.
        assert!(dangling_targets(&known, [&present]).is_empty());
    }
}
