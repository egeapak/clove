---
schema: 1
id: clov-FAE0G5TC
title: 'sync github: a pull from an issue without clove-meta resets the local priority to the default'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.556Z
updated: 2026-11-14T19:26:21.728Z
closed: 2026-11-14T19:26:21.728Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/github.rs map_issue maps an issue with no clove-meta priority to Priority::DEFAULT, and sync_net.rs pull_update always sends priority in the EditRequest. A pull of any edit of such an issue sets the local priority to 2. content_equal also reports a difference for a local non-default priority.
Expected: StagedIssue.priority is None when the body carries none; pull_update and content_equal then leave the priority alone (as they do for the type).
Done when: crates/clove/tests/sync_github.rs::a_pull_from_an_issue_without_meta_keeps_the_local_priority passes.