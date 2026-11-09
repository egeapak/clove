---
schema: 1
id: clov-ATBHHDKQ
title: 'Release clove 0.3.0: bump versions and CHANGELOG'
status: closed
type: chore
priority: 2
created: 2026-11-09T07:28:17.124Z
updated: 2026-11-09T07:28:17.194Z
closed: 2026-11-09T07:28:17.194Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Bump [workspace.package] version and the internal dep versions in Cargo.toml, Cargo.lock, .claude-plugin/plugin.json, crates/clove-web/web/package.json and package-lock.json from 0.2.0 to 0.3.0. Move the [Unreleased] CHANGELOG entries to a dated 0.3.0 section and add entries for the merged changes that had none. Update the version strings in docs/RELEASE.md.
Done when: the quality gate passes and xtask/tests/claude_plugin.rs passes.