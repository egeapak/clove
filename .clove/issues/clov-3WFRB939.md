---
schema: 1
id: clov-3WFRB939
title: 'sync github: a change of only the assignee on GitHub is never pulled'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:51.537Z
updated: 2026-11-14T19:26:21.724Z
closed: 2026-11-14T19:26:21.724Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync.rs decide_linked: with a recorded gh_content_hash, a remote change needs a different content fingerprint, and content_fingerprint leaves the assignee out. An issue whose only change on GitHub is its assignee is reported in sync and the local assignee stays stale.
Expected: the sync state records the remote assignee logins (sorted) and a different set counts as a remote change.
Done when: crates/clove/tests/sync_github.rs::a_remote_change_of_only_the_assignee_is_pulled passes.