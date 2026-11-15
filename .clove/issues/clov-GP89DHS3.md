---
schema: 1
id: clov-GP89DHS3
title: Workflows give a write token to build jobs and use movable action tags
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:33.519Z
updated: 2026-11-14T19:17:23.801Z
closed: 2026-11-14T19:17:23.801Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
.github/workflows/release.yml sets `permissions: contents: write` for the whole workflow, including the build and npm jobs. ci.yml has no permissions block. Third-party actions (dtolnay/rust-toolchain, Swatinem/rust-cache, taiki-e/install-action, EmbarkStudios/cargo-deny-action, softprops/action-gh-release) are referenced by tags or branches that can move.
Done when: ci.yml has `permissions: contents: read`, only the release job of release.yml has `contents: write`, and each third-party action is pinned to a commit SHA.