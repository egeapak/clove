---
schema: 1
id: clov-FFXX4SET
title: Item files are written with mode 0600 and lose their existing mode on update
status: closed
type: bug
priority: 2
created: 2026-11-14T19:09:31.028Z
updated: 2026-11-14T19:09:31.176Z
closed: 2026-11-14T19:09:31.176Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/write.rs atomic_write persists a tempfile::NamedTempFile, which is created 0600, and sets no permissions. A new item file is 0600, and an update of a 0644 file makes it 0600, so another uid or group that could read the store cannot read the item. fs_safe::write_atomic keeps the mode and gives a new file 0644.
Expected: a new item file is 0644 and a rewrite keeps the mode of the file it replaces.
Done when: write::tests::item_files_keep_an_ordinary_mode passes.