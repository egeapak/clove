---
schema: 1
id: clov-J3GQVWZQ
title: clove edit --body and --body-file set the body without an editor
status: closed
type: feature
priority: 2
created: 2026-11-14T19:17:43.240Z
updated: 2026-11-14T19:44:49.994Z
closed: 2026-11-14T19:44:49.994Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The CLI has no non-interactive way to replace an item body: 'clove set <id> body=hello' fails with 'unknown editable field' (EditRequest::from_tokens has no body key) and 'clove edit' without --field opens $EDITOR. MCP clove_edit, the web PATCH and the TUI form can all set the body through EditRequest.body.
Expected: 'clove edit <id> --body TEXT' and 'clove edit <id> --body-file FILE' ('-' reads stdin) replace the body through clove_core::apply_edit, and combine with --field in one write.
Done when: crates/clove/tests/cli_regressions.rs edit_sets_the_body_from_a_flag_or_a_file passes.