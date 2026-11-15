---
schema: 1
id: clov-1DBDXZ58
title: SDK license guard skips dev-dependencies and accepts a crate with no license
status: closed
type: bug
priority: 2
created: 2026-11-14T19:12:33.728Z
updated: 2026-11-14T19:17:24.177Z
closed: 2026-11-14T19:17:24.177Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The SDK license guard in .github/workflows/ci.yml runs `cargo tree -e normal,build` and greps the output for GPL. A copyleft crate under [dev-dependencies] of clove-types, clove-plugin or clove-plugin-echo passes. A crate with an empty `license` field prints an empty column and passes too. The guard is inline bash and cannot run locally or be unit-tested.
Done when: `cargo xtask guard-sdk-licenses` checks normal, build and dev edges, fails on a missing license, reads SPDX OR/AND expressions, has unit tests, and CI calls it.