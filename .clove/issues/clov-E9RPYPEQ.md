---
schema: 1
id: clov-E9RPYPEQ
title: Existing clones keep write.lock untracked until doctor --fix
status: closed
type: feature
priority: 2
created: 2026-10-31T08:41:27.454Z
updated: 2026-10-31T09:33:19.623Z
closed: 2026-10-31T09:33:19.623Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
GITIGNORE_ENTRIES (crates/clove-core/src/config.rs) lists write.lock, but only clove init and clove doctor --fix write it. In a project initialized by an older clove, .clove/write.lock shows as untracked after the first write, and git add -A commits it, until someone runs clove doctor --fix. daemon.token already has an automatic top-up: ensure_gitignored in crates/clove-core/src/daemon_token.rs adds its line to .clove/.gitignore when the token is created.
Expected: when ItemStore::write_lock (crates/clove-core/src/store.rs) creates .clove/write.lock, it adds write.lock to an existing .clove/.gitignore that lacks it, in the same way (refuse a symlinked .gitignore, atomic write). Consider generalizing ensure_gitignored to take the entry.
Done when: a test writes a .gitignore without write.lock, runs one write, and git status --porcelain .clove lists no write.lock.