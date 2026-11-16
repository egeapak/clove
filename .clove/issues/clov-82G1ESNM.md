---
schema: 1
id: clov-82G1ESNM
title: 'web: an open tab loops on reconnect after the server restarts with a new token'
status: closed
type: bug
priority: 2
created: 2026-11-15T07:07:35.611Z
updated: 2026-11-15T07:07:43.028Z
closed: 2026-11-15T07:07:43.028Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The web access token lasts as long as the server process. After a restart, an open tab gets 401 on the WebSocket upgrade and on every API call. crates/clove-web/web/src/lib/store.svelte.ts retried the socket forever (backoff up to 10 s) and showed 'offline'; api.ts read the plain-text 401 body as a bare 'HTTP 401'. Nothing told the user to open the new URL.
Expected: a 401 marks the page 'no access', stops the reconnect loop, and shows a banner that says to open the URL that clove serve prints (or clove daemon status shows).
Done when: src/test/token-expired.svelte.test.ts and the 401 tests in src/lib/api.test.ts pass, and a browser check against a restarted clove serve shows the banner.