---
schema: 1
id: clov-9QB78SJ5
title: Make escargot-built test binaries safe for cargo-nextest
status: open
type: chore
priority: 2
created: 2026-11-03T08:11:48.348Z
updated: 2026-11-03T17:24:10.268Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-153
---
crates/clove/tests/support/mod.rs builds cloved and plugin binaries with escargot at most once per test process, guarded by a process-wide OnceLock. cargo-nextest runs each test in its own process, so tests rebuild (relink) the same binary while another test process spawns it. On macOS CI this failed clove-cli::daemon_cli a_clove_home_other_than_the_daemons_is_reported with 'could not start the daemon: No such file or directory'. CI therefore runs cargo test.
Possible fix: build the artifacts once before the test run (or under a cross-process file lock) and have the helpers reuse an existing up-to-date binary instead of calling cargo build in every process.
Done when: cargo nextest run --workspace --all-features passes repeatedly on ubuntu, macOS and Windows, and ci.yml uses it. Locally (4 cores) nextest ran the --all-features set in 94 s against 168 s for cargo test.