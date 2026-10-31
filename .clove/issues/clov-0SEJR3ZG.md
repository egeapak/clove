---
schema: 1
id: clov-0SEJR3ZG
title: .clove/.gitignore does not ignore write.lock
status: open
type: bug
priority: 2
created: 2026-10-31T06:53:05.193Z
updated: 2026-10-31T06:53:05.193Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
clove creates the store-wide advisory write lock at .clove/write.lock (crates/clove-core/src/store.rs) on the first write. GITIGNORE_ENTRIES in crates/clove-core/src/config.rs, which `clove init` writes to .clove/.gitignore and `clove doctor` checks (GITIGNORE_DRIFT), does not list write.lock. So after the first write, `git status` shows .clove/write.lock as untracked, and `git add .clove` commits a runtime lock file into the source of truth.
Expected: write.lock is in GITIGNORE_ENTRIES, and `clove doctor` reports an existing .clove/.gitignore without it as drift (and repairs it with its usual fix).
Done when: a test runs `clove init` and a write, and `git status --porcelain .clove` lists no write.lock.