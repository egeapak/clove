---
schema: 1
id: clov-0V6DDB95
title: 'web: store.test.ts waits on fixed sleeps'
status: closed
type: chore
priority: 2
created: 2026-11-15T06:52:13.748Z
updated: 2026-11-15T06:52:13.816Z
closed: 2026-11-15T06:52:13.816Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/store.test.ts waits 30-150 ms with setTimeout for the store's loads to settle, and the superseded-response test orders two fetches by a 60 ms delay. On a slow runner a sleep can end before the load, so the test can pass or fail by timing.
Expected: the tests await the real load promises (and order fetches with a held promise), with no setTimeout.
Done when: grep setTimeout in store.test.ts finds nothing and npm run check && npm run test pass.