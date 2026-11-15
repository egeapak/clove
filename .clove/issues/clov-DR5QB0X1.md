---
schema: 1
id: clov-DR5QB0X1
title: An index more than 20 changes behind is never refreshed and reads fall back to files with no warning
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:58.258Z
updated: 2026-11-14T19:10:20.303Z
closed: 2026-11-14T19:10:20.303Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-engine/src/lib.rs lean_via_index returns None when the staleness report has more than STALE_REFRESH_LIMIT (20) changes and applies nothing. After 'clove reindex' and 25 'clove new' calls (or a git pull that brings more than 20 items), every later 'clove ls -f json' reports _meta.source files with empty _meta.warnings until a manual 'clove reindex', and each read pays the per-file staleness pass plus a whole-store scan. docs/DESIGN.md §6.4 promises a warning; the agent-doc text says the index is refreshed transparently.
Expected: with index.auto_refresh on, a read brings the index up to date whatever the change count (incremental apply, or a rebuild when most of the store changed) and answers from it; if that fails, the read answers from the files and says so in warnings.
Done when: an engine test adds more than 20 items behind a built index and the next list reports Source::Index with the new total.