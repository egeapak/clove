---
schema: 1
id: clov-BBJF9SA7
title: Item reads by path follow symlinks, and the frontmatter scan reads whole files
status: closed
type: bug
priority: 2
created: 2026-11-14T19:16:03.186Z
updated: 2026-11-14T19:16:03.256Z
closed: 2026-11-14T19:16:03.256Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/parse.rs parse_item_file opens the path with File::open and parse_frontmatter_file with fs::metadata + fs::read. Both follow a symlink, although the store scan skips symlinks and non-regular files, so ItemStore::get, show, web and MCP reads of a planted link to a device or FIFO block or never end. parse_frontmatter_file also reads the whole file and then drops the body, on every ls, ready, blocked, and whole-store scan; DESIGN.md 13.3 says bodies are not read for those.
Expected: both functions refuse a symlink and anything that is not a regular file, without blocking; parse_frontmatter_file reads only as far as a frontmatter within budget can reach and returns the same verdict as the full parse for every file.
Done when: parse tests refuses_a_symlinked_item_file and frontmatter_parse_agrees_with_the_full_parse_on_large_files pass.