---
schema: 1
id: clov-G4SNS5XG
title: Human CLI output prints terminal control sequences from item text
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:04:50.851Z
updated: 2026-11-14T19:04:50.906Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/show.rs print_human, cmd/listing.rs emit (Human arm), cmd/comments.rs and cmd/stats.rs print titles, assignees, labels, bodies, comment authors and comment bodies raw. parse_title (clove-types fields.rs) accepts control characters, and item files and synced GitHub issues are untrusted, so a title containing ESC/CR/OSC sequences reaches the terminal: it can overwrite or hide lines of 'clove ls' and set the terminal title or clipboard.
Expected: human output drops C0/C1 controls, DEL, bidi controls and invisible separators (newline and tab kept in bodies); JSON output is unchanged.
Done when: crates/clove/tests/cli_regressions.rs human_output_strips_terminal_control_sequences and the util.rs term_line/term_text unit tests pass.