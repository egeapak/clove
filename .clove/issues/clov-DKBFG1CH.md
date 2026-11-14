---
schema: 1
id: clov-DKBFG1CH
title: The version is repeated in files that no test checks
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:34.057Z
updated: 2026-11-14T19:19:12.814Z
closed: 2026-11-14T19:19:12.814Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The workspace version is repeated in crates/clove-web/web/package.json, package-lock.json, and the `version` of each internal crate under [workspace.dependencies]. Only .claude-plugin/plugin.json is pinned by a test. docs/RELEASE.md §8 says to bump only [workspace.package] version.
Done when: an xtask test fails when package.json, package-lock.json or an internal dependency version differs from the workspace version, and RELEASE.md §8 lists every place.