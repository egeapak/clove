//! T-M05 property tests for the pure set-merge logic (`merge_set`).
//!
//! For random `(base, ours, theirs)` over a small alphabet, the three-way set
//! merge must, in the non-conflict case:
//!   * equal the mathematical three-way union
//!     `union(ours, theirs) \ (base \ ours \ theirs)`,
//!   * be sorted and de-duped, and
//!   * be commutative in `(ours, theirs)`.

use std::collections::BTreeSet;

use clove_import::merge::{merge_set, SetMergeResult};
use proptest::prelude::*;

/// A small alphabet keeps overlaps (and conflicts) frequent, exercising both
/// branches of the merge.
fn small_set() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..6, 0..6)
}

/// The mathematical reference: `union(ours, theirs) \ (base \ ours \ theirs)`.
fn reference(base: &[u8], ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    let b: BTreeSet<u8> = base.iter().copied().collect();
    let o: BTreeSet<u8> = ours.iter().copied().collect();
    let t: BTreeSet<u8> = theirs.iter().copied().collect();
    let mut result: BTreeSet<u8> = o.union(&t).copied().collect();
    // Remove base elements absent from both sides (removed by both).
    for x in &b {
        if !o.contains(x) && !t.contains(x) {
            result.remove(x);
        }
    }
    result.into_iter().collect()
}

fn is_sorted_deduped(v: &[u8]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}

proptest! {
    #[test]
    fn clean_merge_equals_reference_and_is_sorted(
        base in small_set(),
        ours in small_set(),
        theirs in small_set(),
    ) {
        match merge_set(&base, &ours, &theirs) {
            SetMergeResult::Resolved(merged) => {
                prop_assert!(is_sorted_deduped(&merged), "not sorted/deduped: {merged:?}");
                prop_assert_eq!(merged, reference(&base, &ours, &theirs));
            }
            // A conflict (remove/add on the same element) is acceptable; the
            // clean-merge invariants only apply to the resolved case.
            SetMergeResult::Conflict(_) => {}
        }
    }

    #[test]
    fn merge_is_commutative_in_ours_theirs(
        base in small_set(),
        ours in small_set(),
        theirs in small_set(),
    ) {
        let ab = merge_set(&base, &ours, &theirs);
        let ba = merge_set(&base, &theirs, &ours);
        match (ab, ba) {
            (SetMergeResult::Resolved(x), SetMergeResult::Resolved(y)) => {
                prop_assert_eq!(x, y, "set merge must be commutative on clean merges");
            }
            // Conflict detection is symmetric too: if one order conflicts, so
            // must the other.
            (SetMergeResult::Conflict(_), SetMergeResult::Conflict(_)) => {}
            (a, b) => prop_assert!(false, "asymmetric outcome: {a:?} vs {b:?}"),
        }
    }
}
