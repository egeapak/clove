---
schema: 1
id: clov-TM3NWMV3
title: 'clove-ipc: daemon answers above 8 MiB are dropped at the frame limit'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.947Z
updated: 2026-11-14T19:33:51.298Z
closed: 2026-11-14T19:33:51.298Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-ipc/src/transport.rs builds LengthDelimitedCodec with its default max frame length (8 MiB). A larger answer (ls --limit 0 on a store of about 70k items, or a show/apply_edit of an item with a multi-megabyte body after JSON escaping) fails to send; the client treats the error as no daemon and the next tier computes the answer again.
Done when: both ends use an explicit frame limit sized for the 4 MiB body cap, and crates/clove-ipc transport::tests::a_frame_above_the_codec_default_round_trips passes.