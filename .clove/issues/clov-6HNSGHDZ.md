---
schema: 1
id: clov-6HNSGHDZ
title: clove-sync-github --help opens with an internal doc comment
status: closed
type: bug
priority: 2
created: 2026-11-15T07:01:15.820Z
updated: 2026-11-15T07:03:58.074Z
closed: 2026-11-15T07:03:58.074Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-sync-github/src/main.rs: the Cli struct has no about, so clap uses the struct doc comment (The forwarded-tail args after the cargo-style ... leading echo is stripped) as the help summary of clove sync github owner/repo --help.
Done when: the help opens with a user-facing summary; the_sync_flag_lists_name_every_plugin_flag asserts it.