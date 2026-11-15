---
schema: 1
id: clov-6DANXK63
title: 'sync github: an item deleted locally is created again by every sync'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.651Z
updated: 2026-11-14T19:26:21.774Z
closed: 2026-11-14T19:26:21.774Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync.rs plan_sync: an issue with no local item is always a pull-create, also when the sync state has an entry for it (it was synced before and the local item was deleted since). The deleted item comes back on every sync.
Expected: an issue with a state entry, no local item and no change on GitHub since the entry is reported under local_missing and not pulled; an issue edited since is pulled again.
Done when: crates/clove/tests/sync_github.rs::a_locally_deleted_item_is_reported_not_pulled_again passes.