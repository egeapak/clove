---
schema: 1
id: clov-V0T0Y4S1
title: clove-web build.rs does not rerun when web/dist changes
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.632Z
updated: 2026-11-14T19:30:30.466Z
closed: 2026-11-14T19:30:30.466Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/build.rs emits rerun-if-changed for the web sources but not for web/dist. After a first cargo build without npm embeds the placeholder, a manual npm run build in crates/clove-web/web followed by cargo build still embeds the placeholder. After a failed npm run build with an older dist present, the warning says the placeholder is embedded although the old dist is.
Expected: cargo reruns the script when dist changes, and the warning names what is embedded.
Done when: build.rs prints cargo:rerun-if-changed for web/dist and the warning text matches the embedded source.