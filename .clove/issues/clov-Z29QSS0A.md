---
schema: 1
id: clov-Z29QSS0A
title: clove agent-doc --out writes no file when the format is JSON
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:25:57.500Z
updated: 2026-11-14T19:25:57.546Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/agent_doc.rs handles --out only in the Human arm. 'CLOVE_FORMAT=json clove agent-doc --out AGENTS.md' prints the envelope, exits 0 and creates no file; the generated document itself recommends CLOVE_FORMAT=json. 'agent-doc --check --file F' prints nothing on success under --format json.
Expected: --out writes the document in every format (the JSON envelope names the path in data.out), and --check prints a success envelope under JSON.
Done when: crates/clove/tests/cli_regressions.rs agent_doc_out_and_check_work_in_json_format passes.