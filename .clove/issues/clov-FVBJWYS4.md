---
schema: 1
id: clov-FVBJWYS4
title: Item writes do not enforce the frontmatter and body size limits
status: closed
type: bug
priority: 2
created: 2026-11-14T19:07:47.568Z
updated: 2026-11-14T19:07:47.912Z
closed: 2026-11-14T19:07:47.912Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/write.rs write_item_file writes any item that passes validate_item. A body above MAX_BODY_BYTES (4 MiB), a title above MAX_FRONTMATTER_BYTES (64 KiB), or about 9,000 labels is written, and the next read fails with BodyTooLarge or FrontmatterTooLarge. The item is then unreadable, and scan_or_fail makes dep add and set_parent fail for the whole store.
Expected: write_item_file returns the same error the reader would return and leaves no file.
Done when: write::tests::write_refuses_what_the_reader_rejects passes.