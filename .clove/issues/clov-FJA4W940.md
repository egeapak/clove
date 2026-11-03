---
schema: 1
id: clov-FJA4W940
title: web UI polish from the v0.1.2 review
status: closed
type: feature
priority: 1
created: 2026-10-08T10:07:05.000Z
updated: 2026-11-03T17:38:52.945Z
closed: 2026-10-17T23:01:12.000Z
labels: [web]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-79
---
User review of the 0.1.2 web UI (2026-10-06):
1. List: the title column doesn't fill the table width (rows end ~mid-page, a gap before assignee/labels).
2. Detail: the page should expand to the full viewport height.
3. Detail relationships: show the related item's truncated title (with the id), not just the id.
4. Timeline: items (row labels and bars) should be clickable → detail.
5. Edit page: fill the full width.
6. Synced items show their external ids (e.g. GitHub #67, linking to the issue).
7. Board: the column-move arrow buttons need tooltips saying what pressing them does.
8. Show unsynced items (items with no external ref when the repo has a sync target).
9. Detail overview: properly parse and render the Markdown body.