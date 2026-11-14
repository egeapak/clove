---
schema: 1
id: clov-Y4Q2JN41
title: DESIGN.md, CLAUDE.md and agent-doc describe commands, flags and envelope keys that do not exist
status: closed
type: docs
priority: 2
created: 2026-11-14T19:25:57.690Z
updated: 2026-11-14T19:44:50.246Z
closed: 2026-11-14T19:44:50.246Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
docs/DESIGN.md section 7.2 lists 'init --no-gitignore', 'new -t', 'new -d', 'reindex --force', 'migrate', 'completions' and 'agent-doc --format markdown', none of which clap accepts, and omits 'title' from the --field keys. Sections 6.7 and 7.3 show _meta.took_ms and _meta.stale_index, which are never emitted; the 'clove show' example in 7.4 shows dangling_deps, warnings and children_summary, which show does not return; exit code 6 (index error) is documented in 7.6 and in cmd/agent_doc.rs but never returned (index failures are exit 5). CLAUDE.md and agent-doc say 'import github' / 'export github' were removed; they exist as one-way views served by clove-sync-github. Without the plugin, 'clove import github o/r' says to run 'clove plugin install import-github', a crate that does not exist.
Expected: the docs match the binary, and the missing-provider hint names every plugin that can serve the request (sync-github for import/export github).
Done when: the listed lines are corrected and crates/clove/tests/cli_regressions.rs missing_import_provider_hint_names_the_sync_plugin passes.