---
schema: 1
id: clov-5G5C31AY
title: Missing /_app chunk is answered with index.html; version.json cached as immutable
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.659Z
updated: 2026-11-14T19:30:30.258Z
closed: 2026-11-14T19:30:30.258Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/src/assets.rs static_handler answers a path under _app/ that is not in the asset table with 200 and index.html, so a stale tab's dynamic import fails on MIME type instead of a 404. cache_for marks every path under _app/ as immutable for a year, including the unhashed _app/version.json.
Expected: an unknown path under _app/ is 404; only _app/immutable/ is cached as immutable.
Done when: assets.rs tests cache_is_immutable_only_for_hashed_assets and crates/clove-web/tests/api.rs::missing_app_chunk_is_404 pass.