# M2 Acceptance Gates — status

Milestone **M2 — Interop** is complete and gated. M2 adds five capabilities and
**no new always-on requirements**: it never changes file-store correctness, adds
no new frontmatter fields, and leaves all M0/M1 gates intact.

- **T-M01** `clove import tk <.tickets-dir>`
- **T-M02** `clove import beads <issues.jsonl>`
- **T-M03** `clove import github` / `clove export github` (behind the `github`
  cargo feature; token via `GITHUB_TOKEN` or `gh auth token`)
- **T-M04** `clove export json` / `clove export jsonl`
- **T-M05** `clove merge-driver <O> <A> <B> <L>` (the 3-way item-file merge driver)

All gates below were verified on this branch: `cargo build --workspace`,
`cargo clippy --all-targets --all-features -- -D warnings` (and the default-feature
and `clove-import --no-default-features` clippy configs), `cargo fmt --all --check`,
and `cargo test --workspace --no-fail-fast` are clean **except** the single
pre-existing, environment-only failure
`clove-core repo::tests::linked_worktree_resolves_to_main_worktree` (the sandbox
routes `git commit` through a signing server that returns 400 — not a code defect;
tolerated by all milestones).

## M2 gate table (maps to VERIFICATION_PLAN.md M2-G01–M2-G06 + the export/fuzz rows)

| Gate | Source | Asserts | Enforced by | Status |
|---|---|---|---|---|
| M2-G01 | T-M01 AC | tk importer maps fields (`task→chore`, `tags→labels`, `links→relates`, H1→title, `external-ref`/tk-id→`external_ref`, `source_system="tk"`); labels normalized | `crates/clove/tests/import_tk.rs::maps_all_fields_correctly`, `missing_h1_emits_filename_fallback_warning` | ✅ |
| M2-G02 | T-M02 AC | Beads importer maps each dependency type + `deferred`→open(+label), `task→chore`, owner→assignee; unmapped fields stashed in `external_ref` as a ` meta:{…}` blob (full key `"beads:<id> meta:{…}"`); `comment_count>0`→stderr warning; `source_system="beads"` | `crates/clove/tests/import_beads.rs::maps_all_fields_correctly`, `comment_count_emits_stderr_warning` | ✅ |
| M2-G03 | T-M03 AC | GitHub `clove-meta` codec round-trips; `GitHubIssue→Item` mapping; idempotency filter; PRs skipped; export encodes meta into body. Network round-trip token-gated (see caveat) | offline: `crates/clove-import/src/github.rs` `mod tests` (12 tests incl. `clove_meta_round_trips`, `maps_github_issue_to_staged`, `idempotency_skips_already_imported`, `export_body_encodes_meta_and_preserves_body`); CLI offline: `tests/import_github.rs::export_github_dry_run_lists_local_items_offline` | ✅ |
| M2-G04 | all importers | every `--dry-run` writes **zero** files and reports `{would_create, would_skip, conflicts}` | `import_tk.rs::dry_run_writes_zero_files_and_reports_would_create`, `import_beads.rs::dry_run_writes_zero_files_and_reports_would_create`, `import_github.rs::export_github_dry_run_lists_local_items_offline` | ✅ |
| (idempotent) | all importers | re-import skips items whose `external_ref` already exists; zero new files on re-run | `import_tk.rs::re_import_is_idempotent`, `import_beads.rs::re_import_is_idempotent`, github `idempotency_skips_already_imported` | ✅ |
| M2-G05 | V-I14 | merge driver auto-resolves a same-value scalar edit (both sides → `closed`): `git merge` exits 0, `status==closed` with a valid timestamp | `crates/clove/tests/merge_driver.rs::v_i14_same_value_status_auto_resolves` | ✅ |
| M2-G05a | V-I15 | dep union (A adds `proj-AAA`, B adds `proj-BBB`) → merged sorted `[proj-AAA, proj-BBB]`, no conflict markers | `merge_driver.rs::v_i15_dep_union_merge` | ✅ |
| M2-G05b | V-I16 | dep removal/add on the same element → conflict, **isolated to `deps`** | `merge_driver.rs::v_i16_dep_removal_conflict` | ✅ |
| (merge — divergent) | T-M05 AC | divergent scalar (A→in_progress, B→closed) → conflict; disjoint-field edits → clean; one-sided body edit clean, both-sides body edit → markers; unparseable side → nonzero without clobbering ours | `merge_driver.rs::{divergent_scalar_status_conflicts, clean_disjoint_scalar_merge, one_sided_body_edit_merges_clean, conflicting_body_edits_produce_markers, unparseable_side_conflicts_without_clobbering_ours}` | ✅ |
| (merge — proptest) | T-M05 | set-merge equals the mathematical 3-way union, is sorted/deduped, and is commutative in ours/theirs for the non-conflict case | `crates/clove-import/tests/merge_props.rs::{clean_merge_equals_reference_and_is_sorted, merge_is_commutative_in_ours_theirs}` (proptest) | ✅ |
| (export) | T-M04 | `export jsonl` — every line is standalone JSON, line count == item count; `export json` validates against the item-list JSON schema; byte-deterministic; `--out` writes file + empty stdout; empty repo → 0 lines / empty `data` | `crates/clove/tests/export.rs` (8 tests) | ✅ |
| (JSONL round-trip) | T-M04 | `clove export jsonl` then `clove import beads` of that output into a fresh repo is lossless on mapped fields and idempotent | `crates/clove/tests/import_beads.rs::jsonl_round_trip_is_lossless_and_idempotent` | ✅ |
| M2-G06 | full suite | all M0 + M1 acceptance gates still pass | `cargo test --workspace` (M0 `clove-core` suite, M1 `clove-index` suite); release perf gates below | ✅ |

## GitHub network caveat (token-gated)

All GitHub **field-mapping and `clove-meta` codec logic is offline-unit-tested**
and runs everywhere (in `crates/clove-import/src/github.rs` `mod tests` and the
offline CLI dry-run test). The **network round-trip**
(`crates/clove/tests/import_github.rs::github_roundtrip`) is
`#[ignore]`-by-default and additionally short-circuits when `GITHUB_TOKEN` is
unset, so it **does not run in CI or the sandbox** and reports as `1 ignored`. To
exercise it locally:

```
GITHUB_TOKEN=ghp_xxx CLOVE_TEST_GH_REPO=youruser/scratch-repo \
  cargo test -p clove --test import_github -- --ignored github_roundtrip
```

The `github` feature is on by default; lean / cross builds use
`cargo build -p clove-import --no-default-features` to drop the `octocrab`/`tokio`
weight (verified clippy-clean in that config).

## Fuzz (T-X02)

Three new cargo-fuzz targets harden the new parsers/merge against arbitrary bytes;
all are registered in `fuzz/Cargo.toml` with committed seed corpora in
`fuzz/corpus/<target>/`, and each runs 30 s against its corpus in the CI `fuzz`
job (alongside the existing `parse_item_file` / `parse_dep_list`).

| Target | Property | Iterations observed | Crashes |
|---|---|---|---|
| `merge_driver` | arbitrary O/A/B blobs → never panics, never escapes the ours path, always terminates | ~105k | 0 |
| `import_tk` | arbitrary markdown+frontmatter → no panic (reuses M0 parse hardening) | ~195k | 0 |
| `import_beads` | arbitrary JSONL bytes → malformed lines reported, never crashes | ~1.15M | 0 |

Run any target locally with:

```
cargo +nightly fuzz run merge_driver   # or import_tk / import_beads
cargo +nightly fuzz run import_beads -- -max_total_time=30   # CI-style 30s replay
```

## Benches (informational, no hard gate)

Criterion benches compile under `cargo bench --no-run --workspace` (CI
"Benchmarks compile" step) and are informational only:

- `crates/clove-import/benches/bench_export.rs` — export 10k-item fixture → JSONL
  throughput (plan target: < 200 ms warm; informational).
- `crates/clove-import/benches/bench_import_tk.rs` — import 1k tk tickets.
- `crates/clove-import/benches/bench_import_beads.rs` — import 10k-line JSONL.

These were confirmed to **compile** (`cargo bench --no-run --workspace`, exit 0);
the M2 plan treats their throughput numbers as informational, not gates.

## M0 + M1 gates re-run (M2-G06)

Re-run green on this branch (release where the gate requires it):

| Suite | Command | Result |
|---|---|---|
| M0 perf gates | `cargo test --release -p clove-core --test perf_gates` | ✅ 3 passed |
| M1 index perf gates | `cargo test --release -p clove-index --test index_perf_gates` | ✅ 1 passed (see `docs/M1_ACCEPTANCE_GATES.md`) |
| M1 file↔index parity | `cargo test --release -p clove-index --test index_parity` | ✅ 1 passed |
| Full workspace | `cargo test --workspace --no-fail-fast` | ✅ all green except the tolerated `linked_worktree` env failure; `github_roundtrip` shows `1 ignored` |

## Coverage map

- `crates/clove-import/src/` — `tk.rs` (T-M01), `beads.rs` (T-M02), `github.rs`
  (T-M03, `github` feature), `export.rs` (T-M04), `merge.rs` (T-M05 set-merge +
  scalar logic), `map.rs` (shared coercion + `external_ref` idempotency index),
  `plan.rs` (`ImportPlan`/`ImportReport`), `error.rs`, `lib.rs`.
- `crates/clove/src/cmd/` — `import.rs`, `export.rs`, `merge_driver.rs`.
- Tests: `crates/clove/tests/{import_tk,import_beads,import_github,export,merge_driver}.rs`,
  `crates/clove-import/tests/merge_props.rs`, github offline `mod tests`.
- Fuzz: `fuzz/fuzz_targets/{merge_driver,import_tk,import_beads}.rs` + corpora.
- Benches: `crates/clove-import/benches/bench_{export,import_tk,import_beads}.rs`.
