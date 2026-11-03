---
schema: 1
id: clov-FZZQ1HF4
title: plugin_install tests pick up clove plugins installed in ~/.cargo/bin
status: closed
type: bug
priority: 3
created: 2026-10-08T10:07:07.000Z
updated: 2026-11-03T17:38:52.941Z
closed: 2026-10-18T13:36:10.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-82
---
With clove-import-tk/-beads/clove-sync-github installed in ~/.cargo/bin (on PATH), plugin_install::the_suite_sees_no_plugin_it_did_not_install and ::package_selects_one_plugin_from_a_multi_plugin_repo fail (plugin list count 3, expected 0; 'would shadow ~/.cargo/bin/clove-import-tk'). The tests don't isolate the $PATH tier of the plugin search. Make them hermetic (scrub PATH to a temp dir). Found 2026-10-06.