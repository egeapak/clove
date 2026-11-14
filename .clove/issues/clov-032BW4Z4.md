---
schema: 1
id: clov-032BW4Z4
title: 'clove-ipc: the fallback runtime directory /tmp/clove-<uid> can be created first by another local user'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:43.230Z
updated: 2026-11-14T19:33:51.358Z
closed: 2026-11-14T19:33:51.358Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-ipc/src/lib.rs resolve_runtime_dir falls back to $TMPDIR or /tmp + clove-<uid> when neither CLOVE_RUNTIME_DIR nor XDG_RUNTIME_DIR is set. Another local user can create that name first; ensure_private_dir then refuses it and the daemon does not start (it fails closed; there is no hijack).
Done when: the fallback is a directory no other user can create, for example under the clove home, with the socket path length limit and the upgrade from a hub running in the old location handled.