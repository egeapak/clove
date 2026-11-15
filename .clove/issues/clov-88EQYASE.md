---
schema: 1
id: clov-88EQYASE
title: clove ready loses the dangling-deps warning on the index tier and counts closed items on the file tier
status: closed
type: bug
priority: 2
created: 2026-11-14T19:10:27.742Z
updated: 2026-11-14T19:13:56.844Z
closed: 2026-11-14T19:13:56.844Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/ops.rs ready_rows_with_graph builds the warning 'N item(s) excluded with dangling deps: ...' on the file tier only. crates/clove-engine/src/lib.rs lean_via_index returns no warnings, so 'clove ready -f json' has _meta.warnings [] with an index and the warning with --no-index. The file tier also lists closed items (never ready candidates) and ignores the filters of the query.
Expected: the warning lists the open or in_progress items that match the filters and have a missing hard dependency, in id order, and is identical on the file and index tiers.
Done when: tier_parity compares ready warnings between the index and file tiers, with a closed item that has a dangling dep in the fixture. The daemon tier (crates/cloved/src/ipc.rs) still returns no warnings; clove_index::Index::ready_warnings is available for it.