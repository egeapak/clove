---
schema: 1
id: clov-PDXMJTX0
title: Web nav highlights a tab when the project path contains its name
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:54.621Z
updated: 2026-11-14T19:18:02.692Z
closed: 2026-11-14T19:18:02.692Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/+layout.svelte isActive() tests the pathname with a substring match, so a base path such as /list-tools/ lights the List tab on every page.
Expected: the tab matches the first path segment after the base path; the app root is the board.
Done when: src/lib/urls.test.ts isActiveTab tests pass.