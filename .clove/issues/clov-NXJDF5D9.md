---
schema: 1
id: clov-NXJDF5D9
title: 'clove show: -v does nothing and --fields is ignored in human output'
status: closed
type: bug
priority: 2
created: 2026-11-15T07:01:15.689Z
updated: 2026-11-15T07:03:58.071Z
closed: 2026-11-15T07:03:58.071Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/show.rs computes ready/blocked_by always, so -v/--verbose changes nothing. In human format the --fields projection is computed but print_human prints the fixed view.
Done when: human show --fields prints only the requested fields (name: value lines, in the requested order), -v adds blocked_by, parent and other links, external ref and timestamps, and crates/clove/tests/cli_commands.rs::show_human_honours_fields_and_verbose passes.