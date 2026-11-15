---
schema: 1
id: clov-Z7YWSSAP
title: 'Test run fragility: no per-test timeout, floating nextest, size-only plugin check'
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:33.608Z
updated: 2026-11-14T19:20:42.702Z
closed: 2026-11-14T19:20:42.702Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
.config/nextest.toml has no slow-timeout, so a hung test waits for the 60-minute CI job limit. CI installs the latest cargo-nextest while the config uses the experimental setup-scripts key. support::plugin() in crates/clove/tests/support/mod.rs accepts a plugin binary beside `clove` when only its length equals this run's build. The daemon suites share one clove home at <repo>/target/test-clove-home, a path that ignores CARGO_TARGET_DIR.
Done when: nextest kills and fails a hung test, CI installs a fixed nextest version, and plugin() compares the files and not only their length.