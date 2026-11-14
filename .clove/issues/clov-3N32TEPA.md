---
schema: 1
id: clov-3N32TEPA
title: clove export holds every item body in memory
status: in_progress
type: chore
priority: 2
created: 2026-11-14T19:06:37.505Z
updated: 2026-11-14T19:06:37.586Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/export.rs loads all items with bodies (store.scan), builds one serde_json Value per item, and with --out also serializes the whole export into one buffer before writing. Peak memory is 2x to 4x the store size (192 MB store: 395 MB for jsonl, 771 MB for json --out).
Expected: plan from the frontmatter scan, then read, shape and write one item at a time; --out streams into the temp file.
Done when: export output is byte-identical to the in-memory writer (clove-import export.rs streamed_json_is_byte_identical_to_the_in_memory_writer) and crates/clove/tests/export.rs passes.