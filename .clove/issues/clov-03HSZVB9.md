---
schema: 1
id: clov-03HSZVB9
title: Three tests fail when run as root or without IPv6
status: closed
type: chore
priority: 2
created: 2026-10-31T08:08:33.979Z
updated: 2026-10-31T08:41:27.478Z
closed: 2026-10-31T08:41:27.478Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
cargo test --workspace --all-features fails in an environment that runs as root (uid 0) or has no IPv6:
- clove-core daemon_token::tests::an_unreadable_token_is_replaced_or_named (crates/clove-core/src/daemon_token.rs:510) and clove-ipc client::tests::an_unusable_token_is_not_sent: root can read a mode-000 token file, so the expected read error does not happen.
- clove-cli tests/web_port.rs explicit_host_skips_the_daemon_hand_off: binding [::1] fails with 'Address family not supported by protocol (os error 97)'.
Expected: each test skips itself (with a printed reason) when its precondition does not hold: effective uid 0, or no IPv6 loopback.
Done when: cargo test --workspace --all-features passes as root in a container without IPv6.