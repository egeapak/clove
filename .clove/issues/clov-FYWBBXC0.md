---
schema: 1
id: clov-FYWBBXC0
title: Alias guard rejects plain scalars that contain a bracket before * or &
status: closed
type: bug
priority: 2
created: 2026-11-14T19:02:50.188Z
updated: 2026-11-14T19:07:47.424Z
closed: 2026-11-14T19:07:47.424Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/parse.rs contains_yaml_anchor_or_alias counts every '[' and '{' as a flow-collection opener, also inside a plain block scalar. The writer emits 'title: Deref [*ptr] crashes' as a plain scalar (valid YAML), and the next read fails with AliasNotAllowed. The item then cannot be listed, edited, or closed, and scan_or_fail makes dep add and set_parent fail for the whole store. An unbalanced '[' in a title also leaves the flow depth above zero for later lines.
Expected: '[' and '{' open a flow collection only at a node position; a title, assignee, or external_ref with '[*x]', '{&x}', or '[a, *b]' round-trips. write_item_file refuses to write bytes that its own parser rejects.
Done when: the parse.rs tests for these titles pass and ops::create followed by store.get returns the item.