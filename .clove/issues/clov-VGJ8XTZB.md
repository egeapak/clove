---
schema: 1
id: clov-VGJ8XTZB
title: An id inside a markdown link label renders nested anchors
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:40.276Z
updated: 2026-11-14T19:14:40.459Z
closed: 2026-11-14T19:14:40.459Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/micromark-clove-id.ts matches an id inside the label of a markdown link. '[fix for #clov-AS5Y8MM7](https://example.com)' renders <a href="https://example.com">fix for <a href="/items/clov-AS5Y8MM7">#clov-AS5Y8MM7</a></a>. Nested anchors are invalid HTML; the browser splits the outer link, so part of its text loses the target. The file's header comment says this cannot happen.
Expected: an id inside a link label is plain text of that link.
Done when: 'does NOT nest a link inside the label of a markdown link' in src/lib/micromark-clove-id.test.ts passes.