---
schema: 1
id: clov-A8N29MWJ
title: Accept several ids in `clove start`, `close`, and `status`
status: open
type: feature
priority: 3
created: 2026-10-31T06:46:18.399Z
updated: 2026-10-31T06:46:18.399Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
`clove start <id>`, `clove close <id>`, and `clove status <id> <state>` accept one id per call. Agents that track several items at once often call `clove start ID1 ID2 ID3`, get an argument error, and then skip the in_progress transition.
Expected: each of these commands accepts one or more ids, applies the transition to each item, and reports a per-id result (JSON: one entry per id; non-zero exit if any id fails).
Done when: the CLI tests cover several ids, including one id that does not exist.