---
schema: 1
id: clov-JQA3TCDS
title: web_port test explicit_host_skips_the_daemon_hand_off fails on hosts without IPv6
status: closed
type: bug
priority: 2
created: 2026-10-31T11:13:13.485Z
updated: 2026-10-31T18:04:40.026Z
closed: 2026-10-31T18:04:40.026Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/tests/web_port.rs explicit_host_skips_the_daemon_hand_off starts 'clove serve --host ::1' and connects to [::1]. On a host where IPv6 is not available (socket(AF_INET6) fails with EAFNOSUPPORT, errno 97), no server listens on [::1] and the test fails at the 'no standalone server on [::1] serves this repo' assert (line ~307). The failure is the same on a clean checkout.
Expected: the test skips (with a message) when an AF_INET6 socket cannot bind ::1, like other environment-dependent tests.
Done when: cargo test -p clove-cli --all-features --test web_port passes on a host without IPv6 and still runs the check on a host with IPv6.