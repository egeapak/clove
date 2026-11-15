---
schema: 1
id: clov-MH3ZJRKK
title: No MSRV and no pinned toolchain version
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:34.312Z
updated: 2026-11-14T19:33:01.961Z
closed: 2026-11-14T19:33:01.961Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
No manifest sets `rust-version`. rust-toolchain.toml has `channel = "stable"`, and CI and release.yml install the current stable. A new stable clippy can fail master with no code change, release binaries are built with whatever stable is current on the tag day, and `cargo install clove-cli` on an older rustc fails with compile errors and no version message. The rusqlite comment in the root Cargo.toml refers to a pinned toolchain that does not exist.
Done when: rust-toolchain.toml names a version, [workspace.package] sets rust-version, and a CI job builds with that MSRV.