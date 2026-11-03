---
schema: 1
id: clov-VP11JGZZ
title: Publish the workspace crates to crates.io
status: closed
type: feature
priority: 2
created: 2026-09-23T11:40:25.000Z
updated: 2026-11-03T17:38:52.975Z
closed: 2026-10-20T16:36:59.000Z
labels: [release]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-53
---
Publish the 15 workspace crates to crates.io in the order in `docs/RELEASE.md` §3, one at a time, confirming each is live before the next:

clove-types → clove-core → clove-plugin → clove-index → clove-import → clove-ipc → clove-tui → clove-engine → clove-mcp → clove-web → cloved → clove-sync-github → clove-import-tk → clove-import-beads → clove-cli

Blocked on cutting 0.1.1 (fixes for bugs found while testing the v0.1.0 release binaries).

- [ ] crates.io account has a verified email
- [ ] `clove-web` packages the real SPA (~50 `dist-gz/` files, not the placeholder)
- [ ] after publish: the three plugins show up as reverse-deps of `clove-plugin` (else `clove plugin list` is empty)