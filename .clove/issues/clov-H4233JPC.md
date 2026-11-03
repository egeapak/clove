---
schema: 1
id: clov-H4233JPC
title: 'clove serve / cloved: eprintln! panics when stderr is closed (e.g. clove serve 2>&1 | head -1)'
status: closed
type: bug
priority: 4
created: 2026-10-21T07:24:40.000Z
updated: 2026-11-03T17:38:52.907Z
closed: 2026-10-21T07:24:41.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-116
---
Status lines after the URL ("press Ctrl-C to stop", watcher notes) use eprintln!, which panics on EPIPE, killing the server once its stderr reader goes away. Found via the web_port flake on PR #106. Use writeln!(std::io::stderr(), …) and ignore write errors for non-essential status output.