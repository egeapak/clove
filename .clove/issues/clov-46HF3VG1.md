---
schema: 1
id: clov-46HF3VG1
title: sync github misses a local edit made in the same second as a pull
status: closed
type: bug
priority: 2
created: 2026-10-06T18:09:43.000Z
updated: 2026-11-03T17:38:52.958Z
closed: 2026-10-17T23:01:14.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-68
---
Change detection is `local_changed = item.updated > state.local_updated` (crates/clove-import/src/sync.rs ~L971) with second-precision timestamps. A pull writes the item (updated = now, truncated) and records local_updated = that second; a local edit in the same wall-clock second leaves updated equal, so the edit is never pushed — every later sync reports it 'in sync' while GitHub and clove disagree (silent divergence) until some later local edit.

Repro (v0.1.1 release binary, dogfood 2026-10-06): reopen gh-67 on GitHub → `clove sync github` (pulls the reopen, local updated=18:08:37) → `clove close` within the same second → sync: '27 in sync', GitHub #67 stays OPEN, local closed.

Fix idea: record a local content hash in SyncEntry (like gh_content_hash) and treat local as changed when updated > recorded OR hash differs.