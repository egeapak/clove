---
schema: 1
id: clov-X18KXN6W
title: clove export leaves out unparseable item files without a warning
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:06:37.406Z
updated: 2026-11-14T19:06:37.482Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/export.rs discards the scan errors ('let (items, _errors) = ctx.store.scan()?'). With one item file that has an invalid frontmatter value, 'clove export jsonl --out backup.jsonl' prints 'wrote N-1 items', exits 0, and names nothing; a restore from that file loses the item.
Expected: each item file that is not exported is named in a warning on stderr and, for 'export json', in _meta.warnings.
Done when: crates/clove/tests/cli_regressions.rs export_names_item_files_it_could_not_export passes.