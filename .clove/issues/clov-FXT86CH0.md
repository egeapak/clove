---
schema: 1
id: clov-FXT86CH0
title: 'mcp: clove_comment author falls back to ''unknown'' (CLI''s git/USER fallback not shared)'
status: closed
type: bug
priority: 3
created: 2026-10-06T18:18:22.000Z
updated: 2026-11-03T17:38:52.950Z
closed: 2026-10-17T23:01:18.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-74
---
`crates/clove-mcp/src/engine.rs` (~L437) resolves the comment author as CLOVE_AUTHOR → GIT_AUTHOR_EMAIL → 'unknown', while the CLI (`cmd/comments.rs::resolve_author`, the gh-26 fix) also tries git config user.email/name and $USER. A `clove_comment` via MCP with CLOVE_AUTHOR unset is written as `…-unknown-….md` even though git config and USER are set; the CLI writes `egeapak-gmail-com`. Two copies of the same rule diverged — move resolve_author into clove-core and use it from both. Found dogfooding v0.1.1 (2026-10-06); predates 0.1.1.