---
schema: 1
id: clov-BMP6B7M2
title: .clove/write.lock is not git-ignored
status: open
type: bug
priority: 2
created: 2026-10-31T08:08:23.625Z
updated: 2026-10-31T08:08:23.625Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Every store write creates .clove/write.lock (the store-wide advisory lock, crates/clove-core/src/store.rs), but the .clove/.gitignore that clove init writes does not list write.lock, and the doctor gitignore-drift check does not expect it. The root .gitignore of this repo does not list .clove/write.lock either. So after the first write, git status shows .clove/write.lock as untracked, and a git add -A commits it.
Expected: write.lock is in the .clove/.gitignore template, the doctor drift check reports and fixes a .clove/.gitignore without it, and the root .gitignore lists .clove/write.lock.
Done when: after clove init and one clove new in a fresh git repo, git status --porcelain does not list .clove/write.lock, and a test covers it.