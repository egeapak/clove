---
schema: 1
id: clov-FEFMJP2B
title: Committed .clove/config.toml enables daemon git_sync and periodic GitHub sync without local consent
status: closed
type: feature
priority: 2
created: 2026-11-14T19:04:18.777Z
updated: 2026-11-14T19:31:11.465Z
closed: 2026-11-14T19:31:11.465Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/config.rs [daemon] git_sync, github_sync_interval_min, github_sync_repo and [web] port are read from the committed .clove/config.toml. crates/cloved/src/slot.rs acts on them as soon as the hub loads the project (clove daemon start, or clove mcp from .mcp.json). In a fresh clone the daemon then commits under the user's git identity and runs a two-way sync against origin with the user's gh token.
Options: (a) require a per-clone opt-in stored outside the repository (under clove home) before the daemon acts on these keys; (b) print a one-time notice and require clove daemon trust; (c) keep the current behaviour and document it.
Done when: a clone whose config sets these keys does not commit or sync until the local opt-in exists.