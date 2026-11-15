---
schema: 1
id: clov-3CBQ39YN
title: 'sync github: the issue-list retry ignores Retry-After and the 403 rate-limit wait'
status: open
type: bug
priority: 2
created: 2026-11-15T07:01:15.848Z
updated: 2026-11-15T07:01:15.848Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync_net.rs retries each issue-list page on a transient error with a fixed backoff. It does not wait for a Retry-After header or for X-RateLimit-Reset on a 403/429 rate-limit response.
Done when: a rate-limited page waits the time GitHub names (bounded) before the retry, and a mock-server test in crates/clove/tests/sync_github.rs covers a 429 with Retry-After.