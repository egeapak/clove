---
schema: 1
id: clov-GKVS01CJ
title: clove doctor --fix rebuilds the index beside a running daemon
status: closed
type: bug
priority: 2
created: 2026-11-14T19:34:31.052Z
updated: 2026-11-14T19:44:50.333Z
closed: 2026-11-14T19:44:50.333Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/doctor.rs calls clove_index::reindex directly for --fix. A running cloved keeps its handle on the replaced index file until its next reopen. clove reindex asks the daemon to rebuild first (crates/clove/src/cmd/reindex.rs).
Expected: doctor --fix uses the same daemon-first rebuild as clove reindex.
Done when: doctor --fix calls cmd::reindex::rebuild and the doctor and reindex test suites pass.