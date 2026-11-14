---
schema: 1
id: clov-NEP7MV3B
title: 'merge-driver: a dep removed on one branch conflicts when the other branch kept it unchanged'
status: closed
type: feature
priority: 2
created: 2026-11-14T19:15:06.139Z
updated: 2026-11-14T19:26:21.780Z
closed: 2026-11-14T19:26:21.780Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/merge.rs merge_set flags a conflict for every element that is in the base and in exactly one side. A branch that only removes a dep (or label, relates entry) therefore conflicts with any branch that changed the same file and left that element alone. docs/DESIGN.md §9.2 gives the formula union(ours, theirs) \ (ancestor \ ours \ theirs) and calls 'A removes X, B adds X' the conflict; crates/clove/tests/merge_driver.rs::v_i16_dep_removal_conflict pins a conflict where B keeps X and adds another element.
Open choice: keep the conflict for every one-sided removal, or take a one-sided removal cleanly when the other side's set equals the base (the other branch did not touch the field) and keep the conflict only when both branches changed the set.
Done when: the rule is chosen, DESIGN.md §9.2 states it, and merge_set plus its tests follow it.