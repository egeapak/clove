---
schema: 1
id: clov-BKJ0A8QZ
title: Index list queries run a second COUNT pass when the fetched rows already give the total
status: closed
type: bug
priority: 2
created: 2026-11-14T19:25:01.272Z
updated: 2026-11-14T19:25:58.471Z
closed: 2026-11-14T19:25:58.471Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-index/src/query.rs query_filtered (no residue) always runs count_items and then query_list. In Ready and Blocked mode the WHERE clause holds correlated EXISTS subqueries over edges, so each pass evaluates them for every candidate row. When the window has no limit, or the query returns fewer rows than the pushed-down LIMIT, the row count of the list query is already the unpaginated total, and the COUNT pass repeats the whole evaluation for nothing. An unlimited clove ready or clove blocked therefore costs two full passes.
Expected: query_filtered runs the list query first and runs COUNT only when the page came back full.
Done when: a clove-index test shows equal totals for unlimited, short, full, and offset windows in List, Ready, and Blocked mode against count_items, and the helper that decides whether a count is needed is unit-tested.