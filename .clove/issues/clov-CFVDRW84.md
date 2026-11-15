---
schema: 1
id: clov-CFVDRW84
title: clove-cli test suites can attach to the developer's own hub
status: closed
type: bug
priority: 2
created: 2026-11-14T19:12:33.203Z
updated: 2026-11-14T19:19:25.183Z
closed: 2026-11-14T19:19:25.183Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Nineteen suites under crates/clove/tests spawn `clove` without CLOVE_RUNTIME_DIR (broken_pipe, cli_foundation, cli_more, concurrent_writes, export, github_fallback, golden_cli, import_beads, import_json, import_tk, m2_scaffolding, merge_driver, multiplexer_plugin, plugin_dispatch, plugin_install, plugin_registry, schema_validation, stats, sync_github). DaemonClient::probe (crates/clove-ipc/src/client.rs) then looks in the user's real runtime directory, attaches to a hub found there, and can run cleanup_stale_hub on it. Several of them also inherit the user's clove home.
Done when: one helper in crates/clove/tests/support gives every suite a private CLOVE_RUNTIME_DIR and CLOVE_HOME, every suite uses it, and `cargo nextest run -p clove-cli --all-features` passes.