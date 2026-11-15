---
schema: 1
id: clov-ZTC0QEX0
title: 'cloved: the watcher debounce has no maximum wait'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:43.107Z
updated: 2026-11-14T19:33:51.230Z
closed: 2026-11-14T19:33:51.230Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/watcher.rs collect_burst restarts the quiet window on every event. A writer that touches .clove/issues more often than watch_debounce_ms keeps the batch from ever being applied.
Done when: a burst ends after a bounded time, and watcher::tests::a_directory_that_never_goes_quiet_still_flushes passes.