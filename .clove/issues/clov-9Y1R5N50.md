---
schema: 1
id: clov-9Y1R5N50
title: A leftover item temp file is not git-ignored
status: closed
type: bug
priority: 2
created: 2026-11-14T19:10:20.018Z
updated: 2026-11-14T19:10:20.078Z
closed: 2026-11-14T19:10:20.078Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/write.rs atomic_write creates .clove/issues/.tmp<random> and renames it onto the item file. A process killed between create and rename leaves the temp file. The scan skips it, but crates/clove-core/src/config.rs GITIGNORE_ENTRIES has no entry for it, so git add .clove commits it.
Expected: .clove/.gitignore ignores issues/.tmp*; clove init writes the entry, clove doctor checks it, and an existing clone gets it on its next write.
Done when: store::tests::write_lock_git_ignores_itself and the_item_temp_file_matches_its_gitignore_pattern pass.