---
schema: 1
id: clov-CMZHPCWS
title: cloved ready answers carry no dangling-dependency warnings
status: closed
type: bug
priority: 2
created: 2026-11-15T06:56:10.151Z
updated: 2026-11-15T06:56:19.170Z
closed: 2026-11-15T06:56:19.170Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/ipc.rs handle_query returns warnings: Vec::new() for every mode. The index and file tiers name each active item that ready leaves out for a missing hard dependency in _meta.warnings; a clove ready served by the daemon drops that warning, so the item disappears from ready with no notice.
Expected: in Ready mode handle_query calls Index::ready_warnings(&filter, residue) and returns the list.
Done when: crates/clove/tests/daemon_routing.rs ready_through_the_daemon_warns_about_dangling_deps passes (daemon source, same warnings as --no-index).