---
schema: 1
id: clov-XHW0PB77
title: The publish order is a hand-kept list in three places, and docs/RELEASE.md §4 omits clove-service
status: closed
type: bug
priority: 2
created: 2026-11-14T19:12:33.386Z
updated: 2026-11-14T19:17:24.118Z
closed: 2026-11-14T19:17:24.118Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The crates.io publish order is written in scripts/release/publish.sh, in the list of docs/RELEASE.md §3, and in the command block of §4. The §4 block has no `cargo publish -p clove-service`, so a publish that follows it fails at clove-mcp after eight publishes that cannot be undone.
Done when: `cargo xtask publish-order` derives the order from cargo metadata, publish.sh reads it, an xtask test checks the real workspace order, and RELEASE.md holds no hand-kept list.