---
schema: 1
id: clov-EMVTVY63
title: Port pack-web-dist.sh and check-dco.sh to cargo xtask
status: closed
type: chore
priority: 2
created: 2026-11-15T06:50:23.495Z
updated: 2026-11-15T07:06:40.026Z
closed: 2026-11-15T07:06:40.026Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
scripts/release/pack-web-dist.sh (release.yml) and scripts/ci/check-dco.sh (dco.yml) are shell. Build and CI tooling goes in xtask (CLAUDE.md). The smoke scripts (scripts/ci/daemon-smoke.sh, scripts/release/release-smoke.sh) run against packaged binaries on every platform, including by hand from RELEASE_CHECKLIST.md.
Done when: cargo xtask pack-web-dist and cargo xtask check-dco do what the scripts did, the workflows and docs call them, the scripts are removed, and xtask tests cover the archive layout and the sign-off check.