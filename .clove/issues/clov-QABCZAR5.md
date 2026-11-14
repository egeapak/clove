---
schema: 1
id: clov-QABCZAR5
title: 'sync github: a first comment sync duplicates a comment whose GitHub body has CRLF line breaks'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.611Z
updated: 2026-11-14T19:26:21.752Z
closed: 2026-11-14T19:26:21.752Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync.rs plan_comments matches a never-seen GitHub comment with its identical local twin by body_hash, which does not normalize line endings. GitHub can return a body with CRLF line breaks; with no sync state the comment is then pulled as a second local comment and the local one is pushed as a second GitHub comment.
Expected: twin matching uses link_hash (CRLF normalized), like the linked-pair comparison.
Done when: sync::tests::a_fresh_state_adopts_a_twin_that_differs_only_in_line_endings passes.