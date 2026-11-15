---
schema: 1
id: clov-34P0YRJ3
title: 'sync github: every sync lists all issues of the repository and scans the store several times'
status: open
type: chore
priority: 2
created: 2026-11-14T19:15:06.150Z
updated: 2026-11-14T19:15:06.150Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-import/src/sync_net.rs sync_github fetches every issue of the repository (state=all, 100 per page) on each run, and scans the store in local_objects, apply_plan (scan_frontmatter), linked_issues (once per comment plan) and record_synced_assignees. Cost grows with the repository size, not with the changes.
A since=<last sync> issue listing cannot be used as is: remote_missing detection, the lost-link pairing and the comment-count idle check all need the full list.
Done when: a design is chosen (for example a full listing every N runs with since-listings between, and one shared scan per run) and a test with the mock server counts the list pages and store scans of an idle sync.