---
schema: 1
id: clov-K7DKE9F3
title: 'TUI: an item created with the New form is not selected in the list'
status: open
type: bug
priority: 2
created: 2026-11-18T21:58:06.857Z
updated: 2026-11-18T21:58:06.857Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
In `clove tui`, press n, type a title, Ctrl-S. The status line says 'created <id>' and the item appears in the list, but the selection stays on the item that was selected before. submit_new (crates/clove-tui/src/app/mod.rs ~880) returns only a status string, and the refresh after the write keeps the old selection.

Expected (docs/RELEASE_CHECKLIST.md §6 'New form'): the new item is selected after the save.
Done when: after a create from the form, the selection is the new item once the background rescan lands, with a test in the app tests.