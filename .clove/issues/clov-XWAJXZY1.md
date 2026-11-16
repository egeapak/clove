---
schema: 1
id: clov-XWAJXZY1
title: Sync flag lists in clove sync --help and agent-doc omit --adopt-links
status: closed
type: docs
priority: 2
created: 2026-11-15T07:01:15.483Z
updated: 2026-11-15T07:03:58.070Z
closed: 2026-11-15T07:03:58.070Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cli.rs (the sync after-help Form: line and the SyncArgs.rest doc) and crates/clove/src/cmd/agent_doc.rs (the clove sync github line) list the clove-sync-github flags by hand and lack --adopt-links.
Done when: both lists name every flag of the plugin's Cli, and crates/clove/tests/sync_github.rs::the_sync_flag_lists_name_every_plugin_flag compares them with the plugin's own --help.