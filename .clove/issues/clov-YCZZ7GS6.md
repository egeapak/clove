---
schema: 1
id: clov-YCZZ7GS6
title: Rewrite agent directives (CLOVE.md) to state when and how to file items
status: closed
type: feature
priority: 1
created: 2026-10-31T06:46:18.295Z
updated: 2026-10-31T06:48:44.638Z
closed: 2026-10-31T06:48:44.638Z
labels: []
deps: [clov-WH5B0RKA]
relates: []
duplicates: []
supersedes: []
---
The CLOVE.md directives (repo root and CLOVE_MD_CONTENT in crates/clove/src/cmd/setup.rs) say to prefer clove for multi-step work but do not say how to split work into items or what an item must contain. Agents given the current text often file nothing when the tracker is empty, and the items they do file have no body.
The new text must state:
- a request with more than one task, or one task with several steps, is tracked; an empty tracker is not a reason to skip clove;
- one item per separate task, bug, or fix; the steps of one task are comments on that item, not separate items;
- a problem found outside the current task is a new item;
- every item has a self-contained body (what, where, how to verify), with no conversation context (who asked, deadlines, what was said earlier, how the problem was found) unless the work needs it;
- the lifecycle: find existing items, file, link with `dep add`, start, comment, close with a result comment.
Done when: CLOVE.md and the const are byte-identical (the existing drift test passes) and contain these rules.