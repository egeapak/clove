---
schema: 1
id: clov-SXZE5HKG
title: Help and docs do not say where provider arguments start for import, export and sync
status: closed
type: docs
priority: 2
created: 2026-11-14T19:34:31.194Z
updated: 2026-11-14T19:44:50.390Z
closed: 2026-11-14T19:44:50.390Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cli.rs ImportArgs/ExportArgs/SyncArgs say that global flags must precede the provider because everything after it is forwarded. In fact the host still reads a global flag that directly follows the provider (clove export jsonl --no-index --out f works), and forwards everything from the first token that is not a global flag (clove export jsonl --out f --no-index fails with exit 1).
Expected: the doc comments and CLAUDE.md state this rule, and a test pins it.
Done when: cli_regressions::provider_arguments_start_at_the_first_non_global_token passes.