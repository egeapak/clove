---
schema: 1
id: clov-PQ0R4DSP
title: Add agent-eval scenarios for planning, deferral, near-duplicates, and questions
status: closed
type: chore
priority: 2
created: 2026-10-31T07:53:42.351Z
updated: 2026-10-31T07:56:37.182Z
closed: 2026-10-31T07:56:37.182Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
scripts/agent-eval/scenarios.json covers doing work now (s1-s7) but not these cases:
- a request to only plan work (items must stay open);
- a request that does one task now and defers another (one closed, one open, no deadline text in the item);
- an open item that describes the requested task in different words (the agent must reuse it, not file a duplicate);
- a single task with several investigation steps (one item);
- a question that asks for no change (no items).
Add one scenario per case, with fixture data in make_fixture.sh and the scorer checks they need (open status, duplicate pattern) in score.py.
Done when: each new scenario scores 1.0 on a hand-made correct store and lower on a wrong one, and the README scenario table lists them.