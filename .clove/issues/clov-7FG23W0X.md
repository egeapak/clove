---
schema: 1
id: clov-7FG23W0X
title: 'Web write endpoints differ from CLI and MCP: comment author, empty PATCH, force flag'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.597Z
updated: 2026-11-14T19:30:06.520Z
closed: 2026-11-14T19:30:06.520Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/src/write.rs: (1) add_comment attributes a comment with no author to the literal "web"; CLI and MCP use clove_core::comments::comment_author(repo_root). (2) PATCH /items/:id with an empty object or only unknown keys (for example {"parent":"..."}) returns 200 and the unchanged item; MCP answers 'no fields to edit' and the CLI exits 4. (3) DELETE /items/:id reads ?force=1 as false; every other boolean parameter goes through bool_param.
Expected: no author -> comment_author; a client-supplied author is kept. An empty or unknown-key PATCH is a 400/422 error. force goes through bool_param.
Done when: crates/clove-web/tests/api.rs tests comment_without_author_uses_the_shared_author_rule, patch_rejects_empty_and_unknown_fields, delete_force_accepts_the_shared_boolean_spellings pass.