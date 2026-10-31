---
schema: 1
id: clov-6RC64GSZ
title: Measure CLOVE.md adherence per agent model
status: in_progress
type: chore
priority: 3
created: 2026-10-31T08:13:08.802Z
updated: 2026-10-31T08:15:33.732Z
labels: []
deps: [clov-7FE19H73, clov-Q3736PTQ]
relates: []
duplicates: []
supersedes: []
---
The agent-eval results so far cover one model (Claude Sonnet) and one run per scenario. Run the current CLOVE.md on every scenario 3 times with each of Claude Haiku, Sonnet, and Opus, using the repeat-run support in run_round.sh.
Done when: scripts/agent-eval/README.md has a table of the mean and minimum score per scenario and model, and every scenario-model pair with a mean below 0.9 has its own item that names the failure.