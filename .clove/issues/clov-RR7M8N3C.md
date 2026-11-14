---
schema: 1
id: clov-RR7M8N3C
title: Usage errors print no JSON envelope, and a non-UTF-8 argument panics
status: in_progress
type: bug
priority: 2
created: 2026-11-14T19:25:57.574Z
updated: 2026-11-14T19:25:57.634Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/main.rs prints a clap parse error to stderr and exits 1 with an empty stdout even under --format json ('clove --format json show'); the inner parsers of cmd/export.rs and cmd/import.rs do the same. An unknown subcommand does get an envelope. main.rs also collects std::env::args() for the multiplexer-help pre-scan, which panics (exit 134) on an argument that is not valid UTF-8 ('clove show $'\\xff\\xfe'').
Expected: under JSON, a usage error prints the error envelope with code USAGE_ERROR and exit 1 on stdout (clap's text stays on stderr); a non-UTF-8 argument is a usage error.
Done when: crates/clove/tests/cli_regressions.rs usage_errors_carry_a_json_envelope and non_utf8_argument_is_a_usage_error_not_a_panic pass.