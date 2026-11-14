---
schema: 1
id: clov-M75ZB0BK
title: The .woodpecker pipelines no longer mirror the GitHub workflows
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:34.512Z
updated: 2026-11-14T19:33:36.746Z
closed: 2026-11-14T19:33:36.746Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
.woodpecker/lint-test.yml has no SDK license guard, no web-feature arm and no daemon smoke. .woodpecker/release.yml has no tag/version check, no third-party notices and no web-dist asset. Each file says it mirrors a GitHub job.
Done when: the .woodpecker directory is removed, or each pipeline matches its GitHub job, or the files state that they are not maintained.