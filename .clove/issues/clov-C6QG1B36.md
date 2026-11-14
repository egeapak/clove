---
schema: 1
id: clov-C6QG1B36
title: 'clove edit in an editor bypasses the store: stale index, no envelope, invalid file left in place'
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:17:54.474Z
updated: 2026-11-14T19:17:54.562Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/edit.rs open_in_editor() lets the editor write the item file and then only re-parses it. (1) An editor that writes in place (nano, VS Code) changes neither the directory mtime nor the file count, so the index staleness fast check (clove-index stale.rs) reports clean and 'clove ls --status open' keeps returning an item whose file now says closed. (2) With --format json the command prints nothing on success. (3) If the edited file fails to parse or validate, the error is returned and the invalid file stays in the store, where it then disappears from ls and export. (4) A status typed as closed has no closed timestamp.
Expected: a changed file is saved through ItemStore::update_with (validated, renamed into place, closed kept in step with status), the saved item is printed in every format, and an edit that the store refuses is rolled back with the rejected text kept in a temp file named in the error.
Done when: crates/clove/tests/cli_regressions.rs editor_edit_is_saved_through_the_store and editor_edit_that_does_not_validate_is_rolled_back pass.