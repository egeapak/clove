---
schema: 1
id: clov-7FE19H73
title: 'agent-eval: repeat runs per scenario and add a weaker agent model'
status: open
type: chore
priority: 3
created: 2026-10-31T08:00:00.494Z
updated: 2026-10-31T08:00:00.494Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
scripts/agent-eval/run_round.sh runs each scenario once. One run is not enough to compare two directives texts: the same text and scenario scored 0.0 in one run and 1.0 in another. Also, with Claude Sonnet as the agent, s8-s12 score 1.0 for both the 0.2.0 and the current CLOVE.md, so they do not separate the texts.
Expected: run_round.sh takes a repeat count (for example --runs 3) and reports the mean and the minimum score per scenario; the README documents how to run a round with a weaker model (for example through AGENT_CMD with a smaller model) so differences between texts show.
Done when: a round with --runs 3 prints per-scenario mean and minimum, and the README results table states the run count and the agent model.