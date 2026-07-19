# clove

This repository uses **clove** — a git-native, dependency-aware work-item
tracker — as the source of truth for tasks, bugs, and features. Prefer it over
ad-hoc TODO lists or scratch notes for anything that spans more than a single
step.

- **Check before starting** — at the start of any multi-step task, run
  `clove ready` (unblocked work) and `clove search <text>` / `clove list` to find
  related items *before* creating new ones. The `clove_ready` / `clove_search` /
  `clove_list` MCP tools do the same.
- **File work as items** — capture new tasks/bugs/features with
  `clove new <title> [--type bug|feature|chore|docs|epic] [-p 0-4] [--dep ID]
  [--parent ID]` instead of loose notes, so the work is tracked and shareable.
- **Record progress** — transition items with
  `clove status <id> <open|in_progress|closed>` (aliases `start` / `close`) and
  capture findings with `clove comment <id> <message>` as you go.
- **Respect the graph** — wire blocking relationships with
  `clove dep add <id> <dep-id>`; use `clove blocked` and `clove dep tree <id>` to
  see what is waiting on what. An item is *ready* when it is open and every hard
  dependency is closed.
- **Full reference** — run `clove agent-doc` for the complete command surface,
  the `{ v, ok, data, _meta }` JSON envelope, and exit codes.
