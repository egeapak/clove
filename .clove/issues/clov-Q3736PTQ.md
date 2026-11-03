---
schema: 1
id: clov-Q3736PTQ
title: 'agent-eval: add scenarios for blocked work, epics, review fixes, and ambiguous scope'
status: closed
type: chore
priority: 3
created: 2026-10-31T08:13:08.683Z
updated: 2026-10-31T08:15:33.724Z
closed: 2026-10-31T08:15:33.724Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
scripts/agent-eval/scenarios.json (s1-s12) has no case for:
- work that is blocked by an open dependency (the agent must check the graph before it starts);
- a large feature that needs an epic with child items (--parent);
- a list of review comments, each a separate fix;
- a request where only one of two items in the tracker is in scope (do not close the other);
- a request to close an item whose work is already done.
Add one scenario per case, with fixture items in make_fixture.sh and any scorer checks they need in score.py.
Done when: each new scenario scores 1.0 on a correct hand-made store and lower on a wrong one, and the README scenario table lists it.