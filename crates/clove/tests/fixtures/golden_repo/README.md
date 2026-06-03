# golden_repo fixture

A committed, deterministic 7-item store used by the golden CLI snapshot tests
(`crates/clove/tests/golden_cli.rs`) and referenced by the M0 acceptance gates
(`docs/IMPLEMENTATION_PLAN.md` §"M0 Acceptance Gates").

All timestamps are fixed so JSON output is byte-stable across runs.

## Shape: 2 dependency chains + 1 cycle

- **Chain one (3 nodes):** `proj-AAAAAAAA` → `proj-BBBBBBBB` → `proj-CCCCCCCC`
  - `C` is **closed**, so `B` is **ready** and `A` is **blocked** by the open `B`.
- **Chain two (2 nodes):** `proj-DDDDDDDD` → `proj-EEEEEEEE`
  - `E` is **in_progress** (no deps); `D` is **blocked** by `E`.
- **Cycle (2 nodes):** `proj-FFFFFFFF` ↔ `proj-GGGGGGGG`
  - `F` depends on `G` and `G` depends on `F` — detected by `clove dep cycle`.

The tests `clove init` a temp repo and copy `issues/*.md` into `.clove/issues/`,
so the fixture only needs the item files, not a `.clove/config.toml`.
