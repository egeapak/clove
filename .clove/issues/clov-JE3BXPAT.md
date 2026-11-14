---
schema: 1
id: clov-JE3BXPAT
title: 'tui markdown: nested lists join the parent line and tables render on one line'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:04.125Z
updated: 2026-11-14T19:10:39.593Z
closed: 2026-11-14T19:10:39.593Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/markdown.rs: Tag::List pushes the list stack without ending the current line, so '- a\n  - nested b' renders as '• a  • nested b'. Only ENABLE_STRIKETHROUGH is set, so a GFM table renders as one paragraph line with its pipes. Link URLs are dropped, so a link whose text is not the URL cannot be followed.
Expected: a nested list starts on its own line, each table row is its own line, and a link shows its URL after the text when the two differ.
Done when: unit tests in markdown.rs cover a nested bullet list, a list nested in an ordered item, a table, and a link.