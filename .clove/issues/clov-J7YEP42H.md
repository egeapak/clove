---
schema: 1
id: clov-J7YEP42H
title: Human-format list commands truncate at the window with no notice
status: closed
type: bug
priority: 2
created: 2026-11-14T19:05:05.018Z
updated: 2026-11-14T19:44:49.866Z
closed: 2026-11-14T19:44:49.866Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/listing.rs emit (Human arm) prints only the rows; total and the window are unused. With 130 open items, 'clove ls', 'clove ready' and 'clove search x' print exactly 100 lines (the default limit) and nothing says the list is incomplete. cmd/comments.rs already prints 'showing N of M'.
Expected: when the page ends before the total, human format prints a 'showing N of M items' note on stderr (stdout stays one row per line).
Done when: crates/clove/tests/cli_regressions.rs human_list_reports_a_truncated_page passes.