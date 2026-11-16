---
schema: 1
id: clov-B4PV3DVG
title: Test suites share one clove home under target/test-clove-home
status: closed
type: chore
priority: 2
created: 2026-11-15T06:50:23.407Z
updated: 2026-11-15T07:06:02.906Z
closed: 2026-11-15T07:06:02.906Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/tests/{mcp,daemon_cli,daemon_routing,sort_order,filter_parity,web_port}.rs set CLOVE_HOME to <repo>/target/test-clove-home, one directory that all tests share and that ignores CARGO_TARGET_DIR. The unit tests in clove-core daemon_token.rs, clove-ipc client.rs and cloved hub.rs, and crates/cloved/tests/support/mod.rs, use the same path for token records.
Done when: each clove-cli suite uses a clove home inside the test's own temp dir (support::clove_home), the unit and cloved tests keep records inside the build's own target dir, and no source file names target/test-clove-home.