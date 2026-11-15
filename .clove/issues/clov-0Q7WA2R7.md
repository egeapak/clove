---
schema: 1
id: clov-0Q7WA2R7
title: 'sync github: syncing a store against a second repository cross-wires gh-N links'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:12.916Z
updated: 2026-11-14T19:26:21.668Z
closed: 2026-11-14T19:26:21.668Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
external_ref gh-N does not name a repository (crates/clove-import/src/github.rs external_ref_for). With items linked gh-1..gh-N from owner/repo, clove sync github other/repo loads an empty state file, so decide_linked (sync.rs) treats each gh-N that exists in other/repo as a first-sync conflict and the policy overwrites the local item or the unrelated issue.
Expected: the sync refuses to run when local gh-* items exist and the state file of this repository records none of them, unless --adopt-links is passed.
Done when: crates/clove/tests/sync_github.rs syncing_linked_items_against_another_repository_is_refused passes.