---
schema: 1
id: clov-7D3YAKH3
title: Index perf gates fail under load without a code change
status: closed
type: bug
priority: 2
created: 2026-11-15T06:50:23.531Z
updated: 2026-11-15T07:06:09.471Z
closed: 2026-11-15T07:06:09.471Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The index performance gates (reindex under 1 s, ready_lean under 15 ms) take one sample. Under load, runs measured 1.163 s and 15.15 ms and failed.
Done when: each gate takes the best of several samples, so one slow sample does not fail it, while a real regression (every sample over budget) still fails.