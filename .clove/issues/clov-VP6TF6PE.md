---
schema: 1
id: clov-VP6TF6PE
title: mcp.rs index-tier test skips its row-shape assertion on an empty page
status: closed
type: chore
priority: 2
created: 2026-11-14T19:04:18.684Z
updated: 2026-11-14T19:29:50.290Z
closed: 2026-11-14T19:29:50.290Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/tests/mcp.rs read_tools_use_the_index_tier_and_report_it checks the full row shape inside 'if let Some(row) = page["items"].get(0)', so an empty page or a renamed key skips the check.
Expected: the test asserts that the row exists.
Done when: the check indexes items[0] and asserts it is an object.