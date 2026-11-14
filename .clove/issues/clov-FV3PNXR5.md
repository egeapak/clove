---
schema: 1
id: clov-FV3PNXR5
title: Delete without force removes a parent that has children and passes on a partial scan
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:45.171Z
updated: 2026-11-14T19:08:45.278Z
closed: 2026-11-14T19:08:45.278Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/store.rs ItemStore::delete(id, force=false) refuses only when another item lists id in deps. Deleting an item that other items name as parent succeeds and leaves a dangling parent on each child. dependents_of also returns an empty list when the scan fails and skips unparseable files, so a dependent in a broken file does not block the delete; dep_add and set_parent refuse in that state.
Expected: without force, delete returns HasDependents for children as well as dependents, and ScanFailed when any item file is unparseable. force=true still deletes.
Done when: core_behavior tests delete_refuses_a_parent_with_children_then_force_succeeds and delete_refuses_while_an_item_file_is_unparseable pass.