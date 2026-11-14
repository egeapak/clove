---
schema: 1
id: clov-XE3MDP4S
title: 'sync github: a store with no sync state and issues without clove-meta can be synced against the wrong repository'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:15:06.218Z
updated: 2026-11-14T19:26:21.790Z
closed: 2026-11-14T19:26:21.790Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync.rs foreign_links_reason refuses a sync when the local gh-N links belong to another repository only if there is evidence: another repository's state file records them, or the linked issues carry clove-meta ids of other items. A fresh clone (the state under .clove/sync/ is git-ignored) synced against a mistyped repository whose issues carry no clove-meta has no evidence, so the items are paired with unrelated issues by number and the conflict policy overwrites one side.
Open choice: store the repository in the link (external_ref gh-N becomes owner/repo#N, a format change), or record the synced repository in tracked config (.clove/config.toml) and refuse any other without --adopt-links.
Done when: a first sync of a store with gh-N links against a repository other than the recorded one is refused, with a test in crates/clove/tests/sync_github.rs.