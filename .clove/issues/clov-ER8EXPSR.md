---
schema: 1
id: clov-ER8EXPSR
title: clove import --dry-run accepts a file that the real import rejects half-way
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:07:30.582Z
updated: 2026-11-14T19:07:30.646Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/import.rs runs plan_restore for --dry-run, which only checks whether each id exists, and apply_restore (clove-import restore.rs) validates per item with '?'. A jsonl file with a valid item, an item with an empty title, and a valid item: the dry run reports 'would create 3' with exit 0; the real run writes item 1, fails on item 2 with 'invalid title' (exit 4, no id named) and never imports item 3. An id repeated in the file is planned as 'would create 2' and applied as '1 created, 1 skipped'.
Expected: every item that would be written is validated before any write, for both the dry run and the real run; the error names each invalid id and nothing is written. The plan counts a repeated id the way the apply does.
Done when: crates/clove/tests/cli_regressions.rs import_validates_every_item_before_writing_any and clove-import restore.rs plan_counts_an_id_repeated_in_the_file_like_the_apply pass.