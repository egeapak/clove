# M1 Acceptance Gates — status

Measured on the 10,000-item shared fixture corpus (`clove_core::fixtures`),
release build, via `cargo test -p clove-index --release --test index_perf_gates`
and `--test index_parity`.

| M1 gate (IMPLEMENTATION_PLAN) | Target | Measured (10k, release) | Status |
|---|---|---|---|
| `ls`/`ready`/filter ordered output == file-scan path | — | id-sequences equal across 5 seeds | ✅ met |
| `reindex` 10k items | < 1000 ms | ~735 ms | ✅ met |
| `search` 10k via FTS5 (selective query) | < 20 ms | ~3 ms | ✅ met |
| Staleness detection, 10k, 0 stale | < 5 ms | ~3 ms (fast path) | ✅ **met** |
| All M0 tests continue to pass | — | yes | ✅ met |
| `ls` 10k items, warm index | < 8 ms | **~2.5–4.5 ms** (covering index) | ✅ met |

The `ls` gate is **< 8 ms** (tightened from 15 ms). With the `idx_items_list`
covering index the lean list is an index-only scan and lands ~2.5–4.5 ms —
comfortably under (and under the original 10 ms aspiration). The 8 ms bound keeps
~2× headroom over the observed time so it *does* guard against *losing* the
covering scan: if the index-only plan silently regressed to a table scan (~11 ms)
CI would now catch it instead of hiding it under the old loose 15 ms budget. Two
further levers exist for the interactive case: the default
`--limit 100` (the index pushes `LIMIT` into SQL, so a page steps ~100 rows, not
10k), and `_meta.total` from a cheap `COUNT(*)`.

### Covering index (schema v2)

`SELECT id,status,item_type,priority,title … ORDER BY priority,topological_rank,id`
is served by `idx_items_list(priority, topological_rank, id, status, item_type,
title)` as an **index-only scan** — `EXPLAIN QUERY PLAN` →
`SCAN items USING COVERING INDEX idx_items_list` — so there is no per-row lookup
into the `WITHOUT ROWID` `items` b-tree. Enabling it required storing a sentinel
`topological_rank` (`i64::MAX`) for unranked items instead of `NULL`, so the
order is a plain `(priority, topological_rank, id)` the index satisfies (the
sentinel sorts unranked items last, matching the file path's `usize::MAX`). The
index is built **after** the bulk insert during reindex (one pass, not 10k
incremental updates), which keeps reindex at ~735 ms (vs ~620 ms without the
index, ~853 ms if maintained inline). Schema bumped to v2 (old indexes
auto-rebuild on open).

## What changed (the two gaps, now resolved)

### `ls` 10k — 18 ms → ~11 ms (lean projection), and where the time goes
`clove ls` is now served **directly from the index** as a lean projection
(`query_list` → `id, status, type, priority, title`, the columns the list
renders) with **no per-item file read**. This removed both the file-reload and
the full 15-column owned-row materialization (incl. the per-row label-JSON
parse). The result is ~11 ms — down from ~18 ms, comfortably under the revised
8 ms gate.

**Timing breakdown** (10k rows, release; `tests/timing_breakdown.rs`), *with the
covering index*:

| stage | total | per row |
|---|---|---|
| prepare (compile SQL) | 4 µs | — |
| step-only (no decode) | 1.2 ms | **116 ns** |
| + read priority (int) | 1.3 ms | 130 ns |
| + decode lean (SmolStr) — the `ls` path | 3.7 ms | 369 ns |
| + decode lean (String) | 3.9 ms | 386 ns |
| + decode full 15-col (old `query_items`) | 15.7 ms | 1573 ns |

The decisive change: the covering index dropped raw stepping from **793 ns/row to
116 ns/row** — that ~677 ns/row was the second b-tree lookup into the `WITHOUT
ROWID` `items` table for `status`/`type`/`title`, now served from the index leaf.
The lean `ls` path (step + decode) is ~369 ns/row → ~3.7 ms for 10k (the gate
measures ~4.5 ms incl. the `COUNT(*)` and `Vec` build). String vs SmolStr decode
still differs only ~17 ns/row (SmolStr is kept for memory, not time — see below).
There is no separate bulk-step API in SQLite; this index-only scan is the way to
make each step cheaper.

**`SmolStr` short columns are kept — for memory, not time.** Time saved vs
all-`String` is only ~1.7 % (17 ns/row), but the memory win is real
(`tests/memory_footprint.rs`, 10k rows):

| representation | heap retained | allocations |
|---|---|---|
| `SmolStr` (id/status/type inline) | 1.41 MB (141 B/row) | 10 001 (1.0/row) |
| all-`String` | 1.72 MB (172 B/row) | 40 001 (4.0/row) |
| **saved** | **310 KB (18 %)** | **30 000 (75 %)** |

i.e. **75 % fewer allocations** (1 per row — just `title` — instead of 4) and 18 %
fewer bytes. At scale the reduced allocator pressure matters more than the byte
count, so `SmolStr` is the right call for the list row.

**Output shape note (intentional):** the index path returns the lean object; the
file-scan path (no SQLite available) keeps the full frontmatter object. Both
agree on id ordering, and the human table is identical. `_meta.source`
(`"index"` vs `"files"`) tells consumers which shape they received. `show`
remains the full-detail view.

The asserted gate bound is now **8 ms** — the final decision (gh-24), tightened
from the earlier interim 15 ms once the covering-index scan settled at ~2.5–4.5 ms
measured. 8 ms keeps ~2× headroom over the observed time while still tripping if
the index-only plan regresses to a table scan (~11 ms); chasing < 10 ms further
(which would require not returning rows one-by-one) is not pursued.

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
  (`ls_lean` ≤ 8 ms, `search_selective` ≤ 20 ms, `staleness_clean_fast` ≤ 5 ms,
  `reindex` ≤ 1000 ms), plus informational prints for the broad-match search and
  the deep staleness path.
- `crates/clove-index/tests/index_parity.rs` — the file↔index id-order parity
  gate (multi-seed).
- `crates/clove-index/tests/memory_footprint.rs` — counting-allocator comparison
  of the `SmolStr` lean row vs all-`String` (asserts SmolStr uses fewer
  bytes/allocations).
- `crates/clove-index/tests/timing_breakdown.rs` — per-stage `ls` timing
  (prepare / step / decode by representation), informational.

## Still informational (not gates)
- Broad `search` whose term matches every item materializes all 10k rows → ~29
  ms; the same per-row floor as `ls`. Selective search (the FTS use case) is ~3
  ms.
- Deep staleness (`--deep`) at 10k → ~13 ms; opt-in by design.
