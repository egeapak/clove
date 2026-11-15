---
schema: 1
id: clov-G7W86WJ5
title: clove-import declares anyhow but does not use it
status: closed
type: chore
priority: 2
created: 2026-11-15T07:01:15.790Z
updated: 2026-11-15T07:03:58.073Z
closed: 2026-11-15T07:03:58.073Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/Cargo.toml lists anyhow.workspace = true; no source file uses it (cargo warns: unused_dependencies).
Done when: the dependency is removed and cargo build -p clove-import gives no unused-dependency warning.