---
schema: 1
id: clov-Y43952YM
title: A failed index refresh or index query fails the read instead of falling back to the files
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:58.306Z
updated: 2026-11-14T19:10:20.316Z
closed: 2026-11-14T19:10:20.316Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-engine/src/lib.rs lean_via_index propagates errors from check_staleness, apply_staleness and query_filtered with '?' (index_error). One item file that vanishes between the read and the stat in apply_staleness, or a SQLITE_BUSY from a concurrent writer, makes 'clove ls' exit with an I/O error although the file tier can answer. Only a failed open and a too-complex query fall back today.
Expected: any index error falls through to the file tier, and the answer carries a warning that names the error.
Done when: an engine test breaks the index query (a renamed column) and list returns Source::Files with the file rows and one warning.