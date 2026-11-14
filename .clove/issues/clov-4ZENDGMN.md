---
schema: 1
id: clov-4ZENDGMN
title: release-smoke.sh runs in the caller's directory when its scratch setup fails
status: closed
type: bug
priority: 2
created: 2026-11-14T19:12:33.916Z
updated: 2026-11-14T19:20:43.288Z
closed: 2026-11-14T19:20:43.288Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
scripts/release/release-smoke.sh has no `set -e` and does not check `mktemp` or `mkdir && cd`. When either fails, the following `clove init`, `new`, `close`, `import` and `daemon stop --all` run in the caller's working directory and against the caller's hub. The check `! curl ... | grep -qi placeholder` passes when curl fails.
Done when: a failed scratch setup stops the script before any clove command, and the placeholder check fails when the page cannot be fetched.