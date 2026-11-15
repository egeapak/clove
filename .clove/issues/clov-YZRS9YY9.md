---
schema: 1
id: clov-YZRS9YY9
title: Move the inline CI dependency guards and the publish order into cargo xtask
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:34.216Z
updated: 2026-11-14T19:17:24.247Z
closed: 2026-11-14T19:17:24.247Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Three dependency-tree guards live as inline bash in .github/workflows/ci.yml (SDK license guard, registry TLS stays on ring, no clove-web in the default tree), so they cannot run locally and have no tests. scripts/release/publish.sh keeps its own crate list.
Done when: `cargo xtask guard-sdk-licenses`, `guard-tls` and `guard-no-web` exist with unit tests and ci.yml calls them, and publish.sh takes its order from `cargo xtask publish-order`. scripts/release/pack-web-dist.sh and scripts/ci/check-dco.sh are further candidates.