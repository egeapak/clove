---
schema: 1
id: clov-89383ZHS
title: Resolve the crates.io name conflict for the 'clove' crate
status: closed
type: chore
priority: 1
created: 2026-07-18T00:14:24.000Z
updated: 2026-11-03T17:38:52.978Z
closed: 2026-07-18T12:05:49.000Z
assignee: egeapak
labels: [blocker, release]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-30
---
crates.io re-check on 2026-07-18: 'clove' is now TAKEN (unrelated ML framework, v0.1.9). HANDOFF said it was free 2026-06-02. 'cloved' is still free. Decide: rename the published crate, or skip crates.io and ship via GitHub Releases + a Homebrew tap.