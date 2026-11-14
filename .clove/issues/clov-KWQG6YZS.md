---
schema: 1
id: clov-KWQG6YZS
title: 'tui: small form, filter, help, and snapshot gaps'
status: closed
type: bug
priority: 3
created: 2026-11-14T19:03:04.192Z
updated: 2026-11-14T19:14:12.414Z
closed: 2026-11-14T19:14:12.414Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
1. app/mod.rs rebuild_facets: an active filter value that no item has after a refresh stays in list.filter but leaves the menu, so it can only be removed with x (clear all).
2. lib.rs: Up/Down always change the form field, so a multi-line Body can only be navigated with Left/Right.
3. ui/help.rs: the help text does not scroll when it falls back to full screen on a short terminal.
4. ui/util.rs wrapped_height sums into u16: debug panic or release wrap above 65535 rows.
5. app/mod.rs module doc says the TUI never mutates anything; the form writes.
6. snapshot.rs shapes are 40x48, 120x18, 60x60; 80x24 is not covered.
Done when: each point has a fix and a test (or a snapshot shape for 6).