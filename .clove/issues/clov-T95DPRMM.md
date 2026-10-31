---
schema: 1
id: clov-T95DPRMM
title: Agents drop the title when they mention an item id a second time
status: closed
type: chore
priority: 2
created: 2026-10-31T10:24:29.735Z
updated: 2026-10-31T11:23:31.486Z
closed: 2026-10-31T11:23:31.486Z
assignee: claude
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
With the CLOVE.md rule 'name items by id and title when you talk to the user', agent replies sometimes give an id with its title on the first mention and bare on a later mention in the same reply (scripts/agent-eval round 2: haiku s9, opus s13). Haiku also lists several ids bare in one run (s8) and shortens titles (s15).
Possible fix: say in CLOVE.md, the MCP instructions and agent-doc that the title goes with every mention, including a repeated one, and that the title is copied exactly.
Done when: a new 153-run round (scripts/agent-eval) shows no id_titles miss from a repeated mention, and no scenario mean drops.