---
schema: 1
id: clov-YT2TMZTV
title: 'tui form: submit error is not visible when the body is longer than the popup'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:04.111Z
updated: 2026-11-14T19:09:45.438Z
closed: 2026-11-14T19:09:45.438Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/ui/form.rs appends the form error after the Body rows and scrolls to follow the caret, and ui/status.rs does not show form.error. At 80x24 with a 15-line body, a failed Ctrl-S (for example an invalid dep id) draws the error nowhere on screen.
Expected: the error is drawn in a fixed row of the popup that is always visible.
Done when: a render test at 80x24 with a long body and a failed submit finds the error text in the buffer, with focus on Title and on Body.