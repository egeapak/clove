---
schema: 1
id: clov-QSDHS97S
title: The Linux release binary requires the glibc of ubuntu-latest
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:34.468Z
updated: 2026-11-14T19:34:48.687Z
closed: 2026-11-14T19:34:48.687Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
.github/workflows/release.yml builds x86_64-unknown-linux-gnu on ubuntu-latest, so the binary links against that image's glibc and does not start on a distribution with an older glibc. docs/RELEASE_CHECKLIST.md smokes only debian:stable-slim.
Done when: the Linux binary is built against a stated glibc floor (an older runner image, cargo-zigbuild with a glibc version, or a musl target), and the checklist smokes a distribution at that floor.