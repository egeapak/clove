---
schema: 1
id: clov-P0D5RQRS
title: 'CLOVE.md: show examples of conversation context that items must not contain'
status: open
type: docs
priority: 2
created: 2026-10-31T08:27:11.563Z
updated: 2026-10-31T08:27:11.563Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
With the current CLOVE.md, Claude Haiku still writes conversation context into items in 5 of 51 agent-eval runs (scenarios s5, s10, s14, s15), while Sonnet and Opus do so in 0 of 51 each. The leaked phrases are 'next week', 'per the request', and 'Found while testing'. CLOVE.md describes the rule in prose only.
Expected: CLOVE.md shows one or two short wrong/right pairs (for example, wrong: 'Found while testing; out of scope.' right: no such sentence; wrong: 'Deferred to next week.' right: leave the item open with no date) so that small models apply the rule.
Done when: CLOVE.md and CLOVE_MD_CONTENT carry the examples, the drift test passes, and a 3-run Haiku round of scripts/agent-eval on s5, s10, s14, and s15 has no no_leak failure.