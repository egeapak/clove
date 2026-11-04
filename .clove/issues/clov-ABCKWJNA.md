---
schema: 1
id: clov-ABCKWJNA
title: clove dep tree --format json omits repeat_ref (MCP/web emit it)
status: closed
type: bug
priority: 3
created: 2026-10-20T18:47:04.000Z
updated: 2026-11-04T09:54:20.336Z
closed: 2026-11-04T09:54:20.336Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-112
---
READ_PATH_ROADMAP §8. Three tree_to_json implementations (cmd/dep.rs, ops.rs, clove-engine); the CLI one drops repeat_ref. Unify on one serializer so the shape is identical on every surface; make the key required again in docs/json-schema/v1/dep-tree.json.