---
schema: 1
id: clov-R9VZNC2B
title: tier_parity edit() helper overwrites fixture 'updated' with Utc::now()
status: closed
type: chore
priority: 4
created: 2026-10-08T10:07:05.000Z
updated: 2026-11-03T17:38:52.946Z
closed: 2026-10-18T13:36:08.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-78
---
crates/clove-engine/tests/tier_parity.rs: the edit() helper calls store.update(.., Utc::now()), so the fixture's intended 'updated counts down' ordering is really wall-clock order. Noted by the 0.1.2 timestamp work.