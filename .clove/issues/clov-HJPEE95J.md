---
schema: 1
id: clov-HJPEE95J
title: 'sync github: an assignee that isn''t a GitHub user aborts the whole sync with an opaque ''github api error: GitHub'''
status: closed
type: bug
priority: 1
created: 2026-10-06T18:19:07.000Z
updated: 2026-11-03T17:38:52.947Z
closed: 2026-10-21T07:24:45.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-76
---
Repro (v0.1.1, dogfood 2026-10-06): set a synced item's assignee to a name that isn't a GitHub user/collaborator (`dogfood-tui`), run `clove sync github egeapak/clove` → `error: io error at \`<github>\`: github api error: GitHub` (exit 5). GitHub rejects the issue update (assignee validation), the error Display drops GitHub's message, and the run aborts — every later sync fails the same way until the user guesses which item is at fault; earlier steps of the run (new issues created) are already applied. Clearing the assignee fixes it.

Fix: (1) surface octocrab's GitHub error body (status + message + which item), (2) don't let one item's rejection abort the run — skip/warn that item, or push without the unknown assignee and warn.