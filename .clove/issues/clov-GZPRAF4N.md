---
schema: 1
id: clov-GZPRAF4N
title: 'daemon tier answers blocked slower than the index: GraphRequest::Blocked carries no filter/window'
status: closed
type: feature
priority: 2
created: 2026-10-20T18:47:06.000Z
updated: 2026-11-03T17:38:52.908Z
closed: 2026-10-21T07:24:49.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-114
---
READ_PATH_ROADMAP §8: at 700 items / 83 blocked, `blocked --limit 1` daemon 29 ms vs index 10 ms vs files 33 ms; the engine hydrates the whole blocked set before filtering/windowing. Fix = push filter + window into the daemon request (protocol change). Protocol 10 is unreleased in 0.2.0, so extending it now avoids a protocol 11 bump later.