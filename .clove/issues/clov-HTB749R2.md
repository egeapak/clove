---
schema: 1
id: clov-HTB749R2
title: cloved keeps its own copy of STALE_REFRESH_LIMIT
status: closed
type: chore
priority: 2
created: 2026-11-15T06:56:10.215Z
updated: 2026-11-15T06:56:19.172Z
closed: 2026-11-15T06:56:19.172Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/ipc.rs defines STALE_REFRESH_LIMIT = 20 next to the same constant in crates/clove-engine/src/lib.rs. If one changes, the daemon and the engine disagree on how many out-of-date items one read applies.
Expected: one definition in clove-index (both crates depend on it), re-exported by clove-engine and used by cloved.
Done when: grep finds one 'const STALE_REFRESH_LIMIT' in crates/, and cargo check -p cloved -p clove-engine --all-features passes.