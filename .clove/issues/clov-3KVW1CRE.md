---
schema: 1
id: clov-3KVW1CRE
title: 'sync github: a linked item whose issue was deleted makes every comment sync fail'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:12.904Z
updated: 2026-11-14T19:26:21.654Z
closed: 2026-11-14T19:26:21.654Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync_net.rs plan_comment_threads fetches the comment thread of every local item with a gh-N external_ref, also when the issue list did not return issue N (plan_sync reports it as remote_missing). GitHub answers 404 for that thread, the error ends the comment phase, and each later sync (also --dry-run) exits non-zero; threads sorted after it are never reconciled.
Expected: threads of issues absent from the fetched issue list are skipped.
Done when: crates/clove/tests/sync_github.rs a_deleted_issue_does_not_break_the_comment_sync_of_the_others and remote_missing_is_reported_not_fatal pass with a mock that answers 404 for the comments of an unknown issue.