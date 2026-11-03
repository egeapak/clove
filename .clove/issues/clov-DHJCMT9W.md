---
schema: 1
id: clov-DHJCMT9W
title: clove --deep still trusts the directory fast path for in-place edits older than the recent-mtime window
status: open
type: bug
priority: 2
created: 2026-10-20T18:47:04.000Z
updated: 2026-11-03T17:38:52.913Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-111
---
READ_PATH_ROADMAP §8. `stale::check_staleness` (the --deep path) opens with the same dir-mtime + file-count short-circuit as the fast path, guarded only by `any_recent`; an in-place (truncate-and-rewrite) edit whose mtime is outside the recent window and that leaves the directory mtime alone is invisible to both paths. --deep is the documented escape hatch, so it should always run the per-file pass. Regression test: in-place edit, backdate file + dir, assert check_staleness catches it.