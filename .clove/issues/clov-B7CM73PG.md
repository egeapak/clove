---
schema: 1
id: clov-B7CM73PG
title: Token permission tests fail when the tests run as root
status: closed
type: bug
priority: 2
created: 2026-10-31T11:23:22.897Z
updated: 2026-10-31T18:04:40.725Z
closed: 2026-10-31T18:04:40.725Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Two unit tests rely on file modes that root ignores, so they fail when cargo test runs as uid 0 (for example in a container):
- crates/clove-core/src/daemon_token.rs an_unreadable_token_is_replaced_or_named (panics at line ~510: a 0o000 token is still readable, and a 0o500 .clove/ is still writable)
- crates/clove-ipc/src/client.rs an_unusable_token_is_not_sent (panics at line ~960)
The failure is the same on a clean checkout.
Expected: each test skips with a message when the effective uid is 0 (or when a 0o000 file is still readable).
Done when: cargo test -p clove-core -p clove-ipc --all-features passes as root and the two tests still run their checks as a normal user.