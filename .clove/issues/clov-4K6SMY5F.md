---
schema: 1
id: clov-4K6SMY5F
title: Measure id-plus-title adherence per model on all agent-eval scenarios
status: open
type: chore
priority: 2
created: 2026-10-31T10:01:43.244Z
updated: 2026-10-31T10:01:43.244Z
labels: []
deps: [clov-1S95372Y, clov-P5PRP3S2, clov-ZSM0NJ2E]
relates: []
duplicates: []
supersedes: []
---
After the directive and the tool output name items by id and title, run the current CLOVE.md on all 17 agent-eval scenarios, 3 runs each, with Claude Haiku, Sonnet, and Opus (153 runs).
Done when: scripts/agent-eval/README.md has the per-scenario and per-model table including the new reply check, and every scenario-model pair with a mean below 0.9 has its own item that names the failure.