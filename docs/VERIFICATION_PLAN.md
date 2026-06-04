# clove — Verification Plan

> **Status:** Authoritative test strategy. Cross-references DESIGN.md (§) and
> IMPLEMENTATION_PLAN.md (T-*). All PRD §10 performance claims and §15 success criteria
> are mapped to specific, falsifiable checks.

---

## 1. Test Pyramid Overview

| Layer | Crate | Tool | Count Target | Run In |
|---|---|---|---|---|
| Unit | clove-core | `#[test]` | ~150 tests | `cargo test` |
| Integration (file store) | clove-core | `tempfile` + `#[test]` | ~60 tests | `cargo test` |
| Integration (SQLite index) | clove-index | `tempfile` + `#[test]` | ~40 tests | `cargo test` |
| Golden CLI snapshots | clove | `assert_cmd` + `insta` | 1 per command variant | `cargo test` |
| Property-based | clove-core | `proptest` | ~15 properties | `cargo test` |
| Fuzz | crates/fuzz | `cargo-fuzz` | 2 targets | 30s in CI; 24h pre-release |
| Performance regression | all | `criterion` + `#[test]` gates | ~12 benchmarks | criterion in CI (compile check + gate tests) |
| Comparative benchmark | external | `hyperfine` + xtask | N/A | manual pre-release |

---

## 2. Unit Tests (clove-core, zero I/O)

All tests in this section are `#[test]` in `clove-core`, with no filesystem I/O. All
dependency on time uses injected clocks or fixed timestamps.

### 2.1 DAG Engine

**V-U01: Three-node blocking chain**
```
Given: items A (open, deps=[B]), B (open, deps=[C]), C (open)
Assert: ready_items() == []
        blocked_items() contains A (blocking_deps=[B]) and B (blocking_deps=[C])
Close C:
Assert: ready_items() == [B]
Close B:
Assert: ready_items() == [A]
```

**V-U02: Partition completeness invariant**
```
Given: any randomly-constructed set of items with mixed statuses
Assert: ready ∪ blocked ∪ closed == all_items
Assert: ready ∩ blocked == {}
Assert: no item in ready has an open DependsOn neighbor
Assert: no item in blocked has all deps closed AND no dangling deps
```
Run as a proptest property (V-P02).

**V-U03: Cycle detection variants**
Table-driven test over 8 graph shapes:

| Shape | has_cycle() | cycle_members() |
|---|---|---|
| Empty graph | false | [] |
| Single node | false | [] |
| Two nodes, no edge | false | [] |
| Two nodes, A→B (no cycle) | false | [] |
| Two nodes, A→B→A | true | [A, B] |
| Three nodes, linear A→B→C | false | [] |
| Three nodes, cycle A→B→C→A | true | [A, B, C] |
| Diamond A→{B,C}→D | false | [] |
| Diamond + back edge D→A | true | [A, B, C, D] |
| Self-loop A→A | true | [A] |

**V-U04: Dangling deps**
```
Given: item X with deps: ['missing-id-XXXXXX']
Assert: ready_items() does not contain X
Assert: blocked_items() entry for X has dangling_deps == ['missing-id-XXXXXX']
Assert: blocked_items() entry for X has blocking_deps == []
Assert: missing-id is in graph.dangling_ids
```

**V-U05: Soft relations do not block**
```
Given: items P (open) and Q (open) with a Relates edge P→Q
Assert: ready_items() contains both P and Q (neither blocked)
Given: items P (open) and Q (open) with Duplicates edge P→Q
Assert: same — neither blocked
```
This is the correctness test for `is_hard_dep()`.

**V-U06: Epic children summary**
```
Given: epic E with children C1 (closed), C2 (open), C3 (closed)
Assert: epic_children_summary(E) == { total: 3, closed: 2, completable: false }
Close C2:
Assert: { total: 3, closed: 3, completable: true }
Assert: epic_children_summary(non-epic-id) == None
```

**V-U07: Dep tree depth limit**
```
Given: linear chain A1→A2→...→A20 (all open)
Call: dep_tree(A1, max_depth=5)
Assert: tree depth == 5 (A6 does not appear)
Assert: no infinite recursion (completes in < 10ms)
```

**V-U08: Dep tree cycle marker**
```
Given: A→B→C→A (cycle)
Call: dep_tree(A, max_depth=100)
Assert: A appears at root
Assert: somewhere in the tree, a node has cycle_ref=true
Assert: function completes (no infinite loop)
Assert: no node appears more than twice in the full tree
```

**V-U09: Self-loop rejection**
```
Given: item A
Call: dep_add(A, A)
Assert: CloveError::ValidationError returned (code SELF_LOOP, exit 4)
Assert: NOT CloveError::CycleDetected (self-loops are caught before the cycle-path check)
Assert: no edge added to graph
```
Per DESIGN.md §5.4: a self-loop is rejected as a bad argument (ValidationError, exit 4) before
the `has_path_connecting` cycle check. CycleDetected (exit 3) is reserved for non-trivial
cycles discovered by the path-connectivity algorithm.

**V-U10: Parent cycle detection**
```
Given: items X (parent=Y) and Y (parent=X) loaded from files
Assert: graph construction emits malformed_parent error for both
Assert: neither X nor Y appears in ready_items()
```

**V-U11: Topological sort output ordering**
```
Given: DAG with items at different depths and priorities
Assert: ready_items() output is sorted by (priority ASC, topological_rank ASC)
Assert: sources of the DAG (no incoming deps) appear before items they block
```

---

### 2.2 Item Model and Serialization

**V-U12: Status / closed invariant**
```
Closed { at } serializes to:
  status: closed
  closed: <at>
Deserialized back → Closed { at } with same timestamp
open status serializes with no 'closed' field
status=closed without closed timestamp → ValidationError
closed timestamp with status!=closed → ValidationError
```

**V-U13: Priority validation**
```
Priority(5) → ValidationError
Priority(4) → Ok
Priority(0) → Ok
```

**V-U14: ID validation matrix**
```
"proj-7AF3K2MN" → Ok (valid 8-char Crockford)
"proj-7af" → Error (lowercase not in Crockford alphabet; wrong length)
"../evil-7AF3K2MN" → Error (traversal)
"proj-7AF3K2MN/../../etc/passwd" → Error (traversal)
"" → Error (empty)
"a" → Error (too short, no hyphen)
"proj-ILOUXXX1" → Error (I, L, O, U not in Crockford alphabet)
```

**V-U15: Frontmatter field order**
```
Given: fully-populated ItemFrontmatter
Call: FrontmatterWriter::write_item
Assert: output starts with "schema: 1\n"
Assert: "id:" appears before "title:"
Assert: the byte offset of "created:" in the output is less than the byte offset of "updated:"
       (created appears first in canonical order: schema, id, title, status, type, priority, created, updated)
Assert: optional fields absent when None/empty
```
Exact byte sequence for canonical ordering is committed as a golden fixture.

**V-U16: List serialization rules**
```
labels = ["z-label", "a-label"] serializes as labels: [a-label, z-label] (sorted)
deps = [] serializes as deps: []  (not omitted, per §2.2 and §14.5)
```

**V-U16c: Label normalization (case-insensitive labels, §2.2)**
```
normalize_label("Area:iOS") == "area:ios"
normalize_label("  AREA:IOS  ") == "area:ios"   (trim + lowercase)
normalize_label("area  :  ios") collapses internal whitespace per rule
normalize_label("   ") -> Err (empty after trim)
Adding "area:iOS" then "area:ios" to an item yields exactly one label "area:ios"
A `--label AREA:IOS` filter matches an item whose stored label is "area:ios"
```

**V-U16a: assignee null — YAML file layer**
```
Given: ItemFrontmatter with assignee = None
Call: FrontmatterWriter::write_item
Assert: the output does NOT contain "assignee:" (optional field omitted when None, per §2.2)
```

**V-U16b: assignee null — JSON output layer**
```
Given: ItemFrontmatter with assignee = None
Serialize via serde_json (the JSON output path)
Assert: the JSON object contains "assignee": null (present in JSON schema per §7.4)
Assert: the JSON object does NOT omit the "assignee" key
```
Note: The two layers have different rules. FrontmatterWriter (YAML) omits None optionals;
serde JSON serialization keeps them as null (no `skip_serializing_if` on optional scalar
fields — see T-C01 and §2.3 struct definition).

**V-U17: Schema version handling**
```
Frontmatter with missing "schema" field → parsed as schema=1, no error
Frontmatter with "schema: 1" → Ok
Frontmatter with "schema: 99" → ValidationError::UnknownSchema
```

---

### 2.3 ID Generation

**V-U18: ID uniqueness (in-process)**
```
Generate 100,000 IDs using the production generate_id() function
Insert into HashSet
Assert: len == 100,000 (zero collisions)
Assert: every ID matches ^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$
```

**V-U19: ID concurrent uniqueness**
```
Spawn 50 threads, each generating 200 IDs
Collect all 10,000 into a single Vec
Assert: no duplicates
Assert: all match the format regex
```

**V-U20: Birthday paradox bound**
```
Compute collision probability analytically for 10,000 IDs in 1.1T space
Assert: probability < 0.01%  (verifies design choice, not RNG)
```

**V-U21: ID path traversal**
```
For each of: "../evil", "../../etc/passwd", "/absolute", "has/slash", "has\backslash",
  "null\x00byte", "I-am-32-chars-plus-one-XXXXXXXX", "XXXX-XXXXXXXXXXXXXXXXXX" (too long):
  Assert: CloveId::new(s) returns Err
  Assert: CloveId::to_path() never returns a path outside issues_dir
```

---

## 3. Integration Tests (file store, tempfile)

All tests in this section use `tempfile::TempDir` for isolation. No SQLite.

### 3.1 ItemStore

**V-I01: Full round-trip**
```
clove init → clove new "Test Item" -t feature -p 1 -l area:core -d proj-XXXXXXXX
  → clove show <id> (verify all fields)
  → clove status <id> closed (verify closed timestamp set)
  → clove show <id> (verify status=closed, closed=<ts>)
  → clove status <id> open (verify closed field absent)
```

**V-I02: Atomic write crash simulation**
```
Mock the Write impl to fail at a random byte offset
Call write_item_atomic
Assert: .md file either absent or contains pre-crash content (never partial)
Assert: no .md.tmp file survives after the simulated crash
Run 1,000 iterations
```

**V-I03: Concurrent write correctness**
```
Spawn 10 threads, each calling ItemStore::create on different IDs
Assert: all 10 .md files exist and are valid after all threads complete
Assert: no file is corrupt (parse succeeds)
```

**V-I04: Concurrent dep add**
```
Create item A with deps=[]
Spawn 2 threads, both calling dep_add(A, B) and dep_add(A, C) simultaneously
Assert: final deps list is exactly [B, C] (sorted) — not [B] or [C] alone
Assert: file is never observed in partial-write state during concurrent reads
```

**V-I05: Scan skips symlinks and temp files**
```
Create items normal1.md, normal2.md
Create symlink evil.md -> /etc/passwd
Create temp file clove-XXX.md.tmp
Assert: ItemStore::list() returns exactly 2 items
Assert: /etc/passwd contents never appear in output
```

**V-I06: Scan continues on bad files**
```
Create 5 valid items
Write an invalid YAML frontmatter to a 6th file
Assert: ItemStore::list() returns 5 items (soft parse failure)
Assert: ScanError::ParseFailed is yielded for the bad file (not silently skipped AND not abort)
```

**V-I07: `clove init` idempotency**
```
Run clove init twice in same directory
Assert: second run exits 0 with "already initialized" message
Assert: config.toml contents unchanged
Assert: .gitignore contents unchanged
Assert: no second .clove/.clove/ created
```

**V-I08: Config prefix validation rejection**
```
For each invalid prefix: "../bad", "has/slash", "toolongprefixname", "123startsdigit", "":
  Assert: CloveConfig::load() returns ConfigError::InvalidPrefix
```

**V-I09a: Idempotency test**
```
Run `clove status <id> in_progress` on an item already in in_progress state
(i.e., a no-op status transition)
Re-read the file immediately
Assert: only the `updated` field differs from the previous write (no spurious field changes)
Assert: all other fields are byte-identical to the pre-write state
```
Note: `updated` will change because the write always stamps the current time. The test
confirms that no unrelated fields are modified by a no-op status command.

**V-I09b: Mutation timestamp test**
```
Create item with status=open
Capture file content (content_A)
Run `clove status <id> in_progress` (a real transition)
Capture file content (content_B)
Assert: `updated` field in content_B is a later RFC3339 timestamp than in content_A
Assert: `status` field changed from `open` to `in_progress`
```

### 3.2 Comments

**V-I10: Comment round-trip**
```
Create item, add comment "hello world"
Assert: list_comments returns exactly 1 comment with body "hello world"
Assert: comment_count in clove show == 1
```

**V-I11: Concurrent comment conflict-free merge**
```
Create a git repo in tempdir
Create item I on main branch
Branch → agent-A adds comment "A's comment" (nanosecond ts X)
Branch → agent-B adds comment "B's comment" (nanosecond ts Y)
git merge branch-B into branch-A
Assert: git merge exits 0 (no conflicts)
Assert: list_comments returns both comments sorted chronologically
```

**V-I12: Comment same-second collision handling**
```
Mock SystemTime::now() to return same value for first two calls
Call add_comment twice for same item
Assert: two distinct files created on disk
```

### 3.3 Git / Merge Semantics

**V-I13: Parallel branch item creation — no conflict**
```
Using git2: init repo, branch-A creates item X, branch-B creates item Y
Merge branch-B into branch-A
Assert: git merge exits 0 (no conflicts)
Assert: both items X and Y present in list
Assert: X and Y have different IDs (random ID guarantee)
```

**V-I14: Same-field concurrent edit — merge driver resolves**
```
Both branches set status of same item to "closed"
Merge (merge driver installed)
Assert: git merge exits 0 (auto-resolved as same-value conflict)
Assert: final status == closed with valid closed timestamp
```

**V-I15: Dep union merge**
```
Branch-A adds dep proj-AAA to item I
Branch-B adds dep proj-BBB to item I
Merge
Assert: item I has deps: [proj-AAA, proj-BBB] sorted
Assert: no conflict markers in file
```

**V-I16: Dep removal conflict**
```
Branch-A removes dep proj-OLD from item I (I.deps was [proj-OLD])
Branch-B adds dep proj-NEW to item I (I.deps becomes [proj-OLD, proj-NEW])
Merge
Assert: git merge reports a conflict (not silently resolved)
Assert: conflict is in the deps field only
```

---

**V-I17: `clove doctor` detects each issue class (DESIGN §7.7 / T-CLI18)**
```
Seed a repo with exactly one of each: dangling dep, 2-node DependsOn cycle,
id/filename mismatch, duplicate id (two files, same id), priority: 9 (invalid),
non-canonical label "area:iOS", orphaned <id>/comments/ dir (no <id>.md).
Run: clove doctor --format json
Assert: report contains exactly those findings, each with the correct code and severity
        (cycle/dangling/mismatch/dup/invalid-field = error; non-canonical-label/orphan = warning)
Assert: summary.errors and summary.warnings counts match
```

**V-I18: `clove doctor --fix` repairs only safe issues**
```
Given the V-I17 repo
Run: clove doctor --fix
Assert: "area:iOS" rewritten to "area:ios"; orphaned comments dir removed;
        list fields re-sorted/deduped
Assert: structural errors (dangling, cycle, id mismatch, duplicate id, invalid priority) are UNCHANGED
Assert: summary.fixed == count of warning-class issues
```

**V-I19: `clove doctor --strict` exit code**
```
On the V-I17 repo: clove doctor --strict  -> exit 4 (unresolved errors remain)
After hand-fixing all error-class issues: clove doctor --strict -> exit 0
On a clean repo: clove doctor (and --strict) -> exit 0, {errors:0, warnings:0}
```

---

## 4. Integration Tests (SQLite Index)

**V-S01: File/index consistency (property test)**
```
Generate random sequences of 200 mutations (create, update, status, dep add/rm, delete)
Apply each to file store
Run reindex after each
Assert: clove ls --format json output identical from file-scan and index paths
This is the invariant test guarding the entire consistency contract
```

**V-S02: Staleness detection with mtime-preserved copy**
```
Create 100 items, build index
Use cp -p to replace 10 files with modified content (preserving mtime — HFS+ simulation)
Assert: check_staleness() detects all 10 as stale (mtime alone would miss them; hash catches them)
```

**V-S03: git checkout consistency**
```
Create repo with 50 items on branch-A, build index
Create branch-B, modify 20 items, build index on branch-B
git checkout back to branch-A
Run clove ls
Assert: results match fresh file-scan on branch-A (20 items correctly detected stale and refreshed)
```

**V-S04: Concurrent index write safety**
```
Spawn 10 parallel `clove new` processes against same repo
Assert: all 10 exit 0
Assert: clove ls shows exactly 10 items
Assert: no SQLITE_BUSY errors propagated to user
```

**V-S05: Index absent fallback**
```
Delete index.db
Run `clove ls --format json`
Assert: exit code 0
Assert: JSON output structurally identical to indexed path (same fields, same items)
Assert: stderr contains "no index found" INFO line
Assert: stdout contains no error text
Assert: _meta.source == "files"
```

**V-S06: Schema migration test**
```
Build index with correct schema
Manually set PRAGMA user_version = 0
Run any read command
Assert: exit code 0
Assert: correct results returned (rebuilt index or file-scan fallback)
Assert: DB now has correct schema_version
```

**V-S07: Reindex idempotency**
```
Run clove reindex twice on same repo
Assert: results from clove ls are identical after both runs
Assert: second reindex does not fail (idempotent)
```

**V-S08: Reindex re-entrance prevention**
```
Spawn two clove reindex processes simultaneously
Assert: exactly one succeeds (exit 0)
Assert: other exits non-zero with "reindex already running" message
Assert: final index.db is consistent (not a partial merge)
```

**V-S09: FTS5 consistency**
```
Create 100 items with known body text
Build index
Run clove search for known terms → assert results match rayon fallback
Update 10 items' body text, reindex
Run clove search again → assert results reflect updated content
```

**V-S10: Deletion detection**
```
Create 50 items, build index
Delete 10 .md files directly (git rm simulation)
Run any read command
Assert: 10 deleted items do not appear in results
```

**V-S11: HEAD change detection**
```
Build index on branch-A
Switch to branch-B (different items)
Run clove ls
Assert: meta.last_git_head updated
Assert: results reflect branch-B state (full readdir staleness pass triggered)
```

---

## 5. Golden CLI Snapshot Tests

All snapshot tests use `assert_cmd` + `insta`. Fixture: committed `tests/fixtures/golden_repo/`
(7 items, 2 dep chains, 1 cycle, 1 epic). Snapshots committed to `tests/golden/`. Reviewed
and updated intentionally; never auto-accepted in CI.

| Test Name | Command | Assertions |
|---|---|---|
| V-G01 | `clove show proj-7af` | human format, all fields |
| V-G02 | `clove show proj-7af --format json` | validates against v1 item schema |
| V-G03 | `clove ready --format json` | validates against v1 list schema |
| V-G04 | `clove blocked --format json` | validates against v1 list schema |
| V-G05 | `clove dep tree proj-7af --full` | Unicode tree format, depth-limited |
| V-G06 | `clove dep tree proj-7af --flat --format json` | flat array with depth field |
| V-G07 | `clove dep cycle --format json` | data.cycles non-empty (fixture has cycle), exit 0 |
| V-G08 | `clove dep cycle --fail-on-cycle` | exit 3 |
| V-G09 | `clove ls --status open` | human format |
| V-G10 | `clove ls --format json` | validates against v1 list schema; _meta.total present |
| V-G11 | `clove ls --format jsonl` | each line is valid standalone JSON |
| V-G12 | `clove version --format json` | data.schema == 1 |
| V-G13 | `clove show nonexistent --format json` | exit 2; error.code == ITEM_NOT_FOUND |
| V-G14 | `clove new "Test" --format json` | exit 0; data.id and data.path present |
| V-G15 | `clove ready` (empty repo) | exit 0, data == [] |
| V-G16 | `clove agent-doc --format markdown` | contains schema version; idempotent (byte-identical on second run) |
| V-G17 | `clove comments proj-7af --format json` | validates against comment list schema |

**Schema conformance gate:** Every JSON-producing command has its output validated against
`docs/json-schema/v1/<schema>.json` using the `jsonschema` crate. Schema violations are test
failures.

**Exit code table test (V-G18):**
Parameterized test asserting exact integer exit codes:

| Command | Expected exit |
|---|---|
| `clove show nonexistent` | 2 |
| `clove new --priority 9` | 4 |
| `clove show` (no `.clove/`) | 5 |
| `clove dep cycle --fail-on-cycle` (cycle present) | 3 |
| `clove dep cycle` (cycle present, no flag) | 0 |
| `clove ready` (empty result) | 0 |

---

## 6. Property-Based Tests (proptest)

**V-P01: Frontmatter round-trip**
```
Strategy: generate arbitrary ItemFrontmatter
  (strings filtered to avoid YAML-special chars initially; expand in phase 2)
For each generated item:
  serialize via FrontmatterWriter
  parse via FrontmatterParser
  assert structural equality (all fields match)
Also assert: output is byte-identical on two consecutive writes (serializer idempotency)
Run 10,000 iterations
```

**V-P02: DAG invariants**
```
Strategy: generate random DAGs as Vec<(usize, usize)> edge pairs on N nodes (N ≤ 50)
  filtered to remove self-loops
Build GraphStore
Assert:
  has_cycle() == true iff any SCC has size > 1
  toposort() returns Ok iff !has_cycle()
  ready_items() is always a subset of open items
  blocked_items() is always a subset of open items
  ready_items() ∩ blocked_items() == {}
  no item in ready has an open DependsOn neighbor
  no item in blocked has all deps closed AND no dangling deps
```

**V-P03: ID uniqueness under concurrency (proptest)**
```
Strategy: generate (thread_count: 1..=100, ids_per_thread: 1..=200) pairs
Spawn threads, collect all generated IDs
Assert: global uniqueness (zero duplicates)
```

**V-P04: Frontmatter round-trip with Unicode**
```
Same as V-P01 but allow arbitrary Unicode strings in title, body, assignee, labels
Assert: no panics and structural equality after round-trip
This covers Chinese, Arabic, emoji, and YAML-special chars (`:`, `#`, `[`, `]`, `{`, `}`)
```

**V-P05: File/index consistency**
```
Strategy: generate random sequences of valid write operations (create, update, status, dep add/rm)
Apply to file store
After each operation: query both file-scan and index paths
Assert: results identical
Also assert: after any sequence, `dep cycle` result is consistent with the in-memory graph
```

**V-P06: Serializer determinism**
```
Strategy: generate arbitrary ItemFrontmatter
Serialize twice independently
Assert: byte-identical output
This is the regression guard for library upgrade surprises
```

---

## 7. Fuzz Targets

### 7.1 `parse_item_file`

**Target file:** `fuzz/fuzz_targets/parse_item_file.rs`

```rust
fuzz_target!(|data: &[u8]| {
    // Must never panic. Ok or Err both acceptable.
    let _ = parse_item_bytes(data, &dummy_id);
});
```

**Seed corpus** (committed to `fuzz/corpus/parse_item_file/`):
- Empty file
- File with no frontmatter (no `---`)
- File with valid frontmatter, empty body
- File with YAML anchor (`&a`) in value
- File with YAML alias (`*a`)
- File with deeply nested YAML (100 levels)
- File with binary garbage in frontmatter
- File truncated mid-frontmatter
- File with Unicode in every string field
- File with NUL bytes in body
- File with YAML sexagesimal (`60:00`)
- File with Norway problem (`NO` as string)
- File with very long title (1 MB)
- File where `id` doesn't match the expected ID

**CI run:** `cargo fuzz run parse_item_file -- -max_total_time=30` (30 seconds).
**Pre-release run:** 24 hours minimum.

**Memory limit:** 128 MiB process limit via fuzz harness `rss_limit_mb=128` to catch
unbounded allocation (YAML bomb guard).

**Acceptance criteria:** zero panics on entire corpus; OOM results in error return, not crash.

### 7.2 `parse_dep_list`

**Target file:** `fuzz/fuzz_targets/parse_dep_list.rs`

```rust
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_dep_list_yaml(s);
    }
});
```

**Seed corpus:** valid dep lists, empty lists, lists with invalid IDs, binary garbage strings.

---

## 8. Performance Benchmarks and Gates

All criterion benchmarks are in `benches/`. CI runs `cargo bench --no-run` (compile check).
**Gate assertions** are separate `#[test]` functions (named with a `gate_` prefix) using
`std::time::Instant` that fail if measurements exceed the gate. These run in standard
`cargo test --workspace` via the `benchmark-gates` CI job (see §9); no `#[ignore]` is used.
Gate tests use the in-memory fixture generator so they complete in well under 1 second each.
Criterion plots are for developer use only (run manually with `cargo bench`).

### 8.1 Criterion Benchmarks

| Benchmark | Fixture | Gate (p50, CI 3× budget) |
|---|---|---|
| `bench_parse_1000_sequential` | 1k items in memory | mean < 5ms |
| `bench_scan_1000_warm` | 1k item files, OS cache warm | mean < 15ms |
| `bench_ready_1000_no_index` | 1k items + graph | mean < 25ms |
| `bench_ls_10000_index` | 10k items, SQLite | mean < 10ms |
| `bench_ready_10000_index` | 10k items, SQLite | mean < 10ms |
| `bench_search_10000_fts5` | 10k items, SQLite, FTS5 | mean < 20ms |
| `bench_new_single` | empty repo | mean < 10ms |
| `bench_reindex_1000` | 1k items | mean < 500ms |
| `bench_reindex_10000` | 10k items | mean < 1000ms |
| `bench_cycle_detect_1000` | 1k nodes, 2k edges, no cycle | mean < 5ms |
| `bench_staleness_check_10k` | 10k items, 0 stale | mean < 5ms |
| `bench_staleness_check_10k_100stale` | 10k items, 100 stale (hash check) | mean < 20ms |

**Gate test example:**
```rust
#[test]
// No #[ignore] — gate tests run in standard cargo test on CI via the benchmark-gates job.
// Fixtures are in-memory (no disk I/O) so each gate test completes in < 200ms.
fn gate_ls_1000_no_index() {
    let dir = create_in_memory_fixture(1000);
    let start = std::time::Instant::now();
    run_clove_ls_no_index(&dir);
    assert!(start.elapsed() < Duration::from_millis(50),
        "clove ls 1k items (no index) exceeded 50ms gate");
}
```

### 8.2 Startup Cost Test

```
Run `clove version` 100 times via assert_cmd
Assert: no single run exceeds 20ms (4× the 5ms target; conservative for CI flakiness tolerance)
```

### 8.3 Memory Test

```
Run `clove ls` on 10k-item fixture under /usr/bin/time -l (macOS) or /usr/bin/time -v (Linux)
Capture peak RSS
Assert: peak RSS < 60 MB
```

### 8.4 Comparative Benchmark (Pre-Release, Manual)

`cargo xtask bench-compare` runs:
```sh
hyperfine \
  --warmup 3 --runs 20 \
  --export-json docs/benchmarks/$(clove version).json \
  --export-markdown docs/benchmarks/$(clove version).md \
  'clove ls --format json'  \
  'clove ls --format json --no-index'  \
  'tk ls 2>/dev/null || echo "tk not installed"' \
  'bd ls --json 2>/dev/null || echo "bd not installed"'
```

Fixture: 1,000-item repo in `/tmp/clove-bench` (created by `cargo xtask bench-fixtures`).

**CI gate:** the script exits non-zero if any clove measurement exceeds the DESIGN.md §13.1
targets. `tk`/`bd` comparisons are informational only (they may not be installed in CI).

**Expected comparative results (measured, not estimated):**
- `clove ls` scan-mode p50 < 50ms (cold), < 15ms (warm)
- `bd ls` p50 ≥ 55ms cold start (measured Beads baseline) — clove wins at all sizes
- `tk ls` p50 ≥ 500ms (bash subprocess cost at 1k items) — clove wins by 10–100×

---

## 9. CI Matrix

```yaml
# .github/workflows/ci.yml (abridged)
jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        rust: [stable]
        include:
          - os: ubuntu-latest
            rust: "1.80.0"    # MSRV check
    steps:
      - cargo build --workspace
      - cargo test --workspace
      - cargo clippy --workspace -- -D warnings
      - cargo fmt --check
      - cargo deny check    # license + CVE audit

  windows-api-check:
    runs-on: ubuntu-latest
    steps:
      - cargo check --target x86_64-pc-windows-msvc
      # Catches Windows API usage errors without needing a Windows runner for every PR

  fuzz-corpus-regression:
    runs-on: ubuntu-latest
    steps:
      - cargo install cargo-fuzz
      - cargo fuzz run parse_item_file -- -max_total_time=30
      - cargo fuzz run parse_dep_list -- -max_total_time=30

  schema-validation:
    runs-on: ubuntu-latest
    steps:
      - cargo test -- --include-ignored golden_cli_  # runs all V-G* tests

  benchmark-gates:
    runs-on: ubuntu-latest
    steps:
      # Gate tests use std::time::Instant (not criterion) and have no #[ignore] — they run
      # in standard `cargo test`. The in-memory fixture generator keeps them fast enough for CI.
      - cargo test --workspace -- gate_  # runs all gate_* test functions (M0-G09, M0-G10, etc.)
      # Note: full criterion benchmarks (plots, history) are run on developer machines and
      # before releases via `cargo bench`. Gate tests are the CI-enforceable subset.

  daemon-windows:
    # Added for M3: verify daemon shutdown works on Windows via named event mechanism
    runs-on: windows-latest
    steps:
      - cargo test -p cloved -- daemon_  # runs all daemon_* integration tests including Windows shutdown

  weekly-fuzz:
    schedule: "0 2 * * 0"    # weekly
    runs-on: ubuntu-latest
    steps:
      - cargo fuzz run parse_item_file -- -max_total_time=3600
```

**Windows-specific tests that must pass on `windows-latest`:**
- V-I01 (full round-trip, path separator test)
- V-I07 (init idempotency + LF line endings in .gitignore)
- V-I03 (concurrent write with Windows rename-over-locked-file retry)
- V-U21 (path traversal — Windows backslash variants)
- T-D02 daemon Windows shutdown test (named event + clean exit) — M3 gate M3-G03 must pass on `windows-latest`

---

## 10. Milestone Acceptance Gates

### M0 Gates

Every gate must be green before tagging M0.

| Gate | Test | Pass Condition |
|---|---|---|
| M0-G01 | All unit tests | `cargo test -p clove-core` exits 0 on all 3 CI platforms |
| M0-G02 | Integration tests (file store) | `cargo test -p clove-core --test integration` exits 0 |
| M0-G03 | Golden CLI snapshots | All V-G* tests pass |
| M0-G04 | JSON schema validation | Every command's JSON output validates against v1 schema |
| M0-G05 | Exit code correctness | V-G18 parameterized test passes |
| M0-G06 | Merge simulation (no driver) | V-I13 passes (parallel-branch new items, no merge driver needed) |
| M0-G07 | Cycle detection | V-U03 all variants pass |
| M0-G08 | Soft relations do not block | V-U05 passes |
| M0-G09 | Scan performance gate | `clove ls` 1k items (no index) < 50ms (CI gate test) |
| M0-G10 | Ready performance gate | `clove ready` 1k items (no index) < 80ms |
| M0-G11 | Startup gate | `clove version` < 20ms x 100 iterations |
| M0-G12 | Fuzz (30s) | Zero panics on committed corpus |
| M0-G13 | ID uniqueness | V-U18, V-U19 pass |
| M0-G14 | ID traversal safety | V-U21 passes on all 3 platforms |
| M0-G15 | Atomic write safety | V-I02 (1000 crash simulations, zero corrupt files) |
| M0-G16 | Comment conflict-free merge | V-I11 passes |
| M0-G17 | CLOVE_FORMAT env var | `CLOVE_FORMAT=json clove ls` produces valid JSON |
| M0-G18 | Windows CI | All M0 tests pass on `windows-latest` |
| M0-G19 | agent-doc | V-G16 passes (idempotent output, schema version embedded) |

**PRD §15 claims mapped:**
- "Dependency queries correct and instant" → M0-G07, M0-G08, M0-G09, M0-G10
- "Everything works with only binary + files" → M0-G01 through M0-G16 (no SQLite needed)
- "Agent-first ergonomics" → M0-G03, M0-G04, M0-G05, M0-G17, M0-G19

---

### M1 Gates

| Gate | Test | Pass Condition |
|---|---|---|
| M1-G01 | File/index consistency | V-S01 (property test, 200 mutations) passes |
| M1-G02 | Staleness detection | V-S02 (cp -p test) passes |
| M1-G03 | git checkout consistency | V-S03 passes |
| M1-G04 | Concurrent write safety | V-S04 (10 parallel creates) passes |
| M1-G05 | Index absent fallback | V-S05 passes |
| M1-G06 | Schema migration | V-S06 passes |
| M1-G07 | Reindex re-entrance | V-S08 passes |
| M1-G08 | FTS5 consistency | V-S09 passes |
| M1-G09 | Deletion detection | V-S10 passes |
| M1-G10 | Index query performance | `clove ls` 10k items < 10ms (criterion gate) |
| M1-G11 | Search performance | `clove search` 10k items < 20ms (criterion gate) |
| M1-G12 | Reindex performance | `clove reindex` 10k items < 1000ms |
| M1-G13 | Staleness check performance | V-S01-related: 10k items, 0 stale < 5ms |
| M1-G14 | All M0 gates still pass | Full M0 gate suite re-runs and passes |

**PRD §10 performance targets mapped:**
- "With index, ~10,000 items: queries under ~10ms" → M1-G10, M1-G11

---

### M2 Gates

| Gate | Test | Pass Condition |
|---|---|---|
| M2-G01 | tk import fixture | T-M01 AC tests pass |
| M2-G02 | Beads import fixture | T-M02 AC tests pass |
| M2-G03 | GitHub roundtrip | T-M03 AC (with GITHUB_TOKEN) |
| M2-G04 | Import dry-run writes nothing | All three importers with --dry-run write zero files |
| M2-G05 | Merge driver — same-value conflict | V-I14 passes (requires merge driver from T-M05) |
| M2-G05a | Merge driver — dep union | V-I15 passes (requires merge driver from T-M05) |
| M2-G05b | Merge driver — dep removal conflict | V-I16 passes (requires merge driver from T-M05) |
| M2-G06 | All M1 gates still pass | Full M1 gate suite re-runs |

---

### M3 Gates — ✅ all passing (see `docs/M3_ACCEPTANCE_GATES.md`)

| Gate | Test | Pass Condition | Status |
|---|---|---|---|
| M3-G01 | Daemon IPC round-trip | PING/PONG < 5ms | ✅ |
| M3-G02 | Daemon startup sweep | 1k items, 50 modified → ready < 500ms | ✅ |
| M3-G03 | Clean shutdown | SIGTERM → no stale sock/pid files, exit 0 | ✅ |
| M3-G04 | Stale socket recovery | Kill with SIGKILL → next `clove ls` < 200ms | ✅ |
| M3-G05 | Feedback loop prevention | No index.db events processed after reindex | ✅ |
| M3-G06 | Debounce batching | 10 chunks × 10ms → exactly 1 SQLite update | ✅ |
| M3-G07 | git auto-sync skip-during-rebase | T-D06 AC passes | ✅ |
| M3-G08 | Two daemons prevention | Second daemon exits non-zero | ✅ |
| M3-G09 | All M2 gates still pass | Full M2 gate suite re-runs | ✅ |
| M3-G10 | `doctor` daemon-health | T-D07 AC: stale sock/pid flagged; `--fix` cleans a dead-daemon footprint; live daemon untouched (added by M3_PLAN.md §1.1 CLI-surface review) | ✅ |

---

## 11. PRD Claim Verification Mapping

The following table maps every PRD §15 success criterion and §10 target to a specific test.

| PRD Claim | Verification |
|---|---|
| "Real repo adopts clove, agents drive via ready/status" | M0 integration test suite; golden_repo fixture |
| "Dependency queries correct and instant" | V-U01–V-U11, M0-G07–M0-G10 |
| "Benchmarks beat tk and Beads on §10 operations" | Comparative benchmark (§8.4); hyperfine table |
| "clove ls 1k items < 100ms (no index)" | M0-G09 (gate at 50ms for headroom) |
| "clove ls 10k items < 10ms (with index)" | M1-G10 |
| "new/show/status feel instant" | Startup gate (V-G12, M0-G11); M0-G09 |
| "JSON schema stable and documented" | V-G02–V-G17 schema validation; docs/json-schema/v1/ |
| "Everything works without index/daemon" | M0 entire gate suite (no SQLite); V-S05 fallback test |
| "No lock-in; import/export trivial" | M2-G01–M2-G04 |
| "Merge-friendly by construction" | V-I13 (M0, no driver needed); V-I11 (M0, comments); V-I14–V-I16 (M2, requires merge driver) |
| "Cross-platform (Windows)" | M0-G18; Windows-specific CI tests |
| "Agent-first: stable exit codes" | V-G18 exit code table test |
| "Agent-first: CLOVE_FORMAT env var" | M0-G17 |
| "Collision-resistant IDs across parallel branches" | V-U18–V-U20; V-I13 |
| "Files are truth; deleting index loses nothing" | V-S05 (fallback test); V-I01 (file round-trip) |

---

## 12. Schema Stability Policy (Documented Contract)

**Breaking changes (require bumping `v` in envelope):**
- Removing a field from the JSON output.
- Renaming a field.
- Changing a field's type.
- Changing the semantics of a field's values.

**Non-breaking changes (no `v` bump):**
- Adding a new optional field (agents using `#[deny(unknown_fields)]` style parsing should
  handle this; documented in agent-doc).
- Adding new values to an enum field that agents are expected to handle gracefully.

**Deprecation cycle:** deprecated fields appear with a `_deprecated: true` sibling flag for
one minor release before removal. The `_meta.warnings` array announces deprecations.

**Enforcement:** the schema stability test (V-G02 + committed golden file) runs on every CI
push and fails on any output change, forcing a deliberate review.

**Published machine-readable schema:** `docs/json-schema/v1/` contains JSON Schema 2020-12
files for all response types. These are the authoritative contract documents for agent
framework authors.
