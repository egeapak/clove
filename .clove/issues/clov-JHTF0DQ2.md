---
schema: 1
id: clov-JHTF0DQ2
title: Published clove-web build verifies the SPA against a checksum from the same release
status: closed
type: feature
priority: 2
created: 2026-11-14T19:04:18.740Z
updated: 2026-11-14T19:30:46.278Z
closed: 2026-11-14T19:30:46.278Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/build.rs acquire_packaged downloads the SPA tarball and its .sha256 from the same GitHub Release URL (build/fetch.rs base_url). The checksum detects corruption but not a replaced asset: no digest is pinned in the published crate.
Options: (a) write the expected SHA-256 into the packaged crate at release time (a generated file that build.rs reads) and add the step to docs/RELEASE.md and the release workflow; (b) sign the tarball and verify the signature; (c) keep the current check.
Done when: a build of the published crate fails when the release tarball differs from the digest recorded in the crate.