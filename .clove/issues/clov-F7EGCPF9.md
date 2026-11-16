---
schema: 1
id: clov-F7EGCPF9
title: clove setup writes through a symlinked .claude directory in project scope
status: closed
type: bug
priority: 2
created: 2026-11-15T07:01:15.723Z
updated: 2026-11-15T07:03:58.072Z
closed: 2026-11-15T07:03:58.072Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/setup.rs refuses symlinked files in project scope, but a .claude directory that is itself a symlink sends CLOVE.md, CLAUDE.md and settings.json outside the project. The refusal message of write_atomic also has a run of spaces from a missing line continuation.
Done when: project-scope setup (also --dry-run) fails on a symlinked .claude and writes nothing; crates/clove/tests/cli_commands.rs::setup_refuses_a_symlinked_claude_dir passes.