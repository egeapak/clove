# M1 Acceptance Gates — status

Measured on the 10,000-item shared fixture corpus (`clove_core::fixtures`),
release build, via `cargo test -p clove-index --release --test index_perf_gates`
and `--test index_parity`. Re-run any time; the gate tests assert the **met**
bounds in release and guard the unmet ones against gross regression.

| M1 gate (IMPLEMENTATION_PLAN) | Target | Measured (10k, release) | Status |
|---|---|---|---|
| `ls` output identical from file-scan and index paths (property test) | — | equal across 5 seeds + CLI test | ✅ met |
| `reindex` 10k items | < 1000 ms | ~640 ms | ✅ met |
| `search` 10k via FTS5 (selective query) | < 20 ms | ~3 ms | ✅ met |
| All M0 tests continue to pass | — | yes | ✅ met |
| `ls` 10k items, warm index | < 10 ms | ~18 ms | ❌ **gap** |
| Staleness detection, 10k items, 0 stale | < 5 ms | ~11–17 ms | ❌ **gap** |

Informational (not a separately listed gate): a **broad** `search` whose term
matches every item materializes all 10k rows and lands ~30 ms — the same
root cause as the `ls` gap below. A selective search (the case the FTS index
exists for) is ~3 ms.

## Coverage added for the met gates

- `crates/clove-index/benches/index.rs` — criterion benches: `reindex`, `ls`,
  `ready`, `staleness_clean`, and now `search`.
- `crates/clove-index/tests/index_perf_gates.rs` — wall-clock gate test
  (release-asserted), mirroring the M0 `perf_gates.rs` pattern.
- `crates/clove-index/tests/index_parity.rs` — the file↔index "identical output"
  gate as a multi-seed property-style test: for several corpora it asserts the
  index `ls`/`ready`/filtered id-sequences exactly equal an independent
  `clove_core::GraphStore` + file oracle.

## The two gaps — root cause and options (need a decision)

### 1. `ls` 10k < 10 ms (measured ~18 ms)

**Root cause:** `query_items` materializes a full 15-column **owned** `ItemRow`
per item (~1.8 µs/row → ~18 ms for 10k). The per-row label-JSON parse is *not*
the bottleneck — removing it saves only ~1 ms; the cost is SQLite stepping plus
~15 `String` allocations per row. Any large result set hits this (hence the broad
`search` at ~30 ms).

**Options:**
1. **Leaner `ls` read** — a query/row that materializes only the columns the
   `ls` table actually renders (id, status, type, priority, title, …), or uses
   `Box<str>`/`smol_str`/borrowed rows. Likely gets under 10 ms; lowest risk.
2. **Serve `ls` output from the index rows** instead of reloading files. Today
   the CLI uses the index only to get the ordered/filtered id set and then
   re-reads each item's frontmatter from disk (so output is byte-identical to the
   file path). That file reload means the *CLI* `ls` at 10k can never be < 10 ms
   regardless of SQLite speed. Hitting the gate end-to-end requires emitting
   output directly from index rows — which means reconstructing `deps`/`relates`/
   `duplicates`/`supersedes` from the `edges` table so the JSON still matches the
   file path. More work; affects the "identical output" guarantee.
3. **Accept ~18 ms** and revise the documented target.

### 2. Staleness, 10k, 0 stale < 5 ms (measured ~11–17 ms)

**Root cause:** `check_staleness` is O(n): it reads the directory and `stat`s
every file to honor the sub-2-second coarse-mtime guard that lets it detect
in-place content edits which preserve the file mtime (the HFS+ case covered by
`stale.rs::detects_modified_content_with_preserved_mtime`). The DESIGN's L1 fast
path is O(1) (stat the directory only).

**Options:**
1. **O(1) dir-stat fast path** — when the directory mtime + file count match the
   `meta` oracle, return clean without a readdir. Meets < 5 ms, but drops the
   per-file recent-edit guard, so a content rewrite that preserves a file's mtime
   *and* doesn't change the directory mtime (cp-in-place on a coarse-mtime FS) is
   missed until the next `git checkout`/`reindex`. Changes the behavior asserted
   by `detects_modified_content_with_preserved_mtime`.
2. **Hybrid** — O(1) when the directory mtime is older than the 2 s window; fall
   back to the O(n) readdir+hash pass only when the directory mtime is recent.
   Cheap in the common case; still misses the cp-in-place edge (which doesn't
   bump the dir mtime).
3. **Keep current correctness**, accept ~11–17 ms at 10k, revise the target.

Recommendation: option 1 (leaner `ls` read) for the first gap and option 2
(hybrid) for the second give the best correctness/speed balance, but both are
judgment calls — they change either output plumbing or the staleness contract,
so they're flagged here rather than applied unilaterally.
