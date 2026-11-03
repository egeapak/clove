---
schema: 1
id: clov-ZSM0NJ2E
title: Print the item title in clove new and clove comment output
status: closed
type: feature
priority: 2
created: 2026-10-31T10:01:43.163Z
updated: 2026-10-31T10:12:21.342Z
closed: 2026-10-31T10:12:21.342Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
`clove new` prints '<id>  <path>' and its JSON data is {id, path}; `clove comment` prints 'added comment to <id>'. MCP clove_new returns the same {id, path}. So an agent that has just filed or commented on an item does not have the title at hand when it reports to the user. (`clove start`, `close`, and `status` already print the title.)
Expected: the human output of new and comment includes the title; the JSON data of new and comment, and the clove_new / clove_comment MCP results, gain a 'title' key. Adding a key keeps existing JSON readers working.
Done when: CLI tests assert the title in both outputs, the pinned MCP snapshot is updated, and the JSON Schemas under docs/json-schema/v1/ match if they describe these responses.