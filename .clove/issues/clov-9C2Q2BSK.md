---
schema: 1
id: clov-9C2Q2BSK
title: 'sync github: pulled clove-meta deps skip the existence and cycle checks, and a dep that fails is recorded as synced'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.586Z
updated: 2026-11-14T19:26:21.744Z
closed: 2026-11-14T19:26:21.744Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync_net.rs: pull_create writes the clove-meta deps of an issue straight into the new item file (create_item checks neither that the target exists nor cycles), so an issue body can plant a dangling dep or a cycle. pull_update ignores a failed dep_add and records the full remote fingerprint, so the dep is never tried again and the next push drops it from the issue.
Expected: pull_create adds deps through Store::dep_add after all items of the run exist; a dep that cannot be added is left out of the recorded fingerprint, reported as a warning, and tried again by the next sync.
Done when: crates/clove/tests/sync_github.rs::a_pulled_dep_on_a_missing_item_is_left_out_and_tried_again passes.