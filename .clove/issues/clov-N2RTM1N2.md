---
schema: 1
id: clov-N2RTM1N2
title: 'cut v0.2.0: checklist, GitHub Release, then crates.io publish (AGPL relicense)'
status: closed
type: chore
priority: 1
created: 2026-10-20T16:36:55.000Z
updated: 2026-11-03T17:38:52.920Z
closed: 2026-10-21T10:08:29.000Z
labels: []
deps: [clov-G5XRHVP1, clov-HJPEE95J, clov-K0HASWKB, clov-P86FRMQ5, clov-YWZDKFMA]
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-105
---
First release under AGPL-3.0-only + plugin exception (SDK crates stay MIT OR Apache-2.0).

- [ ] Fixes merged: #76, #73, #72, #102, #83
- [ ] Quality gate green on master (fmt, clippy both arms, test --all-features, web check/test)
- [ ] CHANGELOG 0.2.0 dated; tag v0.2.0 → draft GitHub Release
- [ ] docs/RELEASE_CHECKLIST.md run against the draft Release's binaries
- [ ] Release archives carry LICENSE, LICENSE-PLUGIN-EXCEPTION, LICENSE-MIT, LICENSE-APACHE, THIRD_PARTY_LICENSES.txt
- [ ] Publish GitHub Release, then crates.io in docs/RELEASE.md order; each crate's license field + files verified on crates.io
- [ ] (optional) Homebrew tap formula, RELEASE.md §7 — carried over from #22