---
schema: 1
id: clov-EJ0FK4BP
title: daemon_race test times out intermittently on macOS CI
status: open
type: bug
priority: 2
created: 2026-10-31T11:35:58.172Z
updated: 2026-10-31T11:35:58.172Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/tests/daemon_race.rs::stopping_the_last_project_while_another_starts_never_strands_it failed once on GitHub's macos-latest runner: panicked at daemon_race.rs:101:47 with 'A starts: Timeout' (clove_ipc::ensure_daemon_at(&hub, &a) returned ClientError Timeout in one of the 25 rounds). The same commit passed on a re-run, and the test passes on Linux. Run: egeapak/clove actions run 41193004283, job 124513463042 (2026-10-31).
Likely area: the window where a hub has decided to exit (last project detached) while a new ensure_daemon_at spawns or attaches; the start can wait past its deadline on a slow runner.
Expected: ensure_daemon_at never times out in this race; it either attaches to the exiting hub before it exits or starts a fresh one.
Done when: the root cause is found and fixed, and the test passes repeatedly on macOS (for example, run it 20 times in a loop in CI or locally on macOS).