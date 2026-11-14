---
schema: 1
id: clov-JA6JBVTV
title: Id autolink matches the fragment of a URL that is not autolinked
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:40.331Z
updated: 2026-11-14T19:14:40.476Z
closed: 2026-11-14T19:14:40.476Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/micromark-clove-id.ts links '#AS5Y8MM7' in 'example.com/#AS5Y8MM7'. GFM does not autolink a bare domain without www or a protocol, so the text is plain and the id construct sees '#' after '/', which passes its left-boundary check.
Expected: '#' directly after '/' does not start an id.
Done when: 'does NOT link the fragment of a URL that is not autolinked' in src/lib/micromark-clove-id.test.ts passes.