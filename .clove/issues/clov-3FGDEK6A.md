---
schema: 1
id: clov-3FGDEK6A
title: 'clove edit: a body changed in $EDITOR keeps the old updated timestamp'
status: closed
type: bug
priority: 2
created: 2026-11-04T09:57:37.064Z
updated: 2026-11-14T19:44:50.058Z
closed: 2026-11-14T19:44:50.058Z
labels: [sync]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-188
---
crates/clove/src/cmd/edit.rs open_in_editor() runs the editor on the item file and only re-parses it. It does not set the frontmatter 'updated' field. So a body edit made in $EDITOR is not newer than the last GitHub sync (clove_import::sync decide_linked compares 'updated' with the recorded local_updated), and 'clove sync github' does not push it.
Expected: when the file content changed in the editor, 'updated' is set to the current time (through the unified write path, so the status/closed invariant is checked too); when nothing changed, the file is left as it was.
Done when: a test in crates/clove/tests runs 'clove edit <id>' with an EDITOR script that changes the body, and asserts that 'updated' moved; and one with an editor that changes nothing, and asserts that the file is byte-identical.