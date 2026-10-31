---
schema: 1
id: clov-FTTS16EQ
title: 'CLOVE.md: run the check before writing the closing comment'
status: in_progress
type: docs
priority: 3
created: 2026-10-31T08:27:11.589Z
updated: 2026-10-31T10:48:34.810Z
assignee: claude
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
CLOVE.md says: before 'clove close', add a comment that says what changed and how you verified it. In agent-eval runs, Claude Sonnet sometimes wrote 'tests pass' in that comment before it ran the tests, then found two failing tests and added a correction comment (seen in s11, s13, and s15). The final item text is then contradictory.
Expected: the rule says to run the check first and to quote its actual result (for example, which tests pass and which fail) in the closing comment.
Done when: CLOVE.md and CLOVE_MD_CONTENT carry the rule, the drift test passes, and a 3-run Sonnet round on s11, s13, and s15 produces no correction comments.