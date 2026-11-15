---
schema: 1
id: clov-4H6NPFSN
title: CI lean test arm runs the suites against an all-features cloved
status: closed
type: chore
priority: 2
created: 2026-11-15T06:50:23.443Z
updated: 2026-11-15T07:07:08.726Z
closed: 2026-11-15T07:07:08.726Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The default-features test arm in .github/workflows/ci.yml cannot test a lean cloved: the daemon suites need the cloved test knobs (test-knobs feature), so cargo xtask test-bins builds cloved with --all-features. A lean cloved build is never exercised by any test.
Done when: CI builds and starts a default-features cloved in at least one check (for example the daemon smoke), or the limit is documented where the arm is defined.