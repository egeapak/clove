---
schema: 1
id: clov-MKTVAKGE
title: 'web: each page load fetches its rows twice and /meta three times'
status: open
type: chore
priority: 2
created: 2026-11-15T07:11:47.568Z
updated: 2026-11-15T07:11:47.568Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
On every full page load of the web UI (crates/clove-web/web), the route's setQuery loads items + meta, startLive's ensureMeta loads meta again (store.meta is not set yet), and the WebSocket onopen in src/lib/store.svelte.ts calls resync(), which loads items + meta a second time. Seen in Chromium against clove serve: /list issues GET /api/v1/items?...&limit=100 twice and GET /api/v1/meta three times.
The onopen resync closes a real gap: a write between the route's load and the socket opening is otherwise missed. Removing it needs a sequence number in the list response (compare it with the hello frame's seq) so the page can skip the resync when nothing changed.
Done when: a page load issues one items request and one meta request when no write happens in between, and a write in that gap still shows.