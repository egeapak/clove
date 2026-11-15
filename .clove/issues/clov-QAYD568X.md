---
schema: 1
id: clov-QAYD568X
title: clove-import declares an unused anyhow dependency
status: closed
type: chore
priority: 2
created: 2026-11-15T06:50:23.568Z
updated: 2026-11-15T07:06:26.292Z
closed: 2026-11-15T07:06:26.292Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
cargo reports dependency anyhow in crates/clove-import/Cargo.toml as unused.
Done when: the dependency is removed or used, and cargo build -p clove-import --all-features prints no manifest warning.