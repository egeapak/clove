---
schema: 1
id: clov-CHM8QKSE
title: Evaluate the CLOVE.md directives on every agent-eval scenario
status: closed
type: chore
priority: 2
created: 2026-10-31T07:53:42.581Z
updated: 2026-10-31T08:00:00.485Z
closed: 2026-10-31T08:00:00.485Z
labels: []
deps: [clov-PQ0R4DSP]
relates: []
duplicates: []
supersedes: []
---
The current CLOVE.md text was scored on only some scenarios (s1, s5, s7). Run it on every scenario in scripts/agent-eval/scenarios.json, and run the 0.2.0 baseline (candidates/baseline-0.2.0.md) on the scenarios it has not seen, so both texts have a score for each scenario.
Done when: a comment on this item lists the per-scenario score of both texts, and every scenario where the current text scores below 1.0 has its own item with the failure and a proposed text change.