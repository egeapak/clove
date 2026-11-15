---
schema: 1
id: clov-9V7FAXHR
title: Web write responses report the serving mode in _meta.source
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.512Z
updated: 2026-11-14T19:30:06.512Z
closed: 2026-11-14T19:30:06.512Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/src/write.rs respond_item and delete_item put state.source in _meta.source. clove serve returns "standalone", which is outside the daemon|index|files enum of docs/json-schema/v1, and a cloved-hosted server returns "daemon" for a write that went to the files. The read endpoints already report the tier.
Expected: write responses report "files" (clove_engine::Source::Files).
Done when: crates/clove-web/tests/api.rs::write_responses_report_the_files_tier passes.