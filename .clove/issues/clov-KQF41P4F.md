---
schema: 1
id: clov-KQF41P4F
title: 'Cut CI wall time: cache thrash, bench LTO rebuild, serial tests'
status: in_progress
type: chore
priority: 2
created: 2026-11-03T07:49:46.157Z
updated: 2026-11-03T07:49:56.679Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
In .github/workflows/ci.yml every PR job saved its own Swatinem/rust-cache entry (~4.7 GB per run: three ~1.1 GB test caches plus perf/daemon/feature/fuzz). That exceeds the 10 GB repository cache limit after about two runs, so caches are evicted and most Rust jobs log 'No cache found'. The test job's 'cargo bench --workspace --no-run' builds the workspace in the bench profile, which inherits release (fat LTO, codegen-units=1): 3.5 to 12 minutes per OS. Clippy and tests run in one job, one after another, and cargo test runs test binaries serially.
Fix: save caches only on master (save-if), split lint (fmt + clippy) from test, run tests with cargo-nextest (.config/nextest.toml profile ci), run doc tests with cargo test --doc, compile benches with cargo test --benches --no-run (test profile), and add a concurrency group that cancels superseded PR runs.
Done when: CI is green on the PR with these changes and the test jobs no longer spend minutes on the bench step.