---
schema: 1
id: clov-3JP6104B
title: CLI accepts conflicting or empty arguments and ignores them
status: closed
type: bug
priority: 2
created: 2026-11-14T19:25:57.420Z
updated: 2026-11-14T19:44:50.090Z
closed: 2026-11-14T19:44:50.090Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Several commands accept input they then ignore. crates/clove/src/cmd/stats.rs passes an unparseable --since to snapshot_history, which drops it and returns the whole history ('stats --history --since yesterday'). 'stats --history' ignores --snapshot, --top and --no-epics. cmd/assign.rs ignores the name in 'assign <id> bob --clear'. cmd/comments.rs stores an empty comment for 'comment <id> ""'. 'agent-doc --file F' without --check ignores --file. cmd/init.rs rewrites .clove/.gitignore on every run and drops lines a user added.
Expected: an unparseable --since and an empty comment are validation errors (exit 4); the conflicting flag pairs are usage errors (exit 1); init keeps existing .gitignore lines and appends only missing entries.
Done when: crates/clove/tests/cli_regressions.rs conflicting_or_empty_arguments_are_refused and init_rerun_keeps_user_gitignore_lines pass.