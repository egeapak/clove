---
schema: 1
id: clov-GR3NFPHY
title: No test covers a daemon sync whose index apply fails
status: closed
type: chore
priority: 2
created: 2026-11-15T07:08:47.516Z
updated: 2026-11-15T07:08:47.619Z
closed: 2026-11-15T07:08:47.619Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/fresh.rs Freshness::sync puts the reported ids back when apply_staleness fails, so the watcher's retry applies them. No test makes the apply fail, so dropping the put-back would pass every test, and an in-place save (which does not move the directory mtime) would then never be indexed.
Done when: a fresh.rs unit test makes the apply fail, asserts Sync::Failed with nothing applied, and asserts that the next sync with no new report applies the change.