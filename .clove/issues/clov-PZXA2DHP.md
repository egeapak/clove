---
schema: 1
id: clov-PZXA2DHP
title: 'import tk/beads: dependency, parent and link ids of the source tracker are parsed as clove ids'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:12.936Z
updated: 2026-11-14T19:26:21.676Z
closed: 2026-11-14T19:26:21.676Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/tk.rs parse_ids and crates/clove-import/src/beads.rs dependency mapping call CloveId::new on the source tracker's ids. A tk ticket with deps: [nw-5c46] aborts the whole import; a beads line with dependencies [{id: bd-101}] is skipped as malformed, and edges in the issue_id/depends_on_id shape are dropped silently. A reference is never mapped to the id minted for the imported item, so edges inside one import always dangle.
Expected: references are resolved through a source-id to new-id map (this batch plus items imported earlier, by external_ref); an unresolved edge is dropped with a warning.
Done when: the tk and beads tests with real source ids in deps/parent/links pass.