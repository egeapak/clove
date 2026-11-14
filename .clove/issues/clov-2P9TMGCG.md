---
schema: 1
id: clov-2P9TMGCG
title: Titles with trailing line breaks and labels with Unicode line separators do not round-trip
status: closed
type: bug
priority: 2
created: 2026-11-14T19:07:47.644Z
updated: 2026-11-14T19:07:47.932Z
closed: 2026-11-14T19:07:47.932Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/write.rs render_scalar trims the trailing newline of the serde_norway rendering, so the title 'foo\n\n' is written as a clip block scalar and reads back as 'foo\n'. force_double_quoted writes U+2028, U+2029, U+0085 and C0 controls raw inside a double-quoted flow scalar; YAML folds them, so the label 'a<U+2028>,b' reads back as 'a ,b'.
Expected: every title, assignee, and label reads back byte for byte.
Done when: write::tests::unusual_scalars_read_back_unchanged passes.