---
schema: 1
id: clov-ZB5338AW
title: cloved stops serving a project after .clove/issues is removed and created again
status: open
type: bug
priority: 2
created: 2026-11-18T21:58:06.838Z
updated: 2026-11-18T21:58:06.838Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
When .clove/issues is removed and created again while cloved serves the project (for example `git checkout` of a commit without the store, then back), the watcher reports Sync::Lost (crates/cloved/src/watcher.rs ~297) and the hub unloads the project (crates/cloved/src/hub.rs ~475). The watcher doc says the next call loads it again, but CLI reads and writes attach with load=false (DaemonClient::probe, crates/clove-ipc/src/client.rs:243-253), so nothing reloads it: `clove -f json ls` answers from `index`, `clove new` writes without the daemon, and `clove -f json daemon status` no longer lists the project (running=false) until `clove daemon start` runs again.

Repro: git init; clove init; commit; clove new one; clove new two; commit; clove daemon start; git checkout <first commit>; git checkout main; clove -f json daemon status.

Expected (docs/RELEASE_CHECKLIST.md §3 'A recreated .clove/issues is picked up'): the project is still served; daemon status lists it.
Done when: the hub reloads the project with a new watch once issues/ exists again (or the next client call reloads it), and a cloved test covers daemon status after the recreate.