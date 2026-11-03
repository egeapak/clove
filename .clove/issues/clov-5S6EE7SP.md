---
schema: 1
id: clov-5S6EE7SP
title: sync github pull mints fresh ids and leaves clove-meta deps dangling
status: closed
type: bug
priority: 1
created: 2026-11-03T17:28:04.148Z
updated: 2026-11-03T17:42:40.989Z
closed: 2026-11-03T17:42:40.989Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-170
---
crates/clove-import/src/sync_net.rs pull_create() always calls store.new_id(), ignoring the clove-meta id in the issue body, but copies clove-meta deps verbatim. Deps that name other pulled issues then point at ids that no longer exist. After pulling egeapak/clove (74 issues) into an empty store, clove doctor reports 12 DANGLING_REF errors.
Expected: when the clove-meta id parses and no local item has it, the pulled item keeps that id, so deps and id mentions in bodies and comments stay valid. A taken or invalid id still gets a fresh id.
Done when: a crates/clove/tests/sync_github.rs mock test pulls two issues where one depends on the other by clove-meta id, and clove doctor reports no DANGLING_REF; docs/DESIGN.md §11.3 and the github.rs mapping table describe the rule.