---
schema: 1
id: clov-DCB6TCD1
title: Web API has no authentication on the loopback port
status: closed
type: feature
priority: 2
created: 2026-11-14T19:04:18.706Z
updated: 2026-11-14T19:45:49.030Z
closed: 2026-11-14T19:45:49.030Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/src/lib.rs build_router and crates/clove-web/src/hub.rs have no auth layer; the only gates are the Host check and the WebSocket Origin check. Any local user can read and write every loaded project through http://127.0.0.1:<port>/, which bypasses the peer-uid check and the per-project token that protect the daemon IPC socket. docs/DESIGN.md section 8.4 states the web UI is not token-gated.
Options: (a) a per-hub random secret, passed in the URL the CLI prints and exchanged for a SameSite=Strict cookie; (b) serve over a Unix socket in the private runtime directory; (c) keep the documented behaviour.
Done when: a request without the credential is refused, or the decision to keep the current model is recorded in DESIGN.md.