---
schema: 1
id: clov-B39CGPZQ
title: Add an agent-adherence evaluation harness for the directives
status: closed
type: chore
priority: 2
created: 2026-10-31T06:46:18.371Z
updated: 2026-10-31T06:50:43.645Z
closed: 2026-10-31T06:50:43.645Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
There is no repeatable way to measure whether a change to the agent directives (CLOVE.md, MCP instructions) makes agents use clove better. Add a harness under scripts/agent-eval/: a fixture generator for a small Python project, a set of scenario prompts (several separate tasks, one task with phases, a one-line edit, dependent tasks, an existing item to reuse, a request full of conversation context, three similar subcommands), a prompt builder that wraps a candidate directives file, and a scorer that reads the resulting .clove store. The scorer checks the item count per scenario, closed status, body length, dependency wiring, duplicate items, and conversation-context phrases in item text.
Done when: the scripts run against a built clove binary and a README explains how to run one evaluation round.