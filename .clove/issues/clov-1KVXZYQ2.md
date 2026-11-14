---
schema: 1
id: clov-1KVXZYQ2
title: Bare-form id autolink matches ordinary words and numbers
status: open
type: bug
priority: 2
created: 2026-11-14T19:14:40.375Z
updated: 2026-11-14T19:14:40.375Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/micromark-clove-id.ts accepts the bare form '#' + 8 Crockford base32 characters in either case. With a repo id prefix known, '#feedback', '#database' and '#12345678' in a body or comment become links to /items/<prefix>-FEEDBACK and so on, which do not exist. The prefixed form ('#proj-7af3q2k9') does not have the problem.
Options: (1) require upper case for the bare form, the spelling the UI prints; lower-case words stop matching, '#12345678' and upper-case words such as '#DATABASE' still match. (2) Link a bare match only when the item is known to exist (needs a lookup at render time). (3) Keep the current rule.
Done when: the rule is chosen, implemented, and covered in src/lib/micromark-clove-id.test.ts.