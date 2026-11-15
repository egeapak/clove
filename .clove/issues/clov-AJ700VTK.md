---
schema: 1
id: clov-AJ700VTK
title: 'sync github: a transient failure of the issue list aborts the sync without a retry'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.631Z
updated: 2026-11-14T19:26:21.764Z
closed: 2026-11-14T19:26:21.764Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/github.rs fetch_all sends each issue-list page once. A 5xx, 408, 429 or transport error on a page fails the whole sync, while every other idempotent call goes through with_retry (3 attempts with backoff).
Expected: each issue-list page is retried like the comment-list pages.
Done when: crates/clove/tests/sync_github.rs::issue_list_is_retried_on_5xx passes.