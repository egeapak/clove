---
schema: 1
id: clov-RFDC9211
title: sync github pull sets created to the pull time instead of the issue's created_at
status: closed
type: bug
priority: 2
created: 2026-11-03T17:28:04.157Z
updated: 2026-11-03T17:42:40.993Z
closed: 2026-11-03T17:42:40.993Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-169
---
crates/clove-import/src/sync_net.rs pull_create() sets created and updated to Utc::now(). For a closed issue, closed (from closed_at) is then before created, and clove doctor warns TIMESTAMP_INCOHERENT (62 warnings after pulling egeapak/clove). GitHubIssue does not carry created_at.
Expected: created comes from the issue's created_at (falling back to now when absent).
Done when: GitHubIssue/StagedIssue carry created_at, a sync_github.rs mock test asserts the pulled created equals the issue's created_at, and clove doctor shows no TIMESTAMP_INCOHERENT for pulled items.