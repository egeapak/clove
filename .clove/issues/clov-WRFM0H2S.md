---
schema: 1
id: clov-WRFM0H2S
title: Make the github feature opt-out to shrink the release binary
status: closed
type: chore
priority: 2
created: 2026-07-18T00:14:17.000Z
updated: 2026-11-03T17:38:52.993Z
closed: 2026-07-18T11:59:52.000Z
labels: [build, release]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-18
---
The github feature (octocrab + tokio) is ~3.5 MB of the 11.35 MB binary. Make it opt-out so the default distribution binary is leaner; keep sync github behind the feature.