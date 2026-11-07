---
schema: 1
id: clov-5GJ9XVT0
title: Tests leave temp dirs behind under cargo-nextest
status: closed
type: bug
priority: 2
created: 2026-11-04T16:55:39.828Z
updated: 2026-11-06T22:42:10.617Z
closed: 2026-11-04T16:55:40.110Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-187
---
crates/clove-tui/src/snapshot.rs (app(), edge_app()) and crates/cloved/src/graph_cache.rs (tests::cache()) call std::mem::forget on a TempDir, and crates/clove/tests/plugin_install.rs keeps its PATH mirrors in a static Vec<TempDir>, which is never dropped. Under nextest every test is its own process, so each run leaves about 25 dirs in the system temp dir.
Done when: a full cargo nextest run --workspace --all-features adds no entry to the temp dir, and clippy rejects std::mem::forget, TempDir::keep and TempDir::into_path (clippy.toml).