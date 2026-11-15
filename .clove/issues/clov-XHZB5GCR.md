---
schema: 1
id: clov-XHZB5GCR
title: JSON rows of ls, ready and query have 5 keys from the index and 18 from a file scan
status: closed
type: bug
priority: 2
created: 2026-11-14T19:28:46.938Z
updated: 2026-11-14T19:28:54.843Z
closed: 2026-11-14T19:28:54.843Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/{ls,ready,query}.rs ask the engine for Projection::Lean when no --fields is given. An index or daemon answer then has the keys id, status, type, priority, title, and a file-scan answer (no index, --no-index, stale index) has the whole frontmatter. The same command returns different JSON depending on whether .clove/index.db exists.
Expected: without --fields, JSON and JSONL rows carry the whole frontmatter from every tier (Projection::Full); human format and --fields inside the five lean columns keep the lean row.
Done when: cli_regressions::json_list_rows_have_the_same_keys_on_every_tier passes.