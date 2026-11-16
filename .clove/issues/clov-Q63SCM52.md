---
schema: 1
id: clov-Q63SCM52
title: MCP notifier misses a write made before its first successful poll
status: closed
type: bug
priority: 2
created: 2026-11-15T06:57:58.484Z
updated: 2026-11-15T06:57:58.700Z
closed: 2026-11-15T06:57:58.700Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-mcp/src/lib.rs spawn_notifier sleeps one interval before its first poll of the daemon change generation and uses that first value only as a baseline. A write that lands between the subscription and the first successful poll (for example while cloved is still starting) bumps the generation into the baseline, and the subscribed client never gets resources/updated for it.
Expected: read the baseline before the first sleep; when no daemon answers yet, use 0, because a daemon and each project it loads count from 0.
Done when: clove-mcp tests::a_write_before_the_first_successful_poll_still_notifies passes and cargo nextest run -p clove-cli --all-features --test mcp passes.