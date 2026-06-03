# clove — Implementation Plan

> **Status:** Authoritative task plan. Cross-references DESIGN.md (§) and VERIFICATION_PLAN.md (V-*).
> Tasks are ordered so each is independently buildable/testable. Dependencies are listed
> explicitly. M0 is strictly file-only — no SQLite, no daemon.

---

## Milestone Overview

| Milestone | Description | Exit Criterion |
|---|---|---|
| M0 | File-only MVP | All file-store commands work; JSON schema validates; 1k-item scan < 100 ms |
| M1 | SQLite index | `reindex` + incremental refresh; `search`; lean list queries < 15 ms, search < 20 ms at 10k items |
| M2 | Interop | `import beads|tk|github`; `export json|jsonl|github` |
| M3 | Daemon | `cloved` file-watcher + IPC; incremental index; optional git auto-sync |
| M4 | Extras | TUI/web UI; bidirectional vendor bridges; richer changelog |

---

## M0 — File-Only MVP

### Infrastructure

---

**T-I01: Cargo workspace skeleton**
- Files: `Cargo.toml` (workspace root), `crates/clove-core/Cargo.toml`, `crates/clove-index/Cargo.toml`, `crates/clove-import/Cargo.toml`, `crates/clove/Cargo.toml`, `crates/cloved/Cargo.toml`
- Deps: none
- Description: Create the workspace with all five member crates. Set `edition = "2021"`, `rust-version = "1.80.0"`. Add `[workspace.dependencies]` with pinned versions for all shared deps as listed in DESIGN.md §1. Each crate's `Cargo.toml` uses `dep.workspace = true`. Add an `.editorconfig` and a `.gitignore` for the workspace root.
- AC: `cargo build --workspace` succeeds with empty crates. `cargo check --target x86_64-pc-windows-msvc` passes (from macOS/Linux).

---

**T-I02: CI workflow**
- Files: `.github/workflows/ci.yml`
- Deps: T-I01
- Description: Define four CI jobs: (1) `ubuntu-latest` + stable Rust: `cargo test --workspace`, `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo deny check`; (2) `macos-latest` + stable Rust: same; (3) `windows-latest` + stable Rust: same; (4) `ubuntu-latest` MSRV (1.80): `cargo build --workspace`. Add `cargo-deny` config (`deny.toml`) allowing only MIT/Apache-2.0/BSD/ISC licenses.
- AC: All CI jobs pass on a green workspace.

---

**T-I03: Limits and constants**
- Files: `crates/clove-core/src/limits.rs`
- Deps: T-I01
- Description: Define all named constants from DESIGN.md §4: `MAX_FRONTMATTER_BYTES`, `MAX_BODY_BYTES`, `MAX_DEP_ARRAY_LEN`, `MAX_ID_LEN`, `MAX_PREFIX_LEN`, `MAX_ITEMS_NO_INDEX_WARN`. Export as `pub const`. Add unit tests asserting the values are within sane ranges (e.g. `MAX_FRONTMATTER_BYTES < MAX_BODY_BYTES`).
- AC: Constants compile; unit tests pass.

---

**T-I04: CloveId newtype**
- Files: `crates/clove-core/src/id.rs`
- Deps: T-I03
- Description: Implement `CloveId(SmolStr)` newtype. `CloveId::new(s: &str) -> Result<CloveId, CloveError>` validates against `^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$`. `CloveId::to_path(root: &Utf8Path) -> Result<Utf8PathBuf, CloveError>` constructs path, calls `std::fs::canonicalize` on the parent directory (not the file), asserts result starts with canonical `.clove/issues/` root, returns `CloveError::PathTraversal` if not. Implement `Display`, `From<CloveId> for String`, `serde::Deserialize`, `serde::Serialize`. Add unit tests: valid IDs, IDs with traversal chars (`../`, `..`), IDs with null bytes, IDs that are too long.
- AC: Path traversal test asserts no escape regardless of input string. ID regex rejects all path-unsafe chars.

---

**T-I05: ID generator**
- Files: `crates/clove-core/src/id.rs`
- Deps: T-I04
- Description: Implement `fn generate_id(prefix: &str) -> CloveId` using `getrandom::fill` for 5 random bytes, encoded to 8 Crockford base32 uppercase chars using a lookup table (no external crate needed for 8 chars). Implement `fn new_id(prefix: &str, issues_dir: &Utf8Path) -> Result<CloveId, CloveError>` that calls `generate_id`, checks file existence via `stat()` (not open), retries up to 3 times, returns `CloveError::IdConflict` after exhausting retries. Add unit test: generate 100,000 IDs in a loop, insert into HashSet, assert zero collisions. Add concurrent test: 50 threads each generating 200 IDs, assert global uniqueness.
- AC: Uniqueness tests pass. ID format matches the regex from T-I04.

---

**T-I06: Repo root discovery**
- Files: `crates/clove-core/src/repo.rs`
- Deps: T-I04
- Description: Implement `fn find_repo_root(start: &Utf8Path) -> Option<Utf8PathBuf>` that walks ancestor directories looking for a `.clove/` subdirectory. Returns the repo root (the directory containing `.clove/`). Also implement `fn find_issues_dir(start: &Utf8Path) -> Option<Utf8PathBuf>` returning `.clove/issues/`. For worktree awareness: detect if inside a git worktree by checking for `.git` file (not directory); if found, use `git2` crate to resolve the main worktree's root. Unit tests: (a) cwd is repo root, (b) cwd is subdirectory, (c) cwd has no `.clove/` ancestor, (d) linked worktree resolves to main worktree.
- AC: Tests pass on all three CI platforms (path separator differences).

---

### Data Model

---

**T-C01: Item model structs**
- Files: `crates/clove-core/src/model.rs`
- Deps: T-I04
- Description: Define `Item`, `ItemFrontmatter`, `ItemStatus`, `ItemType`, `Priority` as specified in DESIGN.md §2.3. Use `#[serde(deny_unknown_fields)]` on `ItemFrontmatter`. Use `chrono::DateTime<Utc>` for timestamps. `Priority` is a newtype over `u8` with validation (0–4). `ItemStatus` is an enum with a custom `Serialize`/`Deserialize` that handles the `status: closed` + `closed: <ts>` split/merge on the YAML side. `ItemType` serializes as `"type"` field name (rename). Use `#[serde(default)]` (no `skip_serializing_if`) for all `Vec` fields so that serde-derived JSON serialization produces `[]` for empty arrays, consistent with the JSON schema (§7.4). YAML serialization is governed by `FrontmatterWriter` (not serde), which applies the empty-list omit rule from §2.2 independently. Also implement `pub fn normalize_label(raw: &str) -> Result<String, CloveError>` per DESIGN.md §2.2 (Unicode lowercase via `to_lowercase`, trim, collapse internal whitespace to a single space, reject empty) — the single canonicalization point used by item construction, label edits, filters, and importers.
- AC: All types compile. `serde::Serialize` derive works. Custom `ItemStatus` round-trip test: `Closed { at }` serializes to `status: closed` + `closed: <ts>` and deserializes back identically. JSON serialization test: `serde_json::to_value` on an `ItemFrontmatter` with empty `deps` produces `"deps": []` (not an absent field). `normalize_label` test: `"Area:iOS"`, `"  AREA:IOS  "`, `"area:ios"` all map to `"area:ios"`; `"   "` is rejected.

---

**T-C02: FrontmatterWriter (serializer)**
- Files: `crates/clove-core/src/write.rs`
- Deps: T-C01, T-I03
- Description: Implement `FrontmatterWriter<W: Write>` with `fn write_item(&mut self, fm: &ItemFrontmatter) -> io::Result<()>` that writes fields in the exact canonical order from DESIGN.md §2.2 using explicit `write!()` calls. List fields: always inline flow `[a, b, c]` with elements sorted lexicographically. Empty lists: `[]`. Null optionals: `null`. Non-null optionals: written with no null indicator. The `status`/`closed` split: if status is `Closed { at }`, write `status: closed\nclosed: <at>\n`. Implement `fn write_item_file(item: &Item, path: &Utf8Path) -> Result<(), CloveError>` using `tempfile::NamedTempFile::new_in(parent)`, `BufWriter`, `fsync`, `persist()` (atomic rename). Golden-file test: construct a fully-populated `ItemFrontmatter`, call `write_item`, byte-compare output to a committed fixture file at `tests/fixtures/full_item.md`.
- AC: Golden-file test passes. Output is byte-identical on two consecutive writes (idempotency test). Round-trip test: write + parse = structural equality.

---

**T-C03: FrontmatterParser (deserializer)**
- Files: `crates/clove-core/src/parse.rs`
- Deps: T-C01, T-I03
- Description: Implement `fn parse_item_file(path: &Utf8Path) -> Result<Item, CloveError>` that: (1) reads file bytes; (2) checks frontmatter byte budget (returns `ParseError::FrontmatterTooLarge` if > `MAX_FRONTMATTER_BYTES`); (3) scans for YAML anchors/aliases (returns `ParseError::AliasNotAllowed` if found); (4) uses `memchr::memmem::find` to locate closing `---\n`; (5) deserializes via `serde_yaml_neo` (crate name: `serde_yaml_neo = "0.9"` in workspace deps — see DESIGN.md §1 and §14.2) into `ItemFrontmatter` with `#[serde(deny_unknown_fields)]`; (6) validates `id` matches filename stem; (7) validates all fields via `validate_item()`; (8) stores body bytes as `String`. Also implement `parse_item_bytes(bytes: &[u8], expected_id: &CloveId) -> Result<Item, CloveError>` for cases where bytes are already loaded. Missing `schema` field → treat as `schema: 1`.
- AC: Valid item round-trips. Invalid YAML returns typed error with file path. Alias input returns `AliasNotAllowed`. Filename mismatch returns `IdMismatch`. Oversized frontmatter returns `FrontmatterTooLarge`.

---

**T-C04: Item validation**
- Files: `crates/clove-core/src/validate.rs`
- Deps: T-C01, T-I04
- Description: Implement `fn validate_item(fm: &ItemFrontmatter) -> Vec<ValidationError>` checking: (1) priority 0–4; (2) all dep/parent/relates IDs match `CloveId` format; (3) timestamps are valid RFC3339; (4) `Closed { at }` ↔ `status == closed` invariant; (5) schema version is supported (currently 1); (6) array lengths ≤ `MAX_DEP_ARRAY_LEN`. Add validation rejection tests: one test per rule asserting non-empty `Vec<ValidationError>` with a message containing the field name.
- AC: One test per validation rule.

---

**T-C05: ItemStore (file store)**
- Files: `crates/clove-core/src/store.rs`
- Deps: T-C02, T-C03, T-I05, T-I06
- Description: Implement `ItemStore { repo_root: Utf8PathBuf, issues_dir: Utf8PathBuf }` with methods:
  - `fn create(title, type, priority, ...) -> Result<Item, CloveError>`: generate ID, build `ItemFrontmatter`, write file, return.
  - `fn get(id: &CloveId) -> Result<Item, CloveError>`: reads `<id>.md`, parses.
  - `fn update(item: &Item) -> Result<Item, CloveError>`: validates, updates `updated` timestamp, writes atomically with file lock.
  - `fn delete(id: &CloveId, force: bool) -> Result<(), CloveError>`: checks for items that dep on this ID (returns `CloveError::HasDependents` unless force=true), removes `<id>.md` file, then removes the sibling `<id>/` directory and all its contents (comments) if present using `std::fs::remove_dir_all`.
  - `fn list() -> Result<Vec<Item>, CloveError>`: scans directory, skips symlinks and non-`.md` files, yields `ScanError::ParseFailed { path, error }` per-item (not abort), skips `*.tmp` files. Uses `rayon::par_iter()` if item count > 500.
  - `fn scan_lazy(issues_dir) -> impl Iterator<Item=Result<ItemFrontmatter, ScanError>>`: parses only frontmatter (not body) for `ls`/`ready`/`blocked` operations.
- AC: Full round-trip integration test: `create` → `get` → `update` → `list` (verifies all fields, verifies `updated` changed). Scan skips symlinks test. Concurrent write test: 10 threads each creating distinct items → all 10 files valid. Delete-with-comments test: create item, add a comment (causing `<id>/comments/` directory to be created), call `delete`, assert both `<id>.md` and `<id>/` directory are absent from disk.

---

**T-C06: Comment store**
- Files: `crates/clove-core/src/comments.rs`
- Deps: T-I04, T-I06
- Description: Implement `fn add_comment(issues_dir: &Utf8Path, id: &CloveId, author_email: &str, body: &str) -> Result<Utf8PathBuf, CloveError>`. Author slug derivation from email: lowercase, non-alphanumeric → `-`, truncate at 32 chars. Comment filename: `<rfc3339nano>-<author-slug>-<4char-random>.md` using `jiff` for nanosecond timestamp. If target file exists (same nanosecond, same author): regenerate the 4-char random suffix up to 5 times, then return error. Write body as plain Markdown (no frontmatter). Implement `fn list_comments(issues_dir: &Utf8Path, id: &CloveId) -> Result<Vec<Comment>, CloveError>` that reads all files in `<id>/comments/`, parses timestamp and author from filename, returns sorted chronologically. `Comment { timestamp: jiff::Timestamp, author: String, body: String }`.
- AC: Add-then-list round-trip. Two comments in same millisecond produce distinct files. Merge simulation: two branches each adding a comment, merge, assert no conflict, both comments present in list.

---

### Dependency Graph

---

**T-G01: EdgeKind and GraphStore**
- Files: `crates/clove-core/src/graph.rs`
- Deps: T-C01, T-I04
- Description: Define `EdgeKind` enum as specified in DESIGN.md §5.1. Implement `GraphStore { graph: StableDiGraph<ItemMeta, EdgeKind>, id_to_node: HashMap<CloveId, NodeIndex>, node_to_id: Vec<CloveId>, dangling_ids: HashSet<CloveId> }`. Implement `GraphStore::build(items: &[Item]) -> (GraphStore, Vec<DanglingRef>)`: two-pass construction (all IDs → nodes first, then edges). Collect unreferenced dep targets into `dangling_ids`. Implement `fn is_hard_dep(e: EdgeKind) -> bool`.
- AC: Unit test: build from 10-item fixture with known dep structure; assert node count and edge count correct. Dangling dep test: item X with dep on missing-id → `dangling_ids` contains missing-id.

---

**T-G02: Ready and blocked queries**
- Files: `crates/clove-core/src/graph.rs`
- Deps: T-G01
- Description: Implement `GraphStore::ready_items(&self) -> Vec<CloveId>`: call `petgraph::algo::toposort`; if cycle error, fall through to SCC reporting; filter items by status open/in_progress where all `DependsOn` neighbors (filtered by `is_hard_dep`) have status closed AND `has_dangling_deps == false`. Implement `GraphStore::blocked_items(&self) -> Vec<BlockedItem>` where `BlockedItem { id, blocking_deps, dangling_deps }`. Implement topological sort for output ordering. **Partition completeness invariant:** assert `ready ∪ blocked ∪ closed == all_items` in debug builds.
- AC: Tests from VERIFICATION_PLAN.md V-U01 and V-U02. Soft-relations-do-not-block test (V-U05). Dangling deps test (V-U04).

---

**T-G03: Cycle detection**
- Files: `crates/clove-core/src/graph.rs`
- Deps: T-G01
- Description: Implement `GraphStore::check_would_cycle(&self, from: &CloveId, to: &CloveId) -> bool` using `petgraph::algo::has_path_connecting(&self.graph, to_node, from_node, None)`. Implement `GraphStore::all_cycles(&self) -> Vec<Vec<CloveId>>` using `petgraph::algo::kosaraju_scc`, filtering SCCs with len > 1. Implement `GraphStore::has_any_cycle(&self) -> bool`. Detect ParentOf cycles separately and mark items with circular parentage as `malformed_parent: true` in `ItemMeta`.
- AC: Tests from VERIFICATION_PLAN.md V-U03. Self-loop rejection. 3-node cycle detection. DAG with no cycle returns empty.

---

**T-G04: Dep tree rendering**
- Files: `crates/clove-core/src/graph.rs`
- Deps: T-G01
- Description: Implement `GraphStore::dep_tree(&self, root: &CloveId, max_depth: usize) -> DepTreeNode` with depth-bounded DFS. `DepTreeNode { id, title, status, ready, cycle_ref, children }`. Track visited IDs to detect cycles; set `cycle_ref: true` without recursing. Implement `fn render_dep_tree_human(node: &DepTreeNode, prefix: &str, is_last: bool) -> String` for Unicode tree (cargo-tree style). Depth-limit test: 20-item chain, max_depth=5, assert A6 does not appear. Cycle-marker test: A→B→C→A with depth 100 does not infinite-loop.
- AC: Depth limit test. Cycle marker test. Human render matches expected indentation format (snapshot test).

---

**T-G05: Epic roll-up**
- Files: `crates/clove-core/src/graph.rs`
- Deps: T-G01
- Description: Implement `GraphStore::epic_children_summary(&self, epic_id: &CloveId) -> Option<ChildrenSummary>` where `ChildrenSummary { total: u32, closed: u32 }`. Iterates direct `ParentOf` edges only (not recursive). Returns `None` for non-epic items. Derives `completable: bool` field (all children closed).
- AC: Unit test: 3-child epic (2 closed, 1 open) → `{total:3, closed:2}`. All children closed → `completable: true`.

---

### Configuration

---

**T-CF01: CloveConfig**
- Files: `crates/clove-core/src/config.rs`
- Deps: T-I04, T-I06
- Description: Define `CloveConfig { id_prefix: String, id_length: u8, default_type: ItemType, default_format: OutputFormat, index: IndexConfig, daemon: DaemonConfig }` with serde `Deserialize` and `Default`. Implement `fn load_config(repo_root: &Utf8Path) -> Result<CloveConfig, CloveError>` that reads `.clove/config.toml` if present, falls back to `Default`, then applies `CLOVE_*` env var overrides (e.g. `CLOVE_FORMAT`, `CLOVE_ID_PREFIX`). Validate `id_prefix` against `^[a-z][a-z0-9]{0,7}$`, `id_length` ∈ [4, 12]. Validate `config.toml` is not a symlink pointing outside the repo. Precedence test: flag > CLOVE_FORMAT > config.toml > default.
- AC: Validation rejection tests for all invalid prefix/length values. Precedence test.

---

### CLI — M0 Commands

---

**T-CLI01: CLI entry point and global flags**
- Files: `crates/clove/src/main.rs`, `crates/clove/src/cli.rs`
- Deps: T-CF01, T-I06
- Description: Implement top-level `clap` parser with global persistent flags: `--format/-f`, `--no-index`, `--quiet`, `--color`, `--clove-dir`. Parse `CLOVE_FORMAT` env var before clap argument parsing. Define `ExitCode` enum (0–7) as specified in DESIGN.md §7.6. Override clap's default exit code behavior. Implement `fn output<T: Serialize>(format: Format, result: Result<T, CloveError>)` that emits either the JSON envelope or the human-readable form to stdout. All errors emit JSON on stdout on `--format json`; human text goes to stderr.
- AC: `CLOVE_FORMAT=json clove ls` produces JSON without `--format json` flag. Error envelope test: `clove show nonexistent --format json` produces `{ "v":1, "ok":false, "error": { "code": "ITEM_NOT_FOUND", "exit": 2 } }` on stdout.

---

**T-CLI02: `clove init`**
- Files: `crates/clove/src/cmd/init.rs`
- Deps: T-CLI01, T-CF01
- Description: Create `.clove/`, `.clove/issues/`, `.clove/config.toml` (with defaults), `.clove/.gitignore` (contents: `index.db`, `*.db-shm`, `*.db-wal`, `daemon.sock`, `daemon.pid`, `reindex.lock`, `daemon.lock`, `index.db.tmp`). Idempotent: running twice is safe (does not overwrite existing `config.toml`). `--merge-driver` flag: appends to `.gitattributes` and writes `[merge "clove-item"]` stanza to `.git/config`. `--prefix STR` override. Prints one-line hint: `run 'clove agent-doc' to generate an AGENTS.md snippet`. `.gitignore` must use LF line endings on all platforms.
- AC: Idempotency test (run twice, same output). Merge-driver flag writes correct content. `.gitignore` LF test on Windows CI. `.gitignore` contents test: assert the written file contains exactly the 8 expected entries (`index.db`, `*.db-shm`, `*.db-wal`, `daemon.sock`, `daemon.pid`, `reindex.lock`, `daemon.lock`, `index.db.tmp`).

---

**T-CLI03: `clove new`**
- Files: `crates/clove/src/cmd/new.rs`
- Deps: T-CLI01, T-C05
- Description: Parse title + all optional flags. Call `ItemStore::create()`. JSON output: `{ "v":1, "ok":true, "data":{ "id":"...", "path":".clove/issues/<id>.md" } }`. Human output: prints ID and relative path. Exit 4 on validation error.
- AC: Golden output snapshot. `--format json` output validates against JSON Schema v1.

---

**T-CLI04: `clove show`**
- Files: `crates/clove/src/cmd/show.rs`
- Deps: T-CLI01, T-C05, T-C06, T-G01
- Description: Fetch item, load comments for `comment_count`, serialize to `ItemJson`. Two performance tiers:
  - **Fast path (default for human output and `--fields` that exclude `ready`/`blocked_by`):** reads only `<id>.md` and the `<id>/comments/` directory. No graph construction. `ready` and `blocked_by` are omitted or set to `null` with a `_meta.warning` noting "pass --verbose for ready/blocked_by".
  - **Full path (`--format json`, `--verbose`, or when `--fields` explicitly requests `ready` or `blocked_by`):** scan all items (lazy frontmatter parse), build graph context, compute `ready` and `blocked_by`. Performance target for this path matches `clove ls` 1k/file-scan bound (< 50ms cold). This path is documented in `agent-doc` as the authoritative JSON output.
  Support `--fields` for field projection. Exit 2 if not found. JSON output validates against schema.
- AC: `ready` and `blocked_by` accuracy test: A depends-on open B → A has ready=false, blocked_by=[B]. Close B → A has ready=true, blocked_by=[]. Fast-path test: `clove show <id>` (no flags) on a 1k-item repo does NOT scan all items (assert file_read_count == 1 via mock). Full-path test: `clove show <id> --format json` on a 1k-item repo produces valid `ready` field.

---

**T-CLI05: `clove edit`**
- Files: `crates/clove/src/cmd/edit.rs`
- Deps: T-CLI01, T-C05
- Description: Two modes. Without `--field`: open `$EDITOR` on the item file. With `--field KEY=VALUE`: non-interactive field edit. Supported field keys: `status`, `priority`, `assignee`, `type`, `labels+=val`, `labels-=val`. `clove set <id> <field>=<value>` is an alias for `edit --field`. Multi-field atomic update: reads once, applies all changes, writes once. Update `updated` timestamp.
- AC: Non-interactive multi-field update test: `clove set X status=closed priority=0` produces a valid closed item with correct `closed` timestamp.

---

**T-CLI06: `clove status`, `clove start`, `clove close`**
- Files: `crates/clove/src/cmd/status.rs`
- Deps: T-CLI01, T-C05
- Description: `clove status <id> <open|in_progress|closed>`. `start` and `close` are aliases. When transitioning to `closed`: set `closed` timestamp atomically. When transitioning away from `closed`: clear `closed` field. Enforce status/closed invariant. Warn if item has reverse-deps (items that depend on it) when closing — lists them but does not block.
- AC: Round-trip: create → close → re-read → assert `status==closed` and `closed` is valid RFC3339. Re-open → assert `closed` field absent.

---

**T-CLI07: `clove label`, `clove assign`, `clove priority`**
- Files: `crates/clove/src/cmd/label.rs`, `crates/clove/src/cmd/assign.rs`, `crates/clove/src/cmd/priority.rs`
- Deps: T-CLI01, T-C05
- Description: Standard single-field mutations. `label add` passes the label through `normalize_label` (DESIGN.md §2.2), then appends only if not already present; the stored list is re-sorted/de-duped. `label rm` normalizes its argument before removing so `rm Area:iOS` removes `area:ios`. `assign` sets or clears assignee. `priority N` validates 0–4. All update `updated` timestamp. The `--label` filter in `ls`/`ready`/`blocked`/`query` (T-CLI04/09/10) likewise normalizes its argument before matching.
- AC: adding `area:iOS` then `area:ios` yields a single `area:ios` label; `clove label <id> rm AREA:IOS` removes it; `clove ls --label Area:IOS` matches an item labeled `area:ios`.
- AC: Each command has a golden snapshot. `priority 5` exits 4 (ValidationError).

---

**T-CLI08: `clove dep add`, `clove dep rm`**
- Files: `crates/clove/src/cmd/dep.rs`
- Deps: T-CLI01, T-C05, T-G03
- Description: `dep add <id> <dep-id>` validation pipeline (in order): (1) dep_id exists (else exit 2), (2) dep_id != self_id (else exit 4, code `SELF_LOOP` — a bad argument, not a cycle), (3) `check_would_cycle(self, dep)` (else exit 3, code `CYCLE_DETECTED` with cycle path), (4) not already present (else exit 4, code `ALREADY_EXISTS`). On success: acquire file lock, read item, append to `deps`, sort, write atomically. `dep rm`: acquire lock, read, remove dep, write atomically. Error codes match DESIGN.md §5.4.
- AC: Each of the 4 validation failures produces the correct exit code. Cycle rejection test: A→B→C, attempt C→A, assert exit 3.

---

**T-CLI09: `clove dep tree`, `clove dep cycle`**
- Files: `crates/clove/src/cmd/dep.rs`
- Deps: T-CLI01, T-C05, T-G03, T-G04
- Description: `dep tree <id>`: scan all items, build graph, call `dep_tree(id, depth)`. Default depth 5, `--full` removes limit. `--flat` emits flat array with `depth` field. `--format json` emits nested tree object. `dep cycle`: build full graph, call `all_cycles()`. Always exits 0. `--fail-on-cycle` exits 3 when cycles found.
- AC: Depth limit snapshot test. `dep cycle --fail-on-cycle` exits 3 with cycle present; exits 0 without.

---

**T-CLI10: `clove ready`, `clove blocked`**
- Files: `crates/clove/src/cmd/ready.rs`, `crates/clove/src/cmd/blocked.rs`
- Deps: T-CLI01, T-C05, T-G02
- Description: Scan all items (lazy frontmatter parse), build graph, compute ready/blocked. Apply filters (`--status`, `--type`, `--label`, `--assignee`, `--priority`). Sort by `(priority ASC, topological_rank ASC)`. `--include-warnings` flag to include items with dangling deps. Pagination: `--limit`, `--offset`. `_meta.total` = total unfiltered count. JSON output validates against list schema. Items with dangling deps excluded from ready output by default; their dangling IDs surfaced in stderr warning. `ready` with zero results: exit 0 with empty data array.
- AC: Partition completeness test. Empty result = exit 0. Pagination test: 250 items, limit 100, three pages sum to 250.

---

**T-CLI11: `clove ls`, `clove query`**
- Files: `crates/clove/src/cmd/ls.rs`, `crates/clove/src/cmd/query.rs`
- Deps: T-CLI01, T-C05
- Description: `ls`: scan with optional filters, sort, paginate. `query`: supports `--filter EXPR` flag AND reads JSON filter object from stdin when stdin is non-TTY and no `--filter` flag (jq model). `--fields` flag: field projection on serialized `serde_json::Value`. `--format jsonl`: one item envelope per line.
- AC: `clove ls --format json | jq -e '.ok'` exits 0. `--fields id,status` output contains only those two fields. jsonl format: each line is valid standalone JSON.

---

**T-CLI12: `clove comment`, `clove comments`**
- Files: `crates/clove/src/cmd/comments.rs`
- Deps: T-CLI01, T-C06
- Description: `clove comment <id> <message>`: add a comment. `clove comments <id> [--limit N] [--format json]`. JSON response: `{ "v":1, "ok":true, "data":[{ "author":"...", "timestamp":"...", "body":"..." }] }`. Comments are in M0 (not deferred to M2) because agents need it to avoid direct file globbing.
- AC: Add-then-list round-trip. `--format json` output validates against schema.

---

**T-CLI13: `clove version`**
- Files: `crates/clove/src/cmd/version.rs`
- Deps: T-CLI01
- Description: `clove version` prints `clove X.Y.Z`. `--format json`: `{ "v":1, "ok":true, "data":{ "clove":"0.1.0", "schema":1, "build_date":"...", "git_hash":"..." } }`. Schema field lets agents detect schema version bumps at startup.
- AC: JSON output validates against schema. `schema: 1` is present.

---

**T-CLI14: JSON Schema definition and validation**
- Files: `docs/json-schema/v1/item.json`, `v1/item-list.json`, `v1/error.json`, `crates/clove/src/schema.rs`
- Deps: T-CLI01
- Description: Publish JSON Schema draft 2020-12 for: item object, list envelope, error envelope, dep-tree object, comment list. In `crates/clove`, implement `fn validate_json_output(json: &serde_json::Value, schema: &str) -> Result<()>` using the `jsonschema` crate. Used by integration tests to assert every command's output is schema-valid. Schema stability test: committed golden JSON file at `tests/fixtures/schema/v1_item_example.json`; test asserts it still parses with the current deserializer.
- AC: Schema file exists for each command family. Golden schema stability test passes.

---

**T-CLI17: `clove agent-doc`**
- Files: `crates/clove/src/cmd/agent_doc.rs`
- Deps: T-CLI01, T-CLI14
- Description: Implement `clove agent-doc [--format markdown|json] [--out FILE]`. Generates self-contained document as described in DESIGN.md §7.9. `--check --file PATH`: extracts `<!-- generated-by: clove vX.Y schema:N -->` from file, compares against current binary's schema version, exits non-zero with structured error if stale. Idempotency test: run twice, assert byte-identical output. Moved to M0 because agents need this from day one (all dependencies are M0 tasks).
- AC: Idempotency test. `--check` exits non-zero on stale schema version. JSON format output validates against schema.

---

**T-CLI15: `clove reindex` (M0 stub)**
- Files: `crates/clove/src/cmd/reindex.rs`
- Deps: T-CLI01, T-C05
- Description: In M0, `clove reindex` is a no-op that prints `note: index not yet built (M1 feature)` to stderr and exits 0 with `{ "v":1, "ok":true, "data":{"items_indexed":0} }` for JSON format. This prevents the command from being unknown in M0 and allows agents to call it safely.
- AC: Command exits 0. JSON output is valid.

---

**T-CLI16: Benchmark fixture generator**
- Files: `crates/clove-core/src/fixtures.rs` (behind `#[cfg(test)]` or a `fixtures` feature), `xtask/src/bench_fixtures.rs`
- Deps: T-C05
- Description: Implement a deterministic seeded-random fixture generator as specified in DESIGN.md §13.4. `cargo xtask bench-fixtures --count N --out-dir PATH` creates N item files following the statistical profile (25% closed, 65% open, 10% in_progress; 20% with 1–4 deps; etc.). Used by all criterion benchmarks and integration tests. Committed `tests/fixtures/golden_repo/` with exactly 7 items (2 dep chains, 1 cycle) for golden CLI tests.
- AC: `cargo xtask bench-fixtures --count 100 --out-dir /tmp/test` produces 100 valid `.md` files that all parse without errors.

---

**T-CLI18: `clove doctor` (store health check)**
- Files: `crates/clove/src/cmd/doctor.rs`, `crates/clove-core/src/doctor.rs`
- Deps: T-C04, T-C05, T-G01, T-G03, T-CLI14
- Description: Implement `clove doctor [--fix] [--strict] [--format json]` per DESIGN.md §7.7. In `clove-core`, add `fn diagnose(store: &FileStore) -> DoctorReport` that loads all items once, builds the `GraphStore`, and runs the §7.7 check suite: (1) collect parse failures (files that error on `parse_item_file`); (2) id/filename mismatch; (3) duplicate IDs; (4) per-item `validate_item()`; (5) dangling references across all five edge fields (from `GraphStore::build` dangling set + parent/relation targets); (6) `DependsOn` cycles via `kosaraju_scc` (size > 1); (7) invalid parent (self/missing/cyclic); (8) non-canonical labels (`label != normalize_label(label)` or post-normalization dup); (9) unsorted/duplicated list fields; (10) orphaned `<id>/comments/` dirs with no `<id>.md`; (11) config validity. Each finding is `DoctorIssue { severity: Error|Warning, code: &'static str, item: Option<String>, message: String, fixable: bool }`. `--fix` applies only the safe repairs (checks 8, 9, 10) by rewriting affected files via `FrontmatterWriter` / removing orphan dirs, then re-runs the suite. Report includes summary `{ errors, warnings, fixed, checked }`. JSON uses the standard envelope (§7.3). Exit 0 unless `--strict` and ≥1 unresolved error → exit 4. Structural issues (5,6,7) are never auto-fixed.
- AC: A fixture repo seeded with one of each issue (dangling dep, 2-node cycle, id/filename mismatch, duplicate id, `priority: 9`, `area:iOS` non-canonical label, orphaned comments dir) reports exactly those issues with correct severities. `--fix` normalizes the label, removes the orphan dir, sorts/dedups lists, and leaves the structural errors untouched. `--strict` exits 4 while unresolved errors remain and exits 0 after they're hand-fixed. A clean fixture repo reports `{ errors: 0, warnings: 0 }` and exits 0.

---

### M0 Acceptance Gates

- All `cargo test --workspace` tests pass on ubuntu, macos, windows.
- `clove ls` on the 1,000-item benchmark fixture completes in < 50ms (cold) per criterion.
- `clove ready` on the 1,000-item fixture completes in < 80ms (cold).
- All JSON outputs validate against JSON Schema v1.
- Golden CLI snapshot tests pass.
- Fuzz target `parse_item_file` runs 30s with no panics.
- Merge simulation test (T-C06 AC) passes.
- `clove agent-doc` idempotency test passes (T-CLI17 AC).
- `clove doctor` detects each seeded issue class and `--fix`/`--strict` behave per spec (T-CLI18 AC; V-I17/18/19).

---

## M1 — SQLite Index

---

**T-S01: SQLite schema and `Index::open`**
- Files: `crates/clove-index/src/db.rs`
- Deps: T-C01
- Description: Implement `Index { conn: rusqlite::Connection }`. `Index::open(path: &Utf8Path) -> Result<Index, IndexError>`: open or create, set all PRAGMAs (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `cache_size=-65536`). Check `PRAGMA user_version`; if 0 → create schema; if wrong version → delete and rebuild. Run the complete DDL from DESIGN.md §6.1. `Index::open_or_create` is the public entry point. `IndexError` enum: `SqliteError`, `SchemaMismatch`, `CorruptIndex`, `IoError`.
- AC: Opens empty DB. Opens DB from previous session. Wrong schema_version triggers rebuild. `SQLITE_CORRUPT` triggers rebuild and logs warning.

---

**T-S02: `upsert_item`**
- Files: `crates/clove-index/src/write.rs`
- Deps: T-S01, T-C01
- Description: The single write path. Implement `fn upsert_item(conn: &mut Connection, item: &Item) -> Result<(), IndexError>` that within one `BEGIN IMMEDIATE` transaction: (1) `INSERT OR REPLACE` into `items`; (2) `DELETE FROM edges WHERE from_id = ?`; (3) insert new edges into `edges`; (4) `DELETE FROM labels WHERE item_id = ?`; (5) insert new labels into `labels`; (6) sync FTS5 (contentless table — must manage explicitly): `INSERT INTO items_fts(items_fts, rowid, id, title, body) VALUES('delete', <rowid>, ...)` then `INSERT INTO items_fts(rowid, id, title, body) VALUES(...)`. Pass `item.frontmatter.id`, `item.frontmatter.title`, and `item.body` directly — these come from the `Item` struct, not from the `items` table (which has no `body` column). Direct SQL writes to `items` outside this function are forbidden — enforced by making the function the only `pub` write path and keeping the connection private.
- AC: FTS5 consistency test: upsert 100 items, search for known terms, assert results match. Update body text, re-upsert, assert search returns new text.

---

**T-S03: Staleness detection**
- Files: `crates/clove-index/src/stale.rs`
- Deps: T-S01
- Description: Implement `fn check_staleness(conn: &Connection, issues_dir: &Utf8Path) -> Result<StalenessReport, IndexError>` where `StalenessReport { stale_ids, new_ids, deleted_ids }`. Algorithm: single `read_dir` pass to collect `(id, mtime, size)` tuples → compare against `meta` table row (dir_mtime + file_count) for O(1) fast path. On mismatch: query `SELECT id, file_mtime, content_hash FROM items` → diff against readdir results. For mtime-differing entries: compute BLAKE3 hash and compare (the content-hash gate for HFS+ correctness). Implement `fn apply_staleness(conn: &mut Connection, report: &StalenessReport, issues_dir: &Utf8Path) -> Result<(), IndexError>`: parse and upsert stale/new items, delete deleted items, in one WAL transaction. Check macOS HFS+ special case: if `now - file_mtime < 2s`, always recheck hash.
- AC: Staleness detection correctness test: `cp -p` to replace 10 files with modified content while preserving mtime → `check_staleness` detects all 10. Staleness benchmark: 10k items, 0 stale → < 5ms.

---

**T-S04: `clove reindex` (full implementation)**
- Files: `crates/clove-index/src/reindex.rs`, `crates/clove/src/cmd/reindex.rs`
- Deps: T-S01, T-S02, T-C05
- Description: Full implementation replacing the M0 stub. Writes to `index.db.tmp`, rebuilds, renames. PID lockfile at `.clove/reindex.lock`. Steps as specified in DESIGN.md §6.6. Use `PRAGMA synchronous=OFF` during reindex, reset to NORMAL after. Write `meta` row last using `INSERT OR REPLACE INTO meta(id, dir_mtime, file_count, last_git_head) VALUES (1, ...)` (the `id=1` CHECK constraint ensures only one row ever exists). `PRAGMA wal_checkpoint(TRUNCATE)` at end. `HEAD` change detection: compare `.git/HEAD` against `meta.last_git_head`; force full readdir staleness pass if changed. JSON output: `{ "data": { "items_indexed": N, "duration_ms": N, "warnings": [...] } }`.
- AC: 10k-item reindex < 1000ms. Concurrent reindex processes: second exits with "reindex already running". Crashed reindex leaves no corrupt live index.

---

**T-S05: `clove search`**
- Files: `crates/clove/src/cmd/search.rs`
- Deps: T-S01, T-CLI01
- Description: `clove search <text>`: if index present, use FTS5 query; if absent, fall back to parallel rayon substring scan over file content (ranks title matches before body matches). Both paths return identical JSON shape with `_meta.source` indicating which path was used. JSON: `{ "data": [{ ...item... }] }`.
- AC: FTS5 consistency test: 100 items with known body, search for terms, assert match rayon fallback. Index absent: `_meta.source = "files"`. `clove search` < 20ms at 10k items via FTS5.

---

**T-S06: CLI read-path wrapper**
- Files: `crates/clove/src/index_guard.rs`
- Deps: T-S01, T-S03
- Description: Implement `fn with_index<F>(issues_dir, db_path, auto_refresh, f: F)` that: (1) if db_path absent → call f with `IndexMode::FileScan`, set `_meta.source = "files"`; (2) if present → run `check_staleness`, if stale_count ≤ 20 → `apply_staleness` then call f with `IndexMode::Index`; if stale_count > 20 → call f with `IndexMode::FileScan` + print warning; (3) if DB open fails with `SQLITE_CORRUPT` or schema mismatch → delete DB, log warning, call f with `IndexMode::FileScan`. Update `ls`, `ready`, `blocked`, `query` commands to use this wrapper. Also inject `_meta.stale_index: bool` and `_meta.stale_since` when index is detected stale.
- AC: File/index consistency property test: arbitrary sequence of mutations, assert `ls` output is identical from both paths. `_index_used` field present in all list responses.

---

**T-S07: Index-path queries**
- Files: `crates/clove-index/src/query.rs`
- Deps: T-S01, T-G01
- Description: Implement `fn query_items(conn: &Connection, filter: &Filter) -> Result<Vec<ItemRow>, IndexError>` using the ready query SQL from DESIGN.md §6.5 for `ready` mode. For `ls`/`query` mode: `SELECT * FROM items WHERE <filter conditions>`. All queries respect `--fields` projection. `topological_rank` stored and used for ordering but never exposed in public JSON schema.
- AC: SQL ready query returns same items as file-scan ready query on the same fixture. Topological rank test: items sort by (priority, topo rank) as expected.

---

**T-S08: `clove doctor` index-divergence check (M1 extension of T-CLI18)**
- Files: `crates/clove-core/src/doctor.rs`, `crates/clove-index/src/lib.rs`
- Deps: T-CLI18, T-S03
- Description: Extend `diagnose()` so that when an `index.db` is present it adds an **index↔files divergence** check (per DESIGN.md §7.7 M1 extension): compare the file-derived item count/content hashes against the index's stored oracle (reuse the T-S03 staleness machinery). Report divergence as a `warning` with `fixable: true`; under `--fix`, trigger a `reindex` and re-run. With `--no-index`, the check is skipped.
- AC: After manually corrupting/desyncing the index, `clove doctor` reports an index-divergence warning; `clove doctor --fix` rebuilds the index and a subsequent run reports clean. With `--no-index` the check does not run.

---

**M1 Acceptance Gates**

- `clove ls` id-ordering identical from file-scan and index paths (property test passes). The index path serves a lean `id/status/type/priority/title` projection; the file path the full frontmatter (see docs/M1_ACCEPTANCE_GATES.md).
- `clove ls` 10k items with warm index < 15ms (criterion). Revised from 10ms: SQLite's per-row step is ~0.8µs, so returning 10k rows floors at ~8ms; the lean projection lands ~11ms.
- `clove search` 10k items via FTS5 < 20ms (criterion).
- `clove reindex` 10k items < 1000ms.
- All M0 tests continue to pass.
- Staleness detection benchmark: 10k items, 0 stale < 5ms.

---

## M2 — Interop

---

**T-M01: `clove import tk`**
- Files: `crates/clove-import/src/tk.rs`, `crates/clove/src/cmd/import.rs`
- Deps: T-C05, T-CLI01
- Description: Implement importer as specified in DESIGN.md §11.1. Handle H1-title extraction from body. Set `source_system = "tk"`. Idempotent: skip items where `external_ref` matches existing item. `--dry-run` emits `would_create`/`would_skip`/`conflicts` without writing files. Fixture test with 5 representative tickets.
- AC: tk import fixture test. `--dry-run` writes no files. Idempotent re-run skips all.

---

**T-M02: `clove import beads`**
- Files: `crates/clove-import/src/beads.rs`
- Deps: T-C05, T-CLI01
- Description: Implement importer as specified in DESIGN.md §11.2. Define `BeadsIssue` thin deserialization struct. Map all fields. Stash unmapped fields in `metadata` blob. For items with `comment_count > 0`: emit stderr warning with IDs. Set `source_system = "beads"`. `--dry-run` support. Fixture test with Beads JSONL sample.
- AC: Beads import fixture test. Items with `comment_count > 0` emit warnings. `source_system = "beads"` on all items. Unmapped fields in metadata blob.

---

**T-M03: `clove import github`, `clove export github`**
- Files: `crates/clove-import/src/github.rs`, `crates/clove/src/cmd/export.rs`
- Deps: T-C05, T-CLI01
- Description: Implement import/export using `octocrab`. Export: encode clove metadata as `<!-- clove-meta: {...} -->` HTML comment. Import: `gh-<number>` ID prefix, parse clove-meta comment. Idempotent re-import. Integration test with real GitHub repo using `GITHUB_TOKEN` env var; skip if not set.
- AC: GitHub roundtrip test (with GITHUB_TOKEN). `--dry-run` writes no files. Idempotent re-import.

---

**T-M04: `clove export json`, `clove export jsonl`**
- Files: `crates/clove/src/cmd/export.rs`
- Deps: T-C05, T-CLI01
- Description: `clove export json`: single JSON envelope with all items. `clove export jsonl`: one item per line (NDJSON), isomorphic with Beads JSONL format. Both support `--out FILE`.
- AC: JSONL export: each line is valid JSON. Re-import of own JSONL export is idempotent.

---

**T-M05: Git merge driver**
- Files: `crates/clove/src/cmd/merge_driver.rs`
- Deps: T-C02, T-C03, T-CLI02
- Description: Implement `clove merge-driver <ancestor> <ours> <theirs> <marker-size>` as specified in DESIGN.md §9.2. Same-value scalar conflicts → auto-resolve. Union-merge for dep/label lists (three-way set merge). Dep removal conflict (A removes X, B adds X) → flag conflict. Body: delegate to `git merge-file`. Integration tests with fixture files: same-value status conflict (auto-resolves), divergent status (flags), dep union-merge, dep removal conflict.
- AC: Merge driver binary-availability test. Same-value conflict auto-resolves. Union-merge deps test.

---

**M2 Acceptance Gates**

- All three importers pass their fixture tests.
- `--dry-run` for all importers writes no files.
- GitHub roundtrip test passes (with token).
- Merge driver resolves same-value conflicts automatically.

---

## M3 — Daemon

---

**T-D01: M1 prerequisites for daemon**
- Files: `crates/clove-index/src/db.rs`
- Deps: T-S01
- Description: Ensure all item writes use atomic rename (T-C02 AC). Ensure all index writes use `BEGIN IMMEDIATE`. Ensure `file_mtimes` table exists in schema (for daemon startup sweep). This is a prerequisite check task, not new code — verify all prior tasks have met these requirements.
- AC: All existing write tests use atomic rename. File_mtimes table present in schema.

---

**T-D02: Daemon skeleton and signal handling**
- Files: `crates/cloved/src/main.rs`, `crates/cloved/src/lifecycle.rs`
- Deps: T-S01, T-S04
- Description: Implement `cloved` binary with `tokio` multi-thread runtime (2 workers). Shutdown handling is platform-specific:
  - **Unix:** `tokio::signal::unix::signal(SignalKind::terminate())` for SIGTERM.
  - **Windows:** `tokio::signal::ctrl_c()` for interactive shutdown. For `clove daemon stop`, use a named Windows event (`CreateEventW`) with name derived from the repo hash; the daemon selects on this event; `clove daemon stop` calls `OpenEventW` + `SetEvent` to signal shutdown.
  Shutdown sequence (all platforms): flush debounce batches, `PRAGMA wal_checkpoint(TRUNCATE)`, close SQLite, remove socket/pid files, exit 0.
  Implement PID file: write `daemon.pid` only after binding socket. Implement `daemon.lock` (advisory flock) to prevent two daemons.
- AC: SIGTERM test (Unix): start daemon, send SIGTERM, assert clean exit (pid file removed, socket removed, exit 0). Windows shutdown test: signal via named event, assert clean exit. Two daemons: second invocation prints "daemon already running" and exits non-zero.

---

**T-D03: IPC server**
- Files: `crates/cloved/src/ipc.rs`
- Deps: T-D02
- Description: Implement 4-byte length-prefixed JSON frame protocol over `interprocess::LocalSocketListener`. Handle v1 commands: PING/PONG, QUERY, REINDEX, STATUS. CLI-side: `fn try_connect_daemon(sock_path: &Utf8Path) -> Option<DaemonClient>` with 50ms connect timeout. On `ECONNREFUSED` or timeout: delete stale sock/pid files, return None. Send PING → verify PONG before any real command.
- AC: Stale socket recovery test: kill daemon with SIGKILL, run `clove ls`, completes in < 200ms (connection timeout + fallback). IPC PING/PONG round-trip test.

---

**T-D04: File watcher**
- Files: `crates/cloved/src/watcher.rs`
- Deps: T-D02, T-S02
- Description: Use `notify` 6.x with FSEvents (macOS) / inotify (Linux) / `ReadDirectoryChangesW` (Windows). Watch `.clove/issues/` recursively. Filter to `*.md` events. Exclude `index.db`, `*.db-shm`, `*.db-wal` explicitly. Per-file 200ms debounce (reset timer on new event for same path). Batch all events within debounce window into single SQLite transaction. Startup mtime sweep: query `file_mtimes`, scan directory, re-index changed files before writing `daemon.pid`.
- AC: Feedback-loop regression test: run `clove reindex` while daemon runs, assert zero `index.db` events processed. Debounce test: 10 chunks 10ms apart → exactly 1 SQLite update. Startup sweep benchmark: 1k items, 50 modified → < 500ms to ready.

---

**T-D05: Daemon config and `clove daemon` subcommands**
- Files: `crates/cloved/src/config.rs`, `crates/clove/src/cmd/daemon.rs`
- Deps: T-D02, T-D04, T-CF01
- Description: `[daemon]` config section (git_sync, watch_debounce_ms, idle_shutdown_min). `clove daemon start`: spawn a detached child process (Unix: double-fork; Windows: `CreateProcess` with `DETACHED_PROCESS` flag), wait for pid file to appear (5s timeout). `clove daemon stop`: Unix: send SIGTERM via PID; Windows: signal the named shutdown event (see T-D02); both poll for pid file removal (5s). `clove daemon status`: connect, send STATUS, pretty-print. Idle shutdown: `idle_shutdown_min > 0` → self-terminate after N idle minutes.
- AC: `clove daemon start|stop|status` functional test. Idle shutdown test: set `idle_shutdown_min=1` in test, advance time via tokio mock, assert shutdown.

---

**T-D06: Git auto-sync**
- Files: `crates/cloved/src/git_sync.rs`
- Deps: T-D04
- Description: Implement git auto-sync as described in DESIGN.md §8.7. Check conditions before committing: frontmatter parses cleanly (malformed-skip rule), `MERGE_HEAD` absent, `rebase-merge/` absent. Track `synced_at` in `file_mtimes`. Use `git2` crate for all git operations (no subprocess).
- AC: Skip-during-rebase test. Malformed-skip test. Synced_at prevents re-commit test.

---

**M3 Acceptance Gates**

- Daemon IPC round-trip time < 5ms for QUERY command.
- Daemon startup on 1k-item repo (50 modified) < 500ms.
- SIGTERM shutdown leaves no stale sock/pid files.
- All M0/M1/M2 tests continue to pass.

---

## M4 — Extras (Future)

Tasks for M4 (TUI, web UI, vendor bridges, changelog) are not detailed here. They will be
planned in a separate session once M3 is complete. Acceptance gates for M3 completion serve
as the entry condition for M4 planning.

---

## Cross-Cutting Tasks (Any Milestone)

---

**T-X01: Performance benchmarks (criterion)**
- Files: `benches/bench_parse.rs`, `benches/bench_scan.rs`, `benches/bench_graph.rs`, `benches/bench_sqlite.rs`
- Deps: T-CLI16
- Description: Implement all criterion benchmarks from DESIGN.md §13.1. Gate values as listed. CI runs `cargo bench --no-run` (compile check); full benchmarks run on developer machine and before releases. `cargo xtask bench-compare` script generates comparative table.
- AC: All benchmarks compile. Gate assertions embedded as `#[test]` with `std::time::Instant` (not criterion) for CI enforcement.

---

**T-X02: Fuzz targets**
- Files: `fuzz/fuzz_targets/parse_item_file.rs`, `fuzz/fuzz_targets/parse_dep_list.rs`
- Deps: T-C03
- Description: cargo-fuzz targets as described in VERIFICATION_PLAN.md §3.3. Seed corpus committed. CI runs each target for 30 seconds as regression test. Full fuzzing (24h+) before each release.
- AC: 30s CI run with no panics on committed corpus.

---

**T-X03: `cargo xtask` commands**
- Files: `xtask/src/main.rs`
- Deps: T-I01
- Description: Implement xtask commands: `bench-fixtures`, `bench-compare`, `test-all` (runs cargo test + fuzz corpus). Following the existing hn-reader xtask pattern.
- AC: All xtask commands run without error.
