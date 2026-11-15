---
schema: 1
id: clov-RNH3Y8H0
title: 'tui edit form: scalar fields are written before the typed parent and dep ids are validated'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:04.165Z
updated: 2026-11-14T19:07:37.482Z
closed: 2026-11-14T19:07:37.482Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/app/mod.rs submit_edit calls apply_edit and only then parses form.parent and form.deps. A changed title together with a malformed or unknown parent id shows an error, but the new title is already on disk.
Expected: the parent id and dep ids are parsed, and newly referenced items are checked to exist, before anything is written. A graph cycle can still fail after the scalar write.
Done when: a test submits a changed title with parent 'bogus id' and with an unknown id, gets an error, and the title on disk is unchanged.