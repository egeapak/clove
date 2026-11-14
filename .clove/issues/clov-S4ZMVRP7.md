---
schema: 1
id: clov-S4ZMVRP7
title: An item file that starts with a UTF-8 BOM is rejected as MissingFrontmatter
status: closed
type: bug
priority: 2
created: 2026-11-14T19:11:39.437Z
updated: 2026-11-14T19:11:39.621Z
closed: 2026-11-14T19:11:39.621Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/parse.rs split_frontmatter requires the file to start with '---\n' or '---\r\n'. A file saved by a Windows editor with a leading EF BB BF fails with MissingFrontmatter, although CRLF from the same editors is accepted. The item is then a scan error, and dep add and set_parent fail for the whole store.
Expected: a leading UTF-8 BOM is skipped.
Done when: parse::tests::accepts_a_leading_utf8_bom passes.