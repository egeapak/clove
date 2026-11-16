---
schema: 1
id: clov-YXT1NG38
title: Review the RUSTSEC ignores in deny.toml
status: closed
type: chore
priority: 2
created: 2026-11-15T06:50:23.608Z
updated: 2026-11-15T07:10:49.475Z
closed: 2026-11-15T07:10:49.475Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
deny.toml ignores RUSTSEC-2023-0071, RUSTSEC-2026-0387, RUSTSEC-2024-0436, RUSTSEC-2026-0192 and RUSTSEC-2026-0462. An ignore whose crate is no longer in Cargo.lock, or which has a compatible fix release, can be dropped.
Done when: each ignore is either removed (cargo deny check advisories still passes) or still has a current reason.