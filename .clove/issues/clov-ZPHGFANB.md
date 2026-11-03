---
schema: 1
id: clov-ZPHGFANB
title: 'sync github: propagate comment deletions both ways'
status: closed
type: feature
priority: 3
created: 2026-10-08T10:07:06.000Z
updated: 2026-11-03T17:38:52.944Z
closed: 2026-10-21T07:24:42.000Z
labels: [sync]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-80
---
A comment deleted on GitHub stays in clove, and a local comment deleted after being pushed stays on GitHub. Needs the same local↔GitHub comment link as edit sync; decide whether deletion propagates automatically or needs an explicit flag (destructive).