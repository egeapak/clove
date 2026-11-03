---
schema: 1
id: clov-MH8RC6G7
title: 'flaky: cloved daemon_lifecycle an_idle_hub_exits_after_its_projects_idle_out under CPU load'
status: open
type: bug
priority: 3
created: 2026-10-20T18:47:02.000Z
updated: 2026-11-03T17:38:52.917Z
labels: [test]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-108
---
Failed once in `cargo test --workspace --all-features` (2026-10-20, fix/0.2.0-issues, machine loaded by two parallel cargo builds): panicked at crates/cloved/tests/support/mod.rs:81:27 'the hub serves the project: Transport("the connection to the server was already shutdown")'. Passed 5/5 in isolation right after. Likely the idle-shutdown knob fires before the first request under load; the test should wait for readiness / use a margin relative to the knob rather than wall-clock.