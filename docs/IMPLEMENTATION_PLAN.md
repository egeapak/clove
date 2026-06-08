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
- `clove ls` 10k items with warm index < 15ms (criterion). ~4.5ms via the `idx_items_list` covering index (index-only scan; per-row step ~116ns). Gate set to 15ms for headroom; see docs/M1_ACCEPTANCE_GATES.md.
- `clove search` 10k items via FTS5 < 20ms (criterion).
- `clove reindex` 10k items < 1000ms.
- All M0 tests continue to pass.
- Staleness detection benchmark: 10k items, 0 stale < 5ms.

---

## M2 — Interop

> **Status: ✅ complete and gated** (Phases 0–6 on `claude/charming-hamilton-cqkHx`).
> T-M01–T-M05 all implemented; the task specs below are unchanged. Per-gate
> evidence is in `docs/M2_ACCEPTANCE_GATES.md` (mirrors `docs/M2_PLAN.md`).

---

**T-M01: `clove import tk`**  — ✅ implemented (Phase 3)
- Files: `crates/clove-import/src/tk.rs`, `crates/clove/src/cmd/import.rs`
- Deps: T-C05, T-CLI01
- Description: Implement importer as specified in DESIGN.md §11.1. Handle H1-title extraction from body. Set `source_system = "tk"`. Idempotent: skip items where `external_ref` matches existing item. `--dry-run` emits `would_create`/`would_skip`/`conflicts` without writing files. Fixture test with 5 representative tickets.
- AC: tk import fixture test. `--dry-run` writes no files. Idempotent re-run skips all.

---

**T-M02: `clove import beads`**  — ✅ implemented (Phase 4)
- Files: `crates/clove-import/src/beads.rs`
- Deps: T-C05, T-CLI01
- Description: Implement importer as specified in DESIGN.md §11.2. Define `BeadsIssue` thin deserialization struct. Map all fields. Stash unmapped fields in `metadata` blob. For items with `comment_count > 0`: emit stderr warning with IDs. Set `source_system = "beads"`. `--dry-run` support. Fixture test with Beads JSONL sample.
- AC: Beads import fixture test. Items with `comment_count > 0` emit warnings. `source_system = "beads"` on all items. Unmapped fields in metadata blob.

---

**T-M03: `clove import github`, `clove export github`**  — ✅ implemented (Phase 5, `github` feature)
- Files: `crates/clove-import/src/github.rs`, `crates/clove/src/cmd/export.rs`
- Deps: T-C05, T-CLI01
- Description: Implement import/export using `octocrab`. Export: encode clove metadata as `<!-- clove-meta: {...} -->` HTML comment. Import: `gh-<number>` ID prefix, parse clove-meta comment. Idempotent re-import. Integration test with real GitHub repo using `GITHUB_TOKEN` env var; skip if not set.
- AC: GitHub roundtrip test (with GITHUB_TOKEN). `--dry-run` writes no files. Idempotent re-import.

---

**T-M04: `clove export json`, `clove export jsonl`**  — ✅ implemented (Phase 1; round-trip closed Phase 4)
- Files: `crates/clove/src/cmd/export.rs`
- Deps: T-C05, T-CLI01
- Description: `clove export json`: single JSON envelope with all items. `clove export jsonl`: one item per line (NDJSON), isomorphic with Beads JSONL format. Both support `--out FILE`.
- AC: JSONL export: each line is valid JSON. Re-import of own JSONL export is idempotent.

---

**T-M05: Git merge driver**  — ✅ implemented (Phase 2)
- Files: `crates/clove/src/cmd/merge_driver.rs`
- Deps: T-C02, T-C03, T-CLI02
- Description: Implement `clove merge-driver <ancestor> <ours> <theirs> <marker-size>` as specified in DESIGN.md §9.2. Same-value scalar conflicts → auto-resolve. Union-merge for dep/label lists (three-way set merge). Dep removal conflict (A removes X, B adds X) → flag conflict. Body: delegate to `git merge-file`. Integration tests with fixture files: same-value status conflict (auto-resolves), divergent status (flags), dep union-merge, dep removal conflict.
- AC: Merge driver binary-availability test. Same-value conflict auto-resolves. Union-merge deps test.

---

**M2 Acceptance Gates** — ✅ met (see `docs/M2_ACCEPTANCE_GATES.md` for per-gate evidence)

- ✅ All three importers pass their fixture tests.
- ✅ `--dry-run` for all importers writes no files.
- ✅ GitHub roundtrip test passes (with token); offline mapping/codec tested
  everywhere, network round-trip token-gated/`#[ignore]` so CI stays green.
- ✅ Merge driver resolves same-value conflicts automatically (V-I14/15/16).
- ✅ All M0 + M1 gates still pass (M2-G06).

---

## M3 — Daemon

> **Status: ✅ complete and gated** (see `docs/M3_PLAN.md` and
> `docs/M3_ACCEPTANCE_GATES.md`). T-D01–T-D07 all implemented across phases P0–P6;
> the task specs below are unchanged. New crate `clove-ipc` (lean IPC: protocol +
> frame codec + sync client); `cloved` gains lifecycle/IPC/watcher/git-sync; `clove`
> gains `daemon start|stop|status`, transparent read routing, and a `doctor`
> daemon-health check. Index schema bumped to **v3** (`file_mtimes.synced_at`).

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

**T-D07: `clove doctor` daemon-health extension** *(added by the M3_PLAN.md §1.1 CLI-surface review)*
- Files: `crates/clove/src/cmd/doctor.rs`
- Deps: T-D03, T-D05
- Description: Extend `clove doctor` (DESIGN §7.7) with daemon-footprint checks, since M3 is the first milestone that can leave `daemon.sock`/`daemon.pid`/`daemon.lock` on disk. Add three **warning**-severity checks running after the M0 file checks + M1 index-divergence check, reusing the existing `DoctorIssue`/`--fix` machinery: (1) **stale socket/pid** — sock/pid present but liveness probe (connect+PING via `DaemonClient`) fails → `--fix` removes the dead sock+pid (the §8.3 cleanup as an explicit repair); (2) **orphaned `daemon.lock`** — lock file present with no live daemon → `--fix` removes it (only when liveness is negative, since an `fd-lock` file legitimately persists while running); (3) **pid/socket mismatch** — exactly one of pid/sock present → `--fix` removes the orphan. A live, healthy daemon yields zero findings and is never modified by `--fix`. Checks run even with `--no-index` (socket/pid state is index-independent). Warnings exit 0 (like index-divergence); `--strict` is unaffected by these warnings.
- AC: Live daemon → zero daemon findings, `--fix` leaves files intact. SIGKILL'd daemon (stale sock+pid+lock) → warns on each; `--fix` removes them; re-run clean. pid-without-sock and sock-without-pid each → one warning fixed by `--fix`. JSON findings carry `{severity:"warning", code:"daemon-stale-socket"|"daemon-orphan-lock"|"daemon-pid-sock-mismatch", fixable:true}`.

---

**M3 Acceptance Gates**

- Daemon IPC round-trip time < 5ms for QUERY command.
- Daemon startup on 1k-item repo (50 modified) < 500ms.
- SIGTERM shutdown leaves no stale sock/pid files.
- `clove doctor` detects and `--fix`-cleans a dead-daemon footprint; a live daemon is untouched (T-D07, gate M3-G10).
- All M0/M1/M2 tests continue to pass.

---

## M4 — Extras (Future)

Tasks for M4 (web UI, vendor bridges, changelog) are not detailed here. They will be
planned in a separate session. Acceptance gates for M3 completion serve as the entry
condition for M4 planning.

---

**T-U01: `clove tui` — read-only terminal browser**  — ✅ implemented
- Files: `crates/clove-tui/src/{lib,app,ui}.rs`, `crates/clove/src/cmd/tui.rs`,
  `crates/clove/src/cli.rs` (`Tui` subcommand).
- Deps: T-C05, T-G01–T-G05, T-CLI01.
- Description: New `clove-tui` crate (ratatui, which re-exports crossterm; depends
  only on `clove-core`). `clove tui` launches a master-detail browser that reads
  via the file-store scan path (`scan_frontmatter` + `GraphStore::build`) — always
  correct, no index/daemon coupling, never mutates. Top tab bar **All / Ready /
  Blocked** (with live counts), an item list (status glyph, single-letter type
  icon, **short id** [prefix dropped, leading zeros trimmed, e.g. `#42`], a
  **priority glyph** [`!` p0, `↑` p1, `•` p2 & p3, `↓` p4 on a graded colour ramp
  red→amber→dim icy blue (p3)→gray (p4); p2/p3 share `•` and differ by hue], title,
  ready/blocked badge, sorted by `(priority, topo rank, id)` like `ls`), and
  a detail pane with three sub-views: **Overview** (wide: a **fixed, shrink-to-fit
  two-line header** [line 1: short id + priority glyph + ALL-CAPS type tag,
  status flush-right; line 2: bold title with assignee + a **deps count**
  flush-right under the status], an **edge-to-edge rule**, a
  **scrolling Markdown body**, another edge-to-edge rule, and a **sticky footer**
  [labels left, `created Jan 20 · updated Jan 24` right at day resolution]; narrow:
  one scrolling paragraph with the title wrapping and labels/dates inline; the deps
  *list* lives in the Dep tree tab), **Dep tree** (status glyphs + titles inline,
  `[ready]`/`(cycle)` markers), and **Comments**. The body is rendered from
  CommonMark via `pulldown-cmark` (`markdown.rs`): headings, emphasis/strong/
  strikethrough, inline + fenced code, bullet/ordered/nested lists, block quotes,
  rules, and task-list markers; paragraphs reflow under the pane's word wrap.
  Relative times use an injectable `App::now` (refreshed with the data; pinned by
  tests for deterministic snapshots). Substring search (`/`) over
  id/title/labels; `r` re-scans from disk; `?` help overlay. Keys: `j/k`+arrows,
  `g/G`, Tab/`1`/`2`/`3`, `o`/`t`/`c`, `←/h`·`→/l`·Enter (pane focus), PgUp/PgDn,
  `/`, `r`, `?`, `q`/Esc/Ctrl-C. **Adaptive layout** (`ui::pick_layout`): side-by-side
  when wide (≥80 cols), list-over-detail when stacked (50–79 cols & tall), and a
  single focused pane when narrow/short — plus width-aware list-row column dropping,
  a compact one-line tab bar below 20 rows, content-sized/full-screen overlays, and
  a "terminal too small" guard. Packaged as a new crate behind a default-on
  subcommand (per the M4 scoping decision); interactive-only, so it ignores
  `--format`. Design directions came from a frontend-design and a UX/IA review (see
  the deferred backlog for the larger items they raised).
- **Sort & filter** (read-only): `s` cycles the sort field
  (rank/priority/created/updated/id — `rank` = the default `(priority, topo, id)`
  order; topo is dropped for non-priority fields, with an `id` tiebreak), `S`
  toggles direction; only `self.view` is re-sorted (never `self.all`). `f` opens a
  facet **filter menu** (a scrolling popup): status/assignee are single-value
  (radio), type/priority are multi-value OR (checkbox), labels are multi-value AND;
  values are the ones actually present in the repo (sorted/deduped). Facets AND
  across, composing orthogonally with `/` search and the graph-derived tabs; `x`
  clears. Active sort/filter show as status-line chips with an `Items (N/M)` count
  and an empty-result escape-hatch message. Filters/sort persist across tab-switch
  and `r`; selection is preserved by id across every view change.
- AC: data-layer unit tests (ready/blocked partition, tab + search filtering,
  detail load, navigation clamping, **filter narrowing + multi-OR/AND semantics,
  sort ordering, selection stability across filtering**), a `TestBackend` render
  smoke test, and **insta render snapshots** of 12 states (overview, blocked tab,
  dep tree, comments, search, help, detail-focused, empty, **filter menu, filtered,
  sorted, filtered-empty**) each at three terminal shapes — portrait 40×48
  (single), landscape 120×18 (wide + compact tabs), square 60×60 (stacked) — plus
  Overview edge cases (**long/wrapping title, long label list with `+N` footer
  truncation, and a scrolled detail** that keeps the pinned footer in place) —
  validating the adaptive layout. (The first cut was read-only; an `n`/`e`
  add/edit modal form — full field set incl. dep/parent — has since landed,
  writing through the unified `clove_core` path. See HANDOFF "Unified add/edit".)
- Tooling: an `#[ignore]`d `generate_screenshots` test rasterizes each screen's
  cell buffer (colours + bold) to PNG via a system monospace font (DejaVu Sans
  Mono preferred for glyph coverage), behind test-only `image`/`ab_glyph`
  dev-deps. Output goes to `docs/screenshots/` (gitignored; images are not
  committed).
- **Modular refactor (done):** The read-only TUI has been modularized —
  `app/{mod,data,listing,detail,filter_menu}.rs` with state regrouped into
  `Data`/`Listing`/`DetailPane`/`FilterMenu` sub-structs (command methods stay
  on `App` as the coordinator), and `ui/{mod,util,style,tabs,list,status,help,
  filter_menu}.rs` + `ui/detail/{mod,overview,tree,comments}.rs`
  (per-component/per-page). The event loop is tick-driven (1fps idle /
  redraw-after-event / 10fps when `App::is_busy()`). The split is structural
  (no locks yet); the **next step** is the concurrent TUI model — move the
  wholesale re-scan onto a background worker behind per-sub-struct locks and
  drive the 10fps `is_busy()` cadence with a spinner.

**M4 backlog (recorded so it is not lost; not yet task-specified):**
- **TUI write actions** — extend `clove tui` with the common mutations (status
  transitions, priority/assignee/label edits) and beyond, on top of the read-only
  browser landed in T-U01.
- **TUI read-only follow-ups** (from the T-U01 design/UX reviews; all backed by
  existing `clove-core` APIs, no engine work): (1) inbound/"blocks" + referenced-by
  + epic-children lists in Overview (the graph has the reverse edges); (2) a
  navigation stack — follow a related id to its item and pop back, decoupled from
  the active tab; (3) a **Cycles** + **Problems/doctor-lite** view (surfacing
  `all_cycles()`, `dangling_ids()`, malformed parents, invalid priorities — items
  currently *excluded* from both Ready and Blocked and thus invisible); (4) an
  **Excluded/attention** tab (or fold into Problems) to complete the
  ready∪blocked∪closed partition. *(Relative timestamps + Markdown body rendering,
  and sort + filter controls, landed in T-U01.)* Possible later refinements to the
  shipped sort/filter: per-namespace OR within labels, an "assigned to me" toggle,
  and lifting a shared filter type into `clove-core` if a third consumer appears.
- **`clove stats` — work-item analytics command** — **DONE (M4)**. A user-facing
  aggregate/statistics view: counts by status / type / priority / assignee / label,
  ready / blocked / excluded / dangling totals, dependency-cycle count, per-epic
  completion rollups, and created/closed throughput over rolling windows (7d/30d/all).
  Also surfaces daemon operational telemetry (the §8.4 `STATUS` payload) and local index
  presence/freshness in the same report. Analytics are computed from a single file scan +
  graph build (files are truth); the index/daemon are reported, not relied on for
  correctness. **Persistence:** snapshots are stored in a `snapshots` table **inside
  `.clove/index.db`** (one database for the tool, no separate file). The index is a
  rebuildable cache, so the layer carries the `snapshots` table across its two
  destructive ops — a full `reindex` (tmp-build + atomic rename) copies the rows
  before the rename, and schema-mismatch recovery reads them out before the rebuild
  and reinserts after; the table is created idempotently on open, so no
  `user_version` bump is needed. Only raw file corruption loses history (acceptable;
  files remain truth). `--snapshot` records; `--history [--since] [--limit]` replays.
  Implemented across `clove-core::stats` (`StatsReport`/`compute`),
  `clove-index::stats_store` (the `snapshots` table + `Index::record_snapshot`/
  `snapshot_history` + reindex/recovery carry-over), and `clove/src/cmd/stats.rs`;
  JSON schema `docs/json-schema/v1/stats.json`. A running daemon also auto-records
  snapshots on a timer (`[daemon] stats_snapshot_min`, default 60;
  `cloved/src/snapshot.rs`), using the same compute path so daemon and manual
  snapshots are identical.
- **Incremental index & daemon graph** — **DONE (M4)**. The incremental
  `apply_staleness` path now keeps the derived graph columns exact (canonical
  Kahn toposort in clove-core; `clove-index::derive` recomputes
  `topological_rank`/`has_dangling_deps`/`excluded` from the index's own
  `items`/`edges` tables, delta-only, in-transaction), so it matches a full
  `reindex` without one (schema v4 adds `items.excluded`; SQL `ready` excludes
  hard-cycle/malformed-parent members). The daemon's hot `GraphStore` is rebuilt
  from the index DB (`Index::graph_frontmatters`) rather than re-scanning the item
  files. `apply_staleness` skips the recompute entirely for content-only edits
  (status/title/assignee/priority/labels) via a topology-change guard, recomputing
  only when an item is added/deleted or a changed item's edge/parent signature
  differs. True sub-linear O(region) delta mutation (Pearce–Kelly) was implemented
  and benchmarked but rejected: its order is history-dependent, which breaks
  clove's canonical-order parity contract (and it can't represent cycles).
- **Web UI** — **DONE (M4)**. New `clove-web` crate (axum + embedded SvelteKit SPA)
  and a `clove serve` subcommand; the daemon serves it by default and `clove serve`
  hands off to a running daemon. Kanban board / filterable list / detail / timeline,
  read + light-write, live updates over WebSocket via the file-watcher; `/api/v1`
  REST mirrors the CLI's JSON envelope + exit codes. Assets are gzip-precompressed,
  embedded, and served from memory; markdown via micromark + a custom id-autolink
  extension. Plan, decisions, and status: `docs/M4_WEB_UI_PLAN.md`; design themes in
  `docs/web-ui-mockups/`. Deferred follow-ups: binary-size trim (make the `github`
  import feature opt-out — it costs ~3.5 MB) and wiring the SQLite stats-snapshot
  series into the web `/stats/history`. (A body editor + full add/edit page have
  since landed — see HANDOFF "Unified add/edit".)
- **MCP server** — **DONE (M4)**. New `clove-mcp` crate (rmcp 1.7) and a `clove mcp`
  subcommand expose clove to AI agents over the MCP **stdio** transport as 12 native
  tools (`clove_ready`/`blocked`/`list`/`show`/`search`/`dep_tree`/`stats` reads;
  `clove_new`/`status`/`edit`/`comment`/`dep_add` writes), behind a default-on `mcp`
  feature. **Topology B:** each client spawns `clove mcp`; writes prefer the single
  `cloved` daemon (concurrent agents share one serialized writer that keeps the
  index/graph coherent) and fall back to direct ops; reads compute from files. The
  shared logic was lifted into `clove_core::view` (filters/ordering/JSON shaping) and
  `clove_core::ops` (the high-level operations), reused by the CLI, daemon, and MCP
  engine. The CLI↔daemon IPC was **rebuilt on `tarpc`** (typed service over the
  interprocess socket), replacing the hand-rolled frame/protocol; `DaemonClient`
  keeps its blocking API and gained the mutation/show/stats methods. Decisions and
  the comparison that chose rmcp + tarpc + topology B are in this session's notes.
  Deferred follow-ups: auto-start the daemon from `clove mcp`, and server-push
  notifications (MCP `tools/list_changed` / a ready-queue subscription) when the
  graph changes.
- Bidirectional vendor bridges (GitHub/GitLab/Jira); richer history/changelog.

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
