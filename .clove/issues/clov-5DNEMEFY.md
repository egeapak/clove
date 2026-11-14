---
schema: 1
id: clov-5DNEMEFY
title: 'CI is red on master: two RUSTSEC advisories and a clippy 1.100 lint on Windows'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:39:30.635Z
updated: 2026-11-14T19:39:30.694Z
closed: 2026-11-14T19:39:30.694Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
cargo-deny fails on RUSTSEC-2026-0459 (libsqlite3-sys 0.35 bundles SQLite older than 3.54; fixed by rusqlite 0.41) and on RUSTSEC-2026-0462 (rmcp 1.7 Streamable HTTP clients; clove compiles only the stdio server features). The Windows lint job fails on clippy::needless_bool in crates/clove-core/src/daemon_token.rs is_private_dir with Rust 1.100.
Done when: cargo deny check advisories passes, and cargo clippy --workspace --all-targets --all-features -- -D warnings passes on Rust 1.100 on Linux and Windows.