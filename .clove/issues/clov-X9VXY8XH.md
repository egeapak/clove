---
schema: 1
id: clov-X9VXY8XH
title: RELEASE_CHECKLIST has no daemon steps for in-place saves and a recreated issues directory
status: closed
type: docs
priority: 2
created: 2026-11-15T07:07:21.418Z
updated: 2026-11-15T07:07:21.479Z
closed: 2026-11-15T07:07:21.479Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
docs/RELEASE_CHECKLIST.md section 3 does not check that the daemon indexes an editor's in-place save (watch reports per item id), that it reloads a project whose .clove/issues was removed and recreated (git checkout to a commit without it and back), or that ready through the daemon carries the dangling-dependency warning.
Done when: section 3 has one step for each, with the command to run and the expected result.