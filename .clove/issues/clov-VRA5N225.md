---
schema: 1
id: clov-VRA5N225
title: Web list shows a backend error for an invalid value in the URL
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:46.571Z
updated: 2026-11-14T19:08:46.821Z
closed: 2026-11-14T19:08:46.821Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/query.ts parseQuery passes status, sort, dir, type and priority from the browser URL through unvalidated (priority=abc becomes NaN). The server rejects an unknown value with a validation error, and crates/clove-web/web/src/routes/list/+page.svelte shows it as 'Couldn't reach the backend' with the table and its sort headers hidden. Retry repeats the error; the only recovery from /list?sort=bogus is editing the URL.
Expected: parseQuery drops a value the server would reject.
Done when: 'values the server would reject are dropped from a browser URL' in src/lib/query.test.ts passes.