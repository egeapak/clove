//! Pure, process-free three-way merge logic for clove item frontmatter
//! (DESIGN.md §9.2).
//!
//! This module owns the *semantics* of the `clove merge-driver` command without
//! touching the filesystem, spawning `git`, or knowing about conflict-marker
//! formatting. The CLI command (`crates/clove/src/cmd/merge_driver.rs`) parses
//! the three file versions into [`ItemFrontmatter`] + body, calls
//! [`merge_frontmatter`] here, delegates the body to `git merge-file`, and
//! renders the result (clean → write canonical frontmatter; conflict → embed
//! git-style markers). Keeping the merge math here means it is unit- and
//! property-testable in-process.
//!
//! ## Algorithm
//!
//! - **Scalars** ([`merge_scalar`]): if `ours == theirs` take it; if one side
//!   equals the base take the *other* side (that side is the only one that
//!   changed); if both changed to different values → [`ScalarConflict`].
//! - **Sets** ([`merge_set`]): three-way set merge
//!   `union(ours, theirs) \ (base \ ours \ theirs)`, sorted + de-duped. A
//!   *remove/add* conflict on the same element (in base, removed by one side,
//!   kept or re-added by the other) is flagged on that field.

use std::collections::BTreeSet;

use clove_types::{ItemFrontmatter, ItemStatus};

/// A scalar three-way conflict: both sides diverged from the base to different
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarConflict<T> {
    /// The base (ancestor) value, if any.
    pub base: Option<T>,
    /// Our value.
    pub ours: T,
    /// Their value.
    pub theirs: T,
}

/// Three-way merge of a scalar field.
///
/// `base` is the ancestor value (`None` for an add/add merge with no common
/// ancestor). Returns the resolved value, or a [`ScalarConflict`] when both
/// sides changed the value to two different things.
pub fn merge_scalar<T: Eq + Clone>(
    base: Option<&T>,
    ours: &T,
    theirs: &T,
) -> Result<T, ScalarConflict<T>> {
    // Same value on both sides → trivially resolved (the same-value rule).
    if ours == theirs {
        return Ok(ours.clone());
    }
    match base {
        Some(base) => {
            if ours == base {
                // Only theirs changed.
                Ok(theirs.clone())
            } else if theirs == base {
                // Only ours changed.
                Ok(ours.clone())
            } else {
                // Both changed, differently → conflict.
                Err(ScalarConflict {
                    base: Some(base.clone()),
                    ours: ours.clone(),
                    theirs: theirs.clone(),
                })
            }
        }
        // Add/add with no base and differing values → conflict.
        None => Err(ScalarConflict {
            base: None,
            ours: ours.clone(),
            theirs: theirs.clone(),
        }),
    }
}

/// The result of a three-way set merge of a list field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetMergeResult<X> {
    /// Clean merge: the sorted, de-duped union result.
    Resolved(Vec<X>),
    /// Remove/add conflict on one or more elements.
    Conflict(SetConflict<X>),
}

/// A set-field conflict: ours and theirs after the three-way merge, plus the
/// specific elements one side removed while the other kept/added them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetConflict<X> {
    /// The base set (sorted, de-duped).
    pub base: Vec<X>,
    /// What our side wants the set to be (sorted, de-duped).
    pub ours: Vec<X>,
    /// What their side wants the set to be (sorted, de-duped).
    pub theirs: Vec<X>,
    /// Elements that are the locus of the remove/add conflict (sorted).
    pub conflicting: Vec<X>,
}

/// Three-way set merge of a list field.
///
/// Computes `union(ours, theirs) \ (base \ ours \ theirs)`, then sorts and
/// de-dupes. Detects a remove/add conflict: an element that was present in the
/// base, removed by one side, but kept or re-added by the other. Such an element
/// is ambiguous (delete vs. keep) so the field is flagged as a conflict.
pub fn merge_set<X: Ord + Clone>(base: &[X], ours: &[X], theirs: &[X]) -> SetMergeResult<X> {
    let base_set: BTreeSet<X> = base.iter().cloned().collect();
    let ours_set: BTreeSet<X> = ours.iter().cloned().collect();
    let theirs_set: BTreeSet<X> = theirs.iter().cloned().collect();

    // Remove/add conflict: element in base, removed by exactly one side and
    // retained by the other. (If both sides removed it, it is cleanly gone; if
    // neither removed it, it survives — neither is a conflict.)
    let mut conflicting: Vec<X> = base_set
        .iter()
        .filter(|x| {
            let in_ours = ours_set.contains(*x);
            let in_theirs = theirs_set.contains(*x);
            in_ours != in_theirs
        })
        .cloned()
        .collect();

    if !conflicting.is_empty() {
        conflicting.sort();
        return SetMergeResult::Conflict(SetConflict {
            base: sorted_unique(base),
            ours: sorted_unique(ours),
            theirs: sorted_unique(theirs),
            conflicting,
        });
    }

    // With no remove/add conflict, the resolved set is exactly
    // union(ours, theirs): an element removed by both sides is absent from both
    // sets and therefore absent from the union — no explicit subtraction needed.
    let merged: BTreeSet<X> = ours_set.union(&theirs_set).cloned().collect();
    SetMergeResult::Resolved(merged.into_iter().collect())
}

fn sorted_unique<X: Ord + Clone>(items: &[X]) -> Vec<X> {
    let set: BTreeSet<X> = items.iter().cloned().collect();
    set.into_iter().collect()
}

/// A per-field conflict surfaced by [`merge_frontmatter`]. `ours`/`theirs` are
/// the rendered scalar values (or comma-joined list values) for embedding in
/// conflict markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldConflict {
    /// The frontmatter field name (e.g. `status`, `deps`).
    pub field: String,
    /// Our side rendered for display.
    pub ours: String,
    /// Their side rendered for display.
    pub theirs: String,
}

/// The outcome of merging two frontmatters against a common base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Fully clean merge: the merged frontmatter.
    Clean(Box<ItemFrontmatter>),
    /// One or more field conflicts. `merged` is a best-effort frontmatter (clean
    /// fields resolved, conflicting fields left at our value) so the written
    /// file still round-trips for a human; `conflicts` lists every field that
    /// could not be auto-resolved.
    Conflict {
        /// Best-effort merged frontmatter (conflicting fields hold our value).
        merged: Box<ItemFrontmatter>,
        /// The fields that conflicted.
        conflicts: Vec<FieldConflict>,
    },
}

impl MergeOutcome {
    /// Whether the merge was fully clean.
    pub fn is_clean(&self) -> bool {
        matches!(self, MergeOutcome::Clean(_))
    }
}

/// Merge `ours` and `theirs` against the common `base` (`None` for add/add).
///
/// Scalars are merged per [`merge_scalar`]; the status discriminant and its
/// `closed` timestamp are merged coherently as a pair. List fields (labels,
/// deps, relates, duplicates, supersedes) are set-merged per [`merge_set`]. On a
/// clean merge the `updated` timestamp is the newer (max) of the two sides.
pub fn merge_frontmatter(
    base: Option<&ItemFrontmatter>,
    ours: &ItemFrontmatter,
    theirs: &ItemFrontmatter,
) -> MergeOutcome {
    let mut conflicts: Vec<FieldConflict> = Vec::new();

    // Start from our side as the canonical skeleton (preserves id, etc.).
    let mut merged = ours.clone();

    // --- Scalars ---

    // schema: numeric; merge but normally identical.
    merge_scalar_field(
        base.map(|b| &b.schema),
        &ours.schema,
        &theirs.schema,
        "schema",
        |v| v.to_string(),
        &mut merged.schema,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.title),
        &ours.title,
        &theirs.title,
        "title",
        |v| v.clone(),
        &mut merged.title,
        &mut conflicts,
    );

    // status + closed are merged coherently as a pair.
    merge_status_pair(base, ours, theirs, &mut merged, &mut conflicts);

    merge_scalar_field(
        base.map(|b| &b.item_type),
        &ours.item_type,
        &theirs.item_type,
        "type",
        |v| v.as_str().to_owned(),
        &mut merged.item_type,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.priority),
        &ours.priority,
        &theirs.priority,
        "priority",
        |v| v.get().to_string(),
        &mut merged.priority,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.assignee),
        &ours.assignee,
        &theirs.assignee,
        "assignee",
        render_opt,
        &mut merged.assignee,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.parent),
        &ours.parent,
        &theirs.parent,
        "parent",
        |v| {
            v.as_ref()
                .map_or_else(|| "null".to_owned(), |id| id.to_string())
        },
        &mut merged.parent,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.created),
        &ours.created,
        &theirs.created,
        "created",
        |v| v.to_rfc3339(),
        &mut merged.created,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.source_system),
        &ours.source_system,
        &theirs.source_system,
        "source_system",
        render_opt,
        &mut merged.source_system,
        &mut conflicts,
    );

    merge_scalar_field(
        base.map(|b| &b.external_ref),
        &ours.external_ref,
        &theirs.external_ref,
        "external_ref",
        render_opt,
        &mut merged.external_ref,
        &mut conflicts,
    );

    // updated: on a clean merge take the newer (max) timestamp.
    merged.updated = ours.updated.max(theirs.updated);

    // --- List (set) fields ---
    merge_set_field(
        base.map(|b| b.labels.as_slice()),
        &ours.labels,
        &theirs.labels,
        "labels",
        |xs| xs.join(", "),
        &mut merged.labels,
        &mut conflicts,
    );
    merge_id_set_field(
        base.map(|b| b.deps.as_slice()),
        &ours.deps,
        &theirs.deps,
        "deps",
        &mut merged.deps,
        &mut conflicts,
    );
    merge_id_set_field(
        base.map(|b| b.relates.as_slice()),
        &ours.relates,
        &theirs.relates,
        "relates",
        &mut merged.relates,
        &mut conflicts,
    );
    merge_id_set_field(
        base.map(|b| b.duplicates.as_slice()),
        &ours.duplicates,
        &theirs.duplicates,
        "duplicates",
        &mut merged.duplicates,
        &mut conflicts,
    );
    merge_id_set_field(
        base.map(|b| b.supersedes.as_slice()),
        &ours.supersedes,
        &theirs.supersedes,
        "supersedes",
        &mut merged.supersedes,
        &mut conflicts,
    );

    if conflicts.is_empty() {
        MergeOutcome::Clean(Box::new(merged))
    } else {
        MergeOutcome::Conflict {
            merged: Box::new(merged),
            conflicts,
        }
    }
}

/// Render an `Option<String>` for conflict display.
fn render_opt(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "null".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn merge_scalar_field<T: Eq + Clone>(
    base: Option<&T>,
    ours: &T,
    theirs: &T,
    field: &str,
    render: impl Fn(&T) -> String,
    out: &mut T,
    conflicts: &mut Vec<FieldConflict>,
) {
    match merge_scalar(base, ours, theirs) {
        Ok(value) => *out = value,
        Err(c) => {
            // Leave `out` at our value (best effort); flag the conflict.
            *out = ours.clone();
            conflicts.push(FieldConflict {
                field: field.to_owned(),
                ours: render(&c.ours),
                theirs: render(&c.theirs),
            });
        }
    }
}

/// Merge `status` together with its `closed` timestamp so the pair stays
/// coherent (a `closed` status carries its timestamp; reopening drops it).
fn merge_status_pair(
    base: Option<&ItemFrontmatter>,
    ours: &ItemFrontmatter,
    theirs: &ItemFrontmatter,
    merged: &mut ItemFrontmatter,
    conflicts: &mut Vec<FieldConflict>,
) {
    let base_status = base.map(|b| b.status);
    match merge_scalar(base_status.as_ref(), &ours.status, &theirs.status) {
        Ok(status) => {
            merged.status = status;
            // Pick the closed timestamp coherently with the resolved status.
            merged.closed = if status == ItemStatus::Closed {
                resolve_closed(base, ours, theirs)
            } else {
                None
            };
        }
        Err(c) => {
            merged.status = ours.status;
            merged.closed = ours.closed;
            conflicts.push(FieldConflict {
                field: "status".to_owned(),
                ours: render_status(c.ours, ours.closed),
                theirs: render_status(c.theirs, theirs.closed),
            });
        }
    }
}

/// Choose the `closed` timestamp for a resolved `closed` status: prefer whichever
/// side actually carries one, taking the later if both do; fall back to base.
fn resolve_closed(
    base: Option<&ItemFrontmatter>,
    ours: &ItemFrontmatter,
    theirs: &ItemFrontmatter,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match (ours.closed, theirs.closed) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => base.and_then(|b| b.closed),
    }
}

fn render_status(status: ItemStatus, closed: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match closed {
        Some(ts) => format!("{} ({})", status.as_str(), ts.to_rfc3339()),
        None => status.as_str().to_owned(),
    }
}

fn merge_set_field<X: Ord + Clone + ToString>(
    base: Option<&[X]>,
    ours: &[X],
    theirs: &[X],
    field: &str,
    render: impl Fn(&[String]) -> String,
    out: &mut Vec<X>,
    conflicts: &mut Vec<FieldConflict>,
) {
    let base = base.unwrap_or(&[]);
    match merge_set(base, ours, theirs) {
        SetMergeResult::Resolved(values) => *out = values,
        SetMergeResult::Conflict(c) => {
            // Leave `out` at our value (best effort); flag the conflict.
            *out = ours.to_vec();
            let render_vec =
                |xs: &[X]| render(&xs.iter().map(ToString::to_string).collect::<Vec<_>>());
            conflicts.push(FieldConflict {
                field: field.to_owned(),
                ours: render_vec(&c.ours),
                theirs: render_vec(&c.theirs),
            });
        }
    }
}

fn merge_id_set_field<X: Ord + Clone + ToString>(
    base: Option<&[X]>,
    ours: &[X],
    theirs: &[X],
    field: &str,
    out: &mut Vec<X>,
    conflicts: &mut Vec<FieldConflict>,
) {
    merge_set_field(
        base,
        ours,
        theirs,
        field,
        |xs| format!("[{}]", xs.join(", ")),
        out,
        conflicts,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_types::CloveId;

    fn id(s: &str) -> CloveId {
        CloveId::new(s).unwrap()
    }

    #[test]
    fn scalar_same_value_resolves() {
        assert_eq!(merge_scalar(Some(&1), &2, &2), Ok(2));
    }

    #[test]
    fn scalar_one_side_changed_takes_changed() {
        // ours == base, theirs changed → take theirs.
        assert_eq!(merge_scalar(Some(&1), &1, &3), Ok(3));
        // theirs == base, ours changed → take ours.
        assert_eq!(merge_scalar(Some(&1), &5, &1), Ok(5));
    }

    #[test]
    fn scalar_both_diverge_conflicts() {
        assert_eq!(
            merge_scalar(Some(&1), &2, &3),
            Err(ScalarConflict {
                base: Some(1),
                ours: 2,
                theirs: 3
            })
        );
    }

    #[test]
    fn scalar_addadd_same_resolves_diff_conflicts() {
        assert_eq!(merge_scalar(None, &2, &2), Ok(2));
        assert!(merge_scalar(None, &2, &3).is_err());
    }

    #[test]
    fn set_union_merge() {
        let r = merge_set(
            &[id("proj-OLD00000")],
            &[id("proj-OLD00000"), id("proj-AAAAAAAA")],
            &[id("proj-OLD00000"), id("proj-BBBBBBBB")],
        );
        assert_eq!(
            r,
            SetMergeResult::Resolved(vec![
                id("proj-AAAAAAAA"),
                id("proj-BBBBBBBB"),
                id("proj-OLD00000"),
            ])
        );
    }

    #[test]
    fn set_remove_by_both_drops() {
        let r = merge_set(&[id("proj-OLD00000")], &[], &[]);
        assert_eq!(r, SetMergeResult::Resolved(vec![]));
    }

    #[test]
    fn set_remove_add_conflict() {
        // base has OLD; ours removes it, theirs keeps it (and adds NEW).
        let r = merge_set(
            &[id("proj-OLD00000")],
            &[],
            &[id("proj-OLD00000"), id("proj-NEW00000")],
        );
        match r {
            SetMergeResult::Conflict(c) => {
                assert_eq!(c.conflicting, vec![id("proj-OLD00000")]);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn set_result_is_sorted_and_deduped() {
        let r = merge_set::<i32>(&[], &[3, 1, 2, 1], &[2, 5, 5]);
        assert_eq!(r, SetMergeResult::Resolved(vec![1, 2, 3, 5]));
    }
}
