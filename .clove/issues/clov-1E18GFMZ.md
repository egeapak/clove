---
schema: 1
id: clov-1E18GFMZ
title: Build the cloved and plugin test binaries once per nextest run
status: in_progress
type: chore
priority: 2
created: 2026-11-04T14:38:02.843Z
updated: 2026-11-04T15:21:42.004Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The daemon and plugin suites in crates/clove/tests build cloved and the plugins with escargot inside test processes (crates/clove/tests/support/mod.rs). Each process can run its own cargo build, and different suites ask for different feature sets, so tests do not all run the same binaries.
Change: a nextest setup script (.config/nextest.toml) builds cloved and the plugins once per run with --all-features, before any test. The plugins go beside the clove binary under test; cloved goes into its own dir that the setup script passes to the tests through NEXTEST_ENV. support::cloved()/plugin() only look the binaries up, and escargot is removed. cargo nextest is the supported test runner; docs (CLAUDE.md, CONTRIBUTING.md, docs/RELEASE*.md) say so, and plain cargo test fails these suites with a message that names nextest.
Done when: cargo nextest run --workspace --all-features and cargo nextest run -p clove-cli -p cloved pass, and CI passes on ubuntu, macOS and Windows.