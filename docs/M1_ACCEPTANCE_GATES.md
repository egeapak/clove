# M1 Acceptance Gates — status

Measured on the 10,000-item shared fixture corpus (`clove_core::fixtures`),
release build, via `cargo test -p clove-index --release --test index_perf_gates`
and `--test index_parity`.

| M1 gate (IMPLEMENTATION_PLAN) | Target | Measured (10k, release) | Status |
|---|---|---|---|
| `ls`/`ready`/filter ordered output == file-scan path | — | id-sequences equal across 5 seeds | ✅ met |
| `reindex` 10k items | < 1000 ms | ~620 ms | ✅ met |
| `search` 10k via FTS5 (selective query) | < 20 ms | ~3 ms | ✅ met |
| Staleness detection, 10k, 0 stale | < 5 ms | ~3 ms (fast path) | ✅ **met** |
| All M0 tests continue to pass | — | yes | ✅ met |
| `ls` 10k items, warm index | < 10 ms | ~11 ms (lean) | 🟡 within ~10% |

## What changed (the two gaps, now resolved or near-resolved)

### `ls` 10k — 18 ms → ~11 ms (lean projection)
`clove ls` is now served **directly from the index** as a lean projection
(`query_list` → `id, status, type, priority, title`, the columns the list
renders) with **no per-item file read**. This removed both the file-reload and
the full 15-column owned-row materialization (incl. the per-row label-JSON
parse). The result is ~11 ms — down from ~18 ms, within ~10 % of the 10 ms
aspiration. The residual is SQLite's per-row step cost (~1 µs/row), the floor for
returning 10k rows individually; `smol_str` for the short columns was tried and
saved < 1 ms, so it was not kept.

**Output shape note (intentional):** the index path returns the lean object; the
file-scan path (no SQLite available) keeps the full frontmatter object. Both
agree on id ordering, and the human table is identical. `_meta.source`
(`"index"` vs `"files"`) tells consumers which shape they received. `show`
remains the full-detail view.

The asserted gate bound is an **interim 15 ms** (comfortably above the ~11 ms
measurement, below the old ~18 ms) pending a final decision: keep chasing < 10 ms
(would require not returning rows one-by-one) or formally set the target to ~12–15
ms for a 10k lean list.

### Staleness 0-stale — ~17 ms → ~3 ms (fast hybrid + `--deep`)
`check_staleness_fast` (the new CLI default) is O(readdir): when the directory
mtime and file count still match the `meta` oracle (and the directory was not
touched in the last 2 s), it returns clean **without stat-ing any file**. Only a
directory-level change triggers the full per-file pass. This meets the < 5 ms
gate (~3 ms).

**Tradeoff + escape hatch:** an in-place content rewrite that changes neither the
directory entry list nor the file count (i.e. not via clove's atomic rename) is
invisible to the fast path until the next add/delete/rename, `git checkout`, or
`reindex`. clove's own writes use an atomic rename (which bumps the directory
mtime), so they are always detected. The thorough per-file check remains
available via the global **`--deep`** flag (and is what `clove doctor` uses);
`check_staleness` (deep) is unchanged. Unit tests
`stale.rs::{fast_clean_when_directory_unchanged, fast_detects_added_and_deleted,
fast_misses_inplace_edit_that_deep_catches}` pin both behaviors.

## Coverage

- `crates/clove-index/benches/index.rs` — `reindex`, `ls`, `ready`,
  `staleness_clean`, `search`.
- `crates/clove-index/tests/index_perf_gates.rs` — release-asserted gate test
  (`ls_lean` ≤ 15 ms, `search_selective` ≤ 20 ms, `staleness_clean_fast` ≤ 5 ms,
  `reindex` ≤ 1000 ms), plus informational prints for the broad-match search and
  the deep staleness path.
- `crates/clove-index/tests/index_parity.rs` — the file↔index id-order parity
  gate (multi-seed).

## Still informational (not gates)
- Broad `search` whose term matches every item materializes all 10k rows → ~29
  ms; the same per-row floor as `ls`. Selective search (the FTS use case) is ~3
  ms.
- Deep staleness (`--deep`) at 10k → ~13 ms; opt-in by design.
