---
schema: 1
id: clov-3YPPXG9S
title: 'sync github: clove-meta in an issue body is trusted for the id, type, priority and deps of local items'
status: closed
type: feature
priority: 2
created: 2026-11-14T19:15:06.198Z
updated: 2026-11-14T19:26:21.785Z
closed: 2026-11-14T19:26:21.785Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/github.rs map_issue reads id, type, priority and deps from the clove-meta HTML comment of the issue body. Anyone who can edit the issue body (or open an issue, for a pull-create) can set these on the local item: reuse a free clove id, change the type or priority, add or remove deps on existing items. The deps go through the checked dep op, so they cannot dangle or cycle, but they are still applied.
Open choice: trust the meta of every issue; trust it only for issues this store pushed (recorded in the sync state); or trust it only when the last editor is a repository collaborator.
Done when: the rule is chosen, docs/DESIGN.md §11.3 states it, and map_issue or the planner enforces it with a test in crates/clove/tests/sync_github.rs.