---
schema: 1
id: clov-KENXY6TK
title: 'tui edit form: saving overwrites fields changed on disk since the form opened'
status: closed
type: bug
priority: 1
created: 2026-11-14T19:03:04.044Z
updated: 2026-11-14T19:07:02.363Z
closed: 2026-11-14T19:07:02.363Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/app/form.rs to_edit_request sends every field (title, body, status, priority, type, assignee, labels as LabelEdit::Set) as Some, with the values loaded when the form opened. submit_edit (app/mod.rs) applies them all, so a field another process changed while the form was open is reverted: close the item elsewhere, change the title in the form, Ctrl-S, and the item is open again with closed unset. Ctrl-S with no change also rewrites the file and bumps updated.
Expected: the EditRequest carries only the fields that differ from the item the form was opened with (labels as a Delta), and a submit with no change writes nothing.
Done when: a test closes an item while its edit form is open, saves a title change, and the item stays closed; and a no-op save leaves updated unchanged.