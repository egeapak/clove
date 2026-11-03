---
schema: 1
id: clov-G5XRHVP1
title: 'clove edit: $EDITOR with arguments (e.g. ''code --wait'') fails with a misleading ''No such file'' on the item'
status: closed
type: bug
priority: 2
created: 2026-10-06T18:18:21.000Z
updated: 2026-11-03T17:38:52.952Z
closed: 2026-10-21T07:24:46.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-73
---
`cmd/edit.rs::open_in_editor` runs `Command::new($EDITOR).arg(path)`, so an EDITOR/VISUAL/CLOVE_EDITOR value with arguments — `code --wait`, `subl -w`, `emacsclient -t`, very common — is exec'd as one program name, fails with ENOENT, and the error is reported as `io error at .clove/issues/<id>.md: No such file or directory` (the item file, which exists and is untouched). Fix: run it the way git does (`sh -c '$EDITOR "$@"' editor <path>` on Unix; split on Windows) and name the editor in the spawn error. Found dogfooding v0.1.1 (2026-10-06); predates 0.1.1.