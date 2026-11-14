---
schema: 1
id: clov-5VRBBKYS
title: 'import beads: a second import duplicates an issue whose unmapped fields changed'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:12.959Z
updated: 2026-11-14T19:26:21.692Z
closed: 2026-11-14T19:26:21.692Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/beads.rs build_external_ref makes the idempotency key 'beads:<id> meta:{all unmapped fields}'. updated_at and other unmapped fields are in the blob, so after an edit in beads the key differs, is_imported misses, and the issue is imported again as a second item.
Expected: an issue is recognized as imported by the 'beads:<id>' token alone.
Done when: the beads test that re-imports a line with a changed updated_at reports already_imported and creates nothing.