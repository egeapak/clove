---
schema: 1
id: clov-CJEXGHV4
title: 'sync github: an issue created by a push whose response was lost is pushed and pulled again as duplicates'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.530Z
updated: 2026-11-14T19:26:21.708Z
closed: 2026-11-14T19:26:21.708Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync.rs plan_sync: when an issue create lands on GitHub but the local external_ref link is not written (the response is lost, or the run stops between the create and the link), the next sync sees an unlinked local item and an unmatched issue. It creates a second issue for the item and a second local item for the first issue. The first issue carries the item's id in clove-meta.
Expected: an unlinked local item is linked to the unmatched issue whose clove-meta id is the item's id and whose content is equal; no second issue or item.
Done when: crates/clove/tests/sync_github.rs::a_lost_create_response_is_linked_on_the_next_sync_not_duplicated passes.