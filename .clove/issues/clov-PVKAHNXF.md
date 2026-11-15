---
schema: 1
id: clov-PVKAHNXF
title: Unlimited web list and board reads hydrate every row from a tier
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.560Z
updated: 2026-11-14T19:30:06.498Z
closed: 2026-11-14T19:30:06.498Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
GET /api/v1/items with no limit (the web default, WEB_LIMIT = 0) and GET /api/v1/board ask clove-engine for Projection::Full over the whole store. An index or daemon answer then re-reads every item file, and crates/clove-web/src/read.rs values_of re-reads each dependency file per row (graph_terms_detailed, missing_refs_on_disk). One file scan answers the same request with one read per file and hands back the graph.
Expected: an unwindowed list or board request is answered by the file tier (Projection::Files); a windowed request keeps the tiers.
Done when: crates/clove-web/tests/api.rs::unlimited_reads_use_one_file_scan passes.