---
schema: 1
id: clov-NEV2CAY8
title: clove --clove-dir with a path not named .clove splits the store
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:03:23.180Z
updated: 2026-11-14T19:03:34.006Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/context.rs discover() and cmd/init.rs take --clove-dir PATH as the clove dir and PATH's parent as the root, but ItemStore::new(root) and load_config(root) always use <root>/.clove. With PATH=/x/store, init writes /x/store/config.toml and every later command reads and writes /x/.clove; with an existing /repo/.clove, 'clove --clove-dir /repo/snapshot-clove new t' writes the item into /repo/.clove/issues while the index, daemon key and plugin env point at snapshot-clove.
Expected: an override that is not <parent>/.clove (by name or as a symlink to it) is rejected with a validation error (exit 4) and nothing is written.
Done when: crates/clove/tests/cli_regressions.rs clove_dir_must_be_a_dot_clove_directory and clove_dir_sibling_of_a_real_store_is_refused pass.