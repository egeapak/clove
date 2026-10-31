---
schema: 1
id: clov-WH5B0RKA
title: CLOVE.md directives name a nonexistent `clove list` command
status: closed
type: bug
priority: 1
created: 2026-10-31T06:46:18.265Z
updated: 2026-10-31T06:48:44.611Z
closed: 2026-10-31T06:48:44.611Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
CLOVE.md and the CLOVE_MD_CONTENT const in crates/clove/src/cmd/setup.rs tell agents to run `clove list`. The CLI has no `list` subcommand (it is `clove ls`), so the command fails with 'unknown subcommand'. An agent that hits this error often stops using clove for the rest of the task.
Done when: the directives name `clove ls`, and a test fails if the directives name a subcommand that the CLI does not define.