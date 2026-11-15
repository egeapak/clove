---
schema: 1
id: clov-NTVBM4CF
title: cargo about generate --fail first runs after the release tag is pushed
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:33.662Z
updated: 2026-11-14T19:19:12.742Z
closed: 2026-11-14T19:19:12.742Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
about.toml `accepted` is a hand copy of deny.toml `[licenses] allow` plus AGPL-3.0-only. `cargo about generate --fail` runs only in .github/workflows/release.yml, so a dependency that cargo-deny accepts and cargo-about rejects is found after the tag exists.
Done when: an xtask test fails when the two lists differ, and CI runs the generate step for every release target.