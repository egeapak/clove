---
schema: 1
id: clov-G03NVJC6
title: README and RELEASE_CHECKLIST drift from the workspace
status: closed
type: docs
priority: 2
created: 2026-11-14T19:12:34.272Z
updated: 2026-11-14T19:20:43.472Z
closed: 2026-11-14T19:20:43.472Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
README.md: the workspace layout table has no row for clove-engine, clove-service and clove-plugin-echo; `cargo build --release` is described as building the lean binaries, but it builds every workspace member. docs/RELEASE_CHECKLIST.md: §1 passes CLOVE_HOME to scripts/ci/daemon-smoke.sh, which overrides it and leaves a stray temp dir; §4 names the versions 0.1.5 and 0.2.0; §6 has no step that creates an item with the TUI new form.
Done when: the README table and build comment match the workspace, and the three checklist steps are corrected.