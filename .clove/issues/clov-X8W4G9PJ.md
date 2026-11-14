---
schema: 1
id: clov-X8W4G9PJ
title: 'cloved: daemon-side create keeps the id prefix and default type from the time the project was loaded'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:43.206Z
updated: 2026-11-14T19:33:51.263Z
closed: 2026-11-14T19:33:51.263Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/slot.rs reads .clove/config.toml once, at load. After an edit of id_prefix or default_type, creates through the daemon (MCP, plugins) keep the old values until the project is unloaded.
Done when: crates/cloved/tests/daemon_watch.rs::a_create_uses_the_config_as_it_is_now passes.