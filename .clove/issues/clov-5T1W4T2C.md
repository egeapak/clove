---
schema: 1
id: clov-5T1W4T2C
title: 'Concurrent TUI model: background rescan behind locks + spinner'
status: closed
type: feature
priority: 2
created: 2026-07-18T00:14:24.000Z
updated: 2026-11-03T17:38:52.977Z
closed: 2026-07-18T11:59:05.000Z
labels: [perf, tui]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-31
---
Move the wholesale re-scan onto a background worker behind per-sub-struct locks and drive the 10fps is_busy() cadence with a spinner.