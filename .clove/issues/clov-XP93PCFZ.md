---
schema: 1
id: clov-XP93PCFZ
title: Engine hydrates a large Full page from a tier although a file scan is cheaper
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:43.609Z
updated: 2026-11-14T19:08:30.571Z
closed: 2026-11-14T19:08:30.571Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-engine/src/lib.rs finish_lean and blocked read every file of the page back after a daemon or index answer (Projection::Full). For a page that is a large share of the store this costs more than the file tier: 'clove blocked --limit 0' at 80k items takes 1884 ms on the index tier and 937 ms with --no-index; MCP clove_list with limit 0 at 20k items takes 545 ms through a tier and 220 ms as a file scan. For blocked, with_blocked_by also reads each dependency file again per row.
Expected: when the tiered page is large relative to the store, the engine answers from the file tier (Source::Files) and returns the same rows.
Done when: an engine test shows that a Full page above the threshold reports Source::Files with rows equal to the file answer, and a small page still reports Source::Index; crates/clove-engine/tests/tier_parity.rs passes.