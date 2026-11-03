---
schema: 1
id: clov-ZPXRBCVY
title: Comment author falls back to 'unknown' when CLOVE_AUTHOR unset
status: closed
type: chore
priority: 3
created: 2026-07-18T00:14:22.000Z
updated: 2026-11-03T17:38:52.983Z
closed: 2026-07-18T11:58:48.000Z
labels: [dx, ux]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-26
---
Found while dogfooding: clove comment writes author 'unknown' when CLOVE_AUTHOR is not set, even though git config user.name ('Ege Apak') and $USER ('egeapak') are available. It should fall back to git config user.name (or $USER) before 'unknown'. Repro: unset CLOVE_AUTHOR; clove comment <id> 'hi'; the comment file is ...-unknown-....  Setting CLOVE_AUTHOR=egeapak produces ...-egeapak-... correctly.