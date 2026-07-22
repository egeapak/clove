# clove — Expanded Design Document

> **Status:** Authoritative design — the implementation-ready spec the shipped
> code (M0–M4) tracks. This is the one canonical design document; the milestone
> task plans and verification plans it originally cross-referenced have been
> retired now that the surface they described has landed.

---

## 1. Crate / Workspace Layout

```
clove/                          (workspace root)
  Cargo.toml                    [workspace] + [workspace.dependencies]
  crates/
    clove-core/                 lib — item model, file store, DAG engine, ID gen
    clove-index/                lib — SQLite index, FTS5, staleness, reindex
    clove-import/               lib — json/jsonl export, merge driver, tk/beads importer logic, pure GitHub mapping
    clove-plugin/               lib — cargo-style plugin support (PluginContext + envelope harness)
    clove-sync-github/          bin — the `clove-sync-github` plugin (two-way GitHub sync, octocrab)
    clove-import-tk/            bin — the `clove-import-tk` plugin (import a tk `.tickets/` dir)
    clove-import-beads/         bin — the `clove-import-beads` plugin (import a Beads `issues.jsonl`)
    clove/                      bin — CLI, JSON envelopes, exit codes, plugin dispatch
    cloved/                     bin — daemon, file-watcher, IPC server
  benches/                      criterion benchmarks (shared fixture crate)
  fuzz/                         cargo-fuzz targets
  tests/
    fixtures/                   committed fixture repos (golden_repo, import/*)
    golden/                     insta snapshot files
```

**Crate dependency graph** (no cycles):

```
clove-core   (no SQLite, no async, no IPC, no clap)
    ↑
clove-index  (rusqlite bundled, depends on clove-core)
    ↑         ↑
clove        cloved        (both depend on clove-core + clove-index)
    ↑
clove-import (depends on clove-core; also tokio + octocrab for GitHub import/export)
```
Note: `clove-import` uses `tokio` (async runtime) and `octocrab` (GitHub API client) for the
GitHub importer. File-based importers (beads, tk) use only `clove-core`. `clove-import` has
no SQLite surface.

**Key dependency pins** (in `[workspace.dependencies]`):

| Crate | Version | Rationale |
|---|---|---|
| `serde` | 1 | baseline |
| `serde_yaml_neo` | `"0.9"` | strict YAML (Ware fork of serde_yaml); exact Cargo.toml key: `serde_yaml_neo = "0.9"`. See §4 and §14.2. |
| `petgraph` | 0.6 | DAG engine |
| `rusqlite` | `{ version="0.31", features=["bundled"] }` | embedded SQLite, no runtime dep |
| `notify` | `6` (stable, NOT 9-rc) | file-watch; 9.0-rc is not production-ready |
| `interprocess` | 2 | Unix socket + Windows named pipe |
| `tokio` | `{ version="1", features=["full"] }` | daemon async runtime only |
| `clap` | `{ version="4", features=["derive"] }` | CLI only |
| `anyhow` | 1 | CLI + daemon only; never in clove-core |
| `thiserror` | 1 | clove-core + clove-index typed errors |
| `proptest` | 1 | property tests |
| `criterion` | 0.5 | benchmarks |
| `insta` | 1 | snapshot tests |
| `assert_cmd` | 2 | CLI integration tests |
| `tempfile` | 3 | atomic writes + test isolation |
| `camino` | 1 | UTF-8 paths throughout clove-core |
| `smolstr` | 0.2 | inline IDs ≤23 bytes |
| `getrandom` | 0.2 | ID entropy |
| `memchr` | 2 | fast frontmatter boundary scanning |
| `blake3` | 1 | content hashing for staleness |
| `rayon` | 1 | parallel parse above 500 items |
| `fd-lock` | 4 | advisory file locking |
| `git2` | 0.19 | worktree detection (no `git` subprocess) |
| `chrono` | 0.4 | RFC3339 timestamps |
| `jiff` | 0.1 | nanosecond timestamps for comment filenames |
| `octocrab` | 0.51 | GitHub REST API client (clove-import GitHub importer/exporter) |

**Rust edition/MSRV:** `edition = "2021"`. **No pinned MSRV — clove tracks current
stable Rust** (decision 2026-06-02: a strict 1.80 MSRV added dependency-version friction
with no consumer needing it for this new binary; revisit if `clove-core` gains external
embedders who require an older toolchain).

**Feature flags:** none. Do not gate clove-index behind a feature flag; keep crate boundaries.

---

## 2. On-Disk Data Model

### 2.1 File Layout

```
.clove/
  config.toml           # repo-level config — committed
  .gitignore            # ignores: index.db, *.db-shm, *.db-wal, daemon.sock, daemon.pid, reindex.lock, daemon.lock, index.db.tmp
  issues/
    <id>.md             # one item per file — committed (source of truth)
    <id>/               # only present when comments exist (sibling dir to <id>.md)
      comments/
        <rfc3339nano>-<author-slug>-<4char-random>.md
  index.db              # SQLite derived cache (also holds the durable `snapshots` history table) — .gitignore'd
  daemon.sock           # Unix socket (macOS/Linux) — .gitignore'd
  daemon.pid            # PID file — .gitignore'd
  reindex.lock          # advisory lock during reindex — .gitignore'd
```

**Decision: item file path stays flat** (`.clove/issues/<id>.md`). Comment subdirectory
(`.clove/issues/<id>/`) only materializes when the first comment is written. A file and a
directory with the same base name coexist cleanly on all target filesystems. `clove show`
checks for both.

### 2.2 Frontmatter Schema

**Schema version field:** Every item file carries `schema: 1` as the first YAML key. Missing
`schema` is treated as `schema: 1` (pre-versioning files are implicitly v1).

**Required fields** (always present, serialized in this exact order):

```
schema, id, title, status, type, priority, created, updated
```

**Optional fields** (omitted when null/empty, serialized after required fields in this order):

```
closed, assignee, parent, labels, deps, relates, duplicates, supersedes
```

**`blocks` is never stored.** It is always derived from scanning other items' `deps` fields.
Storing it creates a dual-write consistency hazard (confirmed by tk and Beads history).

**`source_system` and `external_ref`** are optional fields added from M0. They are listed
after `supersedes` in the optional section. Required for idempotent re-import and roundtrip
export.

**Complete canonical frontmatter example** (`.clove/issues/proj-7af3q.md`):

```yaml
---
schema: 1
id: proj-7af3q
title: Article image download and compression
status: open
type: feature
priority: 1
created: 2026-06-02T10:00:00Z
updated: 2026-06-02T14:23:00Z
assignee: ege
parent: proj-2bk8n
labels: [area:core, perf]
deps: [proj-3k2mz]
relates: [proj-9p1qr]
---

Save compressed versions of images the readability crate kept. Design:
docs/plan-article-image-download.md
```

Fields absent because null/empty: `closed`, `duplicates`, `supersedes`, `source_system`,
`external_ref`. Note `blocks` is absent entirely — always derived.

**List serialization rule:** All list fields (`labels`, `deps`, `relates`, `duplicates`,
`supersedes`) serialize as **inline YAML flow sequences sorted lexicographically**:
`deps: [proj-3k2mz, proj-9p1qr]`. Never block-style. Empty lists serialize as `[]`, never
omitted. Null scalars serialize as `null`, never omitted. **This is enforced by a hand-rolled
`FrontmatterWriter` (see §4), not by serde library defaults.**

**Label normalization rule (case-insensitive labels):** Labels are **canonicalized to
lowercase on every write** — `clove new -l`, `clove label add`, `clove edit labels+=`, and all
importers pass each label through `normalize_label()` (Unicode lowercase via
`str::to_lowercase`, leading/trailing whitespace trimmed, internal whitespace collapsed to a
single space, empty result rejected). The stored frontmatter therefore only ever contains
canonical labels, so `area:iOS`, `area:IOS`, and `area:ios` collapse to the single label
`area:ios` — eliminating case/whitespace drift across a project. After normalization the list
is de-duplicated, then sorted lexicographically (per the list rule above). **Filters
(`--label`) normalize their argument the same way before matching**, so `--label AREA:IOS`
matches `area:ios`. The `key:value` shape (e.g. `area:core`) remains a pure convention — the
colon is not special to the engine; both key and value are lowercased as one string. The
SQLite `labels` table (see §6) stores the already-normalized value, so no `COLLATE NOCASE` is
needed. `clove migrate` re-normalizes legacy labels and de-dups any collisions it creates.
Identifiers that must preserve case (e.g. external ticket keys) belong in `external_ref` /
`source_system`, not in labels.

### 2.3 Rust Type System

```rust
// clove-core/src/model.rs

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub frontmatter: ItemFrontmatter,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFrontmatter {
    pub schema: u32,            // must be 1; future: migration shim
    pub id: CloveId,
    pub title: String,
    pub status: ItemStatus,
    pub item_type: ItemType,    // serialized as "type"
    pub priority: Priority,     // u8, range 0–4
    pub created: chrono::DateTime<chrono::Utc>,
    pub updated: chrono::DateTime<chrono::Utc>,
    // optional fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CloveId>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub deps: Vec<CloveId>,
    #[serde(default)]
    pub relates: Vec<CloveId>,
    #[serde(default)]
    pub duplicates: Vec<CloveId>,
    #[serde(default)]
    pub supersedes: Vec<CloveId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
}

// status + closed are a coupled invariant:
// Closed { at } ↔ status=="closed" + closed timestamp
// Enforced by custom Serialize/Deserialize that merges/splits these fields
#[derive(Debug, Clone, PartialEq)]
pub enum ItemStatus { Open, InProgress, Closed { at: chrono::DateTime<chrono::Utc> } }

#[derive(Debug, Clone, PartialEq)]
pub enum ItemType { Bug, Feature, Chore, Docs, Epic }

pub struct Priority(pub u8); // validated 0–4
```

**Serialization contract:** `ItemFrontmatter` implements `Serialize` manually via
`serde::ser::SerializeMap`, writing fields in the exact canonical order listed in §2.2. This
is approximately 80 lines of boilerplate and is the only file that needs changing when fields
are added or reordered.

### 2.4 Schema Versioning and Migration

- `schema: 1` is the initial version. Future versions increment the integer.
- **Read path:** missing `schema` → treat as v1. Unknown `schema` value → warn on stderr,
  attempt best-effort read via migration shim.
- **Write path:** `clove edit` / `clove status` on a v1-or-later file: auto-migrate in memory
  and apply the write. No silent on-read mutation.
- `clove migrate` rewrites all files to the current schema version; shows a diff for
  confirmation unless `--yes` is passed.
- Never store `schema_version` in `config.toml` — it belongs in each item file.

### 2.5 Comment Files

**Format:** `.clove/issues/<id>/comments/<rfc3339nano>-<author-slug>-<4char-random>.md`

- **Timestamp:** nanosecond-precision RFC3339 (`2026-06-02T10:00:00.123456789Z`) so
  lexicographic sort == chronological sort. The nanosecond portion plus the 4-char random
  suffix makes same-clock-second collisions astronomically unlikely.
- **Author slug:** `git user.email` lowercased, non-alphanumeric → `-`, truncated at 32 chars.
- **Body:** plain Markdown, no frontmatter.
- **Why the 4-char random suffix:** covers HFS+ (1-second mtime granularity) and any network
  filesystem that coarsens clock resolution. If the target path already exists, regenerate.
- **Concurrent comment additions** on two branches create two distinct files → zero merge
  conflicts by construction. This is the decisive reason for the append-only sidecar design.

---

## 3. ID Scheme

**Format:** `<prefix>-<8 Crockford-base32 chars>` — e.g. `proj-7AF3K2MN`.

- **Alphabet:** Crockford base32 uppercase — `0-9 A-Z` minus `I L O U` (32 symbols).
- **Length:** 8 random chars = 32^8 ≈ 1.1 trillion values.
- **Collision probability:** at 10,000 items = 0.005%; at 100,000 items = 0.5%. Acceptable.
- **Prefix:** from `config.toml` `id_prefix` (default: first 4 alphabetic chars of the git
  repo's root directory name, lowercased). Stored in `config.toml`, not derived at runtime on
  every call.
- **Collision retry:** `new_id()` checks file existence before returning; retries up to 3
  times then returns `IdConflict` error.
- **Validation regex:** `^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$` (prefix 1–8 alphanum + hyphen +
  8 Crockford chars). Applied at both creation time and every resolution time.

**Decision: random beats content-hash.** Content-hashes (Beads style) collide on
identical-title items created simultaneously on two branches. Sequential integers cause
deterministic merge conflicts. Random 8-char Crockford is the correct balance of brevity,
safety, and grep-ability.

**`CloveId` newtype:** A `CloveId(SmolStr)` that validates on construction. SmolStr stores
IDs ≤23 bytes (e.g., `proj-7AF3K2MN` is 13 chars) with zero heap allocation.

---

## 4. Frontmatter Parsing and Serialization

**Parsing strategy: hand-rolled zero-copy scanner for list/query operations; serde_yaml_neo
for full parse.**

Two-phase pipeline:
1. Byte-budget check: reject files where frontmatter block > 64 KiB before any allocation.
2. Reject YAML anchors/aliases: scan for `&` in value position and `*` before serde_yaml_neo
   sees the bytes. Return `ParseError::AliasNotAllowed`.
3. Boundary scan: `memchr::memmem::find` to locate the closing `---\n`. This is zero-copy —
   the frontmatter bytes are a slice of the mmap'd/read file.
4. Deserialize via `serde_yaml_neo` into `ItemFrontmatter` with `#[serde(deny_unknown_fields)]`.
5. Validate `id` matches filename stem; reject if not.
6. Validate fields (priority 0–4, ID format, RFC3339, status+closed consistency).

**Serialization strategy: hand-rolled `FrontmatterWriter`.**

```rust
// clove-core/src/write.rs
pub struct FrontmatterWriter<W: Write> { inner: W }
impl<W: Write> FrontmatterWriter<W> {
    pub fn write_item(&mut self, item: &ItemFrontmatter) -> Result<()> {
        // Writes fields in canonical order using explicit write!() calls.
        // Lists: always inline flow [a, b, c] sorted lexicographically.
        // Nulls: always "null", never omitted for required fields.
        // Optional fields: omit entirely when None/empty.
    }
}
```

**Why hand-rolled:** serde_yaml_neo does not guarantee field order; a future library upgrade
could silently reorder fields, producing spurious git diffs on every `clove status` call. The
hand-rolled writer is the single authoritative code path for all item writes.

**Atomic write contract:**

```
1. Serialize frontmatter + body to a String
2. Write to .clove/issues/<id>.md.tmp (same directory = same filesystem)
3. fsync the file descriptor
4. std::fs::rename(tmp, final)          (POSIX: atomic; Windows: MoveFileExW REPLACE_EXISTING)
5. On Windows: retry up to 3 times with 10/50/150ms backoff on ERROR_ACCESS_DENIED
```

**File locking for concurrent writers:** take an exclusive `fd-lock` advisory lock on
`.clove/issues/<id>.md` **before reading** (not just before writing). Hold it through rename.
Lock in sorted-ID order when multiple files are touched in one operation (e.g., `dep add`).
Timeout 500ms → return `LockTimeout` error.

**Limits enforced at all entry points (`clove-core/src/limits.rs`):**

```rust
pub const MAX_FRONTMATTER_BYTES: usize = 65_536;     // 64 KiB
pub const MAX_BODY_BYTES: usize = 4_194_304;          // 4 MiB
pub const MAX_DEP_ARRAY_LEN: usize = 1_000;
pub const MAX_ID_LEN: usize = 32;
pub const MAX_PREFIX_LEN: usize = 16;
pub const MAX_ITEMS_NO_INDEX_WARN: usize = 50_000;   // warn, not error
```

---

## 5. Dependency-Graph Engine

### 5.1 In-Memory Representation

```rust
// clove-core/src/graph.rs
use petgraph::stable_graph::StableDiGraph;

pub struct GraphStore {
    graph: StableDiGraph<ItemMeta, EdgeKind>,
    id_to_node: HashMap<CloveId, NodeIndex>,
    node_to_id: Vec<CloveId>,            // parallel array for O(1) reverse lookup
    dangling_ids: HashSet<CloveId>,      // dep references with no backing file
}

pub struct ItemMeta {
    pub id: CloveId,
    pub status: ItemStatus,
    pub title: SmolStr,
    pub item_type: ItemType,
    pub has_dangling_deps: bool,
}

#[repr(u8)]
pub enum EdgeKind {
    DependsOn  = 1,  // hard dep: self → dep (blocking edge)
    ParentOf   = 2,  // hierarchy: parent → child
    Relates    = 3,  // soft, stored as two directed edges
    Duplicates = 4,  // soft, directional
    Supersedes = 5,  // soft, directional
}

pub fn is_hard_dep(e: EdgeKind) -> bool { matches!(e, EdgeKind::DependsOn) }
```

**Why `StableDiGraph`:** NodeIndex values survive node removal (tombstoning), which is
critical for incremental updates.

**Why petgraph over a plain adjacency list:** petgraph provides DFS, BFS, toposort, and SCC
(cycle detection) for free. Rolling these correctly on a HashMap would take weeks and is a
source of subtle bugs. petgraph is used by cargo itself.

### 5.2 Graph Construction

`GraphStore::build(items: &[Item]) -> (GraphStore, Vec<DanglingRef>)` is the sole constructor.
Two-pass:
1. Insert all item IDs as nodes.
2. Insert edges from `deps`/`parent`/`relates`/etc; collect unreferenced dep targets into
   `dangling_ids`.

Items with dangling deps are excluded from `ready_items()` and included in `blocked_items()`
with a `dangling_deps` field.

### 5.3 Ready / Blocked Computation

**`ready_items()`:** Items with `status` open or in_progress where:
- All `DependsOn` neighbors have `status == Closed`.
- `has_dangling_deps == false`.

Computation: O(V+E) after `petgraph::algo::toposort`. Toposort validates the DAG; if it
returns `Err(Cycle)`, fall through to SCC reporting instead of panicking.

**`blocked_items()`:** Returns `BlockedItem { id, blocking_deps: Vec<CloveId>,
dangling_deps: Vec<CloveId> }`. An item is blocked if it has any open `DependsOn` neighbor
OR any dangling dep.

**Partition completeness invariant:** `ready ∪ blocked ∪ closed == all_items`. No item is in
both `ready` and `blocked`. This is a tested invariant.

**Soft relations (Relates, Duplicates, Supersedes) do NOT participate in ready/blocked.**
This is enforced by the `is_hard_dep()` filter. Tested with a dedicated correctness test.

### 5.4 Cycle Detection

**Proactive (at `dep add` time):** Validation pipeline:
1. **Self-loop check (first):** if `dep_id == self_id`, reject immediately with
   `CloveError::ValidationError { code: "SELF_LOOP" }` — exit 4. This is a bad argument,
   not a graph cycle, so it uses `ValidationError` (exit 4), not `CycleDetected` (exit 3).
2. **Cycle-path check:** call
   `petgraph::algo::has_path_connecting(&graph, dep_node, self_node, None)`. If true, the
   proposed edge would create a cycle; reject with
   `CloveError::CycleDetected { path: Vec<CloveId> }` — exit 3.

**Reactive (`clove dep cycle`):** `petgraph::algo::kosaraju_scc` returns all SCCs. Filter
for size > 1. Report as `{ cycles: [[id1, id2, id1]] }`. Exit 0 always (cycles are data, not
errors). Use `--fail-on-cycle` flag for CI to exit 3.

**Items involved in a cycle** are excluded from both `ready` and `blocked` during scan-path
computation; they appear in a third `cycle` bucket in JSON output to prevent infinite
traversal loops.

### 5.5 Dependency Tree Rendering

`dep tree <id>` uses depth-bounded DFS (default `--depth 5`, configurable). Tracks visited
IDs to detect cycles; marks repeated nodes with `cycle_ref: true` without recursing further.

Human output: Unicode tree identical to `cargo tree` (`├──`, `└──`, `│   ` indentation).

JSON tree mode:
```json
{
  "id": "proj-7af",
  "title": "...",
  "status": "open",
  "ready": false,
  "deps": [{ "id": "proj-3k2", "title": "...", "status": "open", "ready": true, "deps": [] }]
}
```

`--flat` flag emits `[{"id":"...", "depth":0, "ready":true}, ...]` for `jq`-friendly
processing.

### 5.6 Epic / Parent Hierarchy

Parent/child edges use `EdgeKind::ParentOf`. They share the same `StableDiGraph` but are
excluded from `DependsOn`-only traversals via `is_hard_dep()`.

**Epic completeness:** Derived field `children_summary: { total: u32, closed: u32 }` for
items of type `Epic`. Direct children only (not recursive) to avoid O(subtree) queries on
deeply nested epics. An epic with all direct children closed is marked `completable: true`
in JSON output; status is not auto-transitioned.

### 5.7 Topological Sort for Output Ordering

`clove ready` output is sorted by `(priority ASC, topological_rank ASC)`, where
`topological_rank` is the item's position in the toposort result. Items earlier in the topo
order (sources of the DAG) surface first — the correct pick-order for agents. With the SQLite
index, `topological_rank` is stored as a column; without it, it is computed per invocation.

---

## 6. SQLite Index

### 6.1 Schema (DDL)

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;
PRAGMA cache_size=-65536;  -- 64 MB page cache

CREATE TABLE items (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    item_type TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 2,
    assignee TEXT,
    parent_id TEXT,
    topological_rank INTEGER,
    has_dangling_deps BOOLEAN NOT NULL DEFAULT FALSE,
    labels TEXT NOT NULL DEFAULT '[]',   -- JSON array
    created_at TEXT NOT NULL,            -- RFC3339
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    file_mtime INTEGER NOT NULL,         -- Unix epoch ms
    content_hash BLOB NOT NULL,          -- first 8 bytes of BLAKE3
    source_system TEXT,
    external_ref TEXT
) WITHOUT ROWID;

CREATE TABLE edges (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    kind INTEGER NOT NULL,               -- EdgeKind u8
    PRIMARY KEY (from_id, to_id, kind)
) WITHOUT ROWID;

CREATE TABLE labels (
    item_id TEXT NOT NULL,
    label TEXT NOT NULL,
    PRIMARY KEY (item_id, label)
) WITHOUT ROWID;

CREATE VIRTUAL TABLE items_fts USING fts5(
    id UNINDEXED,
    title,
    body,
    content='',         -- contentless: FTS index is self-contained; rows managed by upsert_item()
    tokenize='ascii'    -- faster than unicode61 for ASCII-dominant content
);
-- Note: `body` here is the item body text passed explicitly on insert; it is NOT a reference
-- to a column in the `items` table (which has no body column). The `content=''` declaration
-- makes this a contentless FTS5 table — all content must be provided via explicit
-- INSERT/DELETE in upsert_item(). See §6.3 and T-S02.

-- Staleness oracle (exactly one row enforced by the CHECK constraint)
CREATE TABLE meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    dir_mtime INTEGER NOT NULL,
    file_count INTEGER NOT NULL,
    last_git_head TEXT
);
-- Note: `schema_version` is intentionally absent here. Schema version is stored exclusively
-- in `PRAGMA user_version` (see §6.1 rationale below). Writes use `INSERT OR REPLACE INTO
-- meta(id, ...) VALUES (1, ...)` to guarantee only one row ever exists.

-- File fingerprints (for daemon startup sweep + incremental sync)
CREATE TABLE file_mtimes (
    path TEXT PRIMARY KEY,
    mtime_ns INTEGER NOT NULL,
    content_hash BLOB NOT NULL
);

-- Indexes
CREATE INDEX idx_items_status ON items(status);
CREATE INDEX idx_items_priority ON items(priority, topological_rank);
CREATE INDEX idx_edges_to ON edges(to_id, kind);
CREATE INDEX idx_edges_from ON edges(from_id, kind);
CREATE INDEX idx_labels_label ON labels(label);
```

**`WITHOUT ROWID`** on `items` (TEXT PK), `edges` (composite PK), `labels` (composite PK)
avoids the hidden integer rowid and gives O(log n) point lookups directly on the natural key.

**`content_hash`** is first 8 bytes of BLAKE3 (not xxHash3; BLAKE3 is zero-unsafe in Rust
and SIMD-accelerated). Stored as `BLOB(8)`.

**Schema version** is stored in `PRAGMA user_version` (built-in SQLite mechanism, more
reliable than a custom table row). Checked on every `Index::open`. Mismatch → drop and
rebuild. Current version is **v4** (v2 covering index + sentinel rank; v3
`file_mtimes.synced_at`; v4 `items.excluded`, the persisted hard-cycle /
malformed-parent flag the SQL `ready` query filters on — see §6.5). The same `index.db`
also holds a durable `snapshots` history table (`clove stats --snapshot`/`--history`),
created idempotently and **carried across reindex / schema-rebuild** so it is not a
cache-only artifact (M4).

### 6.2 Staleness Detection (Two-Level)

**Level 1 — directory mtime (O(1) syscall):**
```
stat(.clove/issues/) → compare st_mtime and file count against meta table
```
If matching → index is current for common case.

**Level 2 — per-file hash (only for changed files):**
On dir mtime mismatch: `read_dir` to collect `(path, mtime, size)` tuples; compare against
`file_mtimes` table. For files where mtime or size differs: compute BLAKE3 hash, compare
against stored hash. Only re-parse YAML if hash differs.

**Special case for macOS HFS+:** If `now - file_mtime < 2 seconds`, always recheck hash
regardless of stored mtime (HFS+ 1-second mtime granularity).

**After `git checkout`:** Detect HEAD change by comparing `.git/HEAD` content against
`meta.last_git_head`. On HEAD change: force a full `read_dir` staleness pass (not just
targeted). Update `last_git_head` after.

### 6.3 Write-Through Consistency

Every CLI command that mutates a file (new, edit, status, label, dep, assign, priority) does:
1. Write the `.md` file (atomic rename, see §4).
2. Upsert the SQLite rows (items + edges + labels + FTS5 sync) in a single `BEGIN IMMEDIATE`
   transaction.

If the SQLite upsert fails: log to stderr and continue (file is the truth; index is stale
but recoverable). File writes are always attempted first.

**FTS5 sync:** All item upserts must use the encapsulated `upsert_item(conn, item)` function —
the single write path — which always syncs FTS5 in the same transaction. Direct SQL writes to
`items` outside this function are forbidden.

### 6.4 Auto-Refresh Threshold

On any read command: run Level 1 staleness check (~10 µs). If stale_count ≤ 20: apply
incremental resync inline before running the query (transparent to user). If stale_count > 20
OR > 5% of total items: print one-line warning to stderr and use file-scan fallback.
`index.auto_refresh = false` in `config.toml` disables auto-refresh entirely (for agents that
control all writes).

### 6.5 Ready Query (SQL)

```sql
SELECT i.*
FROM items i
WHERE i.status IN ('open', 'in_progress')
  AND i.has_dangling_deps = FALSE
  AND NOT EXISTS (
      SELECT 1
      FROM edges e
      JOIN items dep ON e.to_id = dep.id
      WHERE e.from_id = i.id
        AND e.kind = 1            -- EdgeKind::DependsOn
        AND dep.status != 'closed'
  )
ORDER BY i.priority ASC, i.topological_rank ASC;
```

### 6.6 Reindex Protocol

`clove reindex` writes to `index.db.tmp`, rebuilds, then renames to `index.db`. This means a
crashed reindex never corrupts the live index. Concurrent reindex prevention: PID-based
lockfile at `.clove/reindex.lock`.

Steps:
1. Open `index.db.tmp`.
2. `BEGIN EXCLUSIVE` transaction.
3. Create all tables and indexes.
4. Parse all `.md` files in parallel (rayon, above 500 items; sequential below).
5. Insert in batches of 500.
6. Update `file_mtimes` for all files.
7. Compute topological sort; update `topological_rank`.
8. `COMMIT`. Write `meta` row last (so a crash before this step leaves `schema_version` unset,
   detectable as corrupt).
9. `PRAGMA wal_checkpoint(TRUNCATE)`.
10. `rename(index.db.tmp, index.db)`.

Target: < 1 second for 10,000 items on an SSD.

**`PRAGMA synchronous=OFF`** during reindex (reset to NORMAL after) for maximum throughput,
with the understanding that a crash during reindex is detected via `PRAGMA user_version` check.

### 6.7 Missing Index Fallback

Missing `index.db`:
- Print to stderr (not stdout): `note: no index found, using file scan (run 'clove reindex' to build)`
- Use file-scan for all operations.
- `clove search` degrades to parallel rayon substring scan over file content.

All JSON responses include `"_meta": { "source": "files" | "index", "took_ms": N }`.

---

## 7. CLI Command Reference

### 7.1 Global Flags

```
clove [GLOBAL FLAGS] <subcommand> [SUBCOMMAND FLAGS]

Global flags:
  -f, --format <human|json|jsonl>   Output format (default: human)
                                    Overridden by CLOVE_FORMAT env var
                                    Precedence: flag > CLOVE_FORMAT > config.toml default > human
  --no-index                        Force file-scan even if index.db present
  --quiet                           Suppress informational stderr
  --color <auto|always|never>       Terminal color control
  --clove-dir <PATH>                Override .clove/ discovery
```

### 7.2 Complete Command Table

```
clove init [--prefix STR] [--no-gitignore] [--merge-driver]
clove new <title> [-t|--type bug|feature|chore|docs|epic]
          [-p|--priority 0-4] [-l|--label KEY:VAL ...]
          [-d|--dep ID ...] [--parent ID] [--assignee STR]
          [--body STR | --body-file PATH] [--format json]
clove show <id> [--format json] [--fields FIELD,...]
clove edit <id>                             # opens $EDITOR
clove edit <id> --field KEY=VALUE [--field KEY=VALUE ...]  # non-interactive
clove set <id> <field>=<value> [<field>=<value> ...]        # agent alias
clove status <id> <open|in_progress|closed> [--format json]
clove start <id>                            # alias: status in_progress
clove close <id>                            # alias: status closed
clove label <id> add <label>
clove label <id> rm <label>
clove assign <id> <assignee>
clove priority <id> <0-4>
clove dep add <id> <dep-id>
clove dep rm <id> <dep-id>
clove dep tree <id> [--depth N] [--full] [--flat] [--format json]
clove dep cycle [--fail-on-cycle] [--format json]
clove ready [--status open|in_progress] [--type T] [--label L]
            [--assignee A] [--priority N] [--limit N] [--offset N]
            [--format json] [--fields F,...] [--include-warnings]
clove blocked [same filters] [--format json] [--fields F,...]
clove ls [--status S] [--type T] [--label L] [--assignee A]
         [--priority N] [--sort id|priority|created|updated]
         [--asc|--desc] [--limit N] [--offset N]
         [--format json] [--fields F,...]
clove query [--filter EXPR] [--format json] [--fields F,...]
            # also reads JSON filter object from stdin when stdin is non-TTY
clove search <text> [--limit N] [--format json]
clove stats [--top N] [--no-epics] [--snapshot]      # work-item analytics + daemon/index telemetry
            [--history [--since RFC3339] [--limit N]] [--format json]
clove comment <id> <message> [--format json]
clove comments <id> [--limit N] [--format json]
clove reindex [--force] [--format json]
clove import [--format json] <json|jsonl> <file> [--dry-run] [--overwrite]  # BUILT-IN native restore (inverse of export);
clove import [--format json] <beads|tk> <src> [--dry-run]   # tk/beads are clove-import-<p> plugins;
clove export [--format json] <json|jsonl> [--out FILE]      # export json/jsonl are built-in; clove global flags precede the
                                                            # provider, provider-owned flags (<src>/--out/--dry-run) follow it
clove agent-doc [--format markdown|json] [--out FILE]
clove agent-doc --check [--file PATH]       # verify embedded doc vs binary
clove migrate [--yes] [--dry-run]
clove doctor [--fix] [--strict] [--format json]
clove daemon start|stop|status [--format json]
clove completions <bash|zsh|fish|powershell>
clove version [--format json]
```

**`clove edit --field`** supports: `status`, `priority`, `assignee`, `type`,
`labels+=key:val` (append), `labels-=key:val` (remove). This is the agent-safe non-interactive
edit path; `$EDITOR` is never opened from an agent subprocess.

### 7.3 JSON Output Envelope

Every response is a top-level object. Never a bare array.

**Success:**
```json
{
  "v": 1,
  "ok": true,
  "data": <payload>,
  "_meta": {
    "took_ms": 4,
    "source": "index",
    "warnings": [],
    "stale_index": false
  }
}
```

**Error:**
```json
{
  "v": 1,
  "ok": false,
  "error": {
    "code": "ITEM_NOT_FOUND",
    "message": "no item with id proj-7AF3K2MN",
    "exit": 2
  }
}
```

**Errors are emitted on stdout** (not stderr). Stderr is human-readable narrative only.
When `--quiet` is set, all stderr is suppressed. JSON parsers receive valid JSON on stdout
regardless of warnings.

**`jsonl` format:** one envelope per line, `"data"` is a single item (not array). Enables
`clove ls --format jsonl | while read line; do ...; done`.

### 7.4 Item JSON Schema (v1)

`clove show <id> --format json` returns:
```json
{
  "v": 1, "ok": true,
  "data": {
    "id": "proj-7af",
    "title": "...",
    "status": "open",
    "type": "feature",
    "priority": 1,
    "labels": ["area:core"],
    "assignee": null,
    "deps": ["proj-3k2"],
    "parent": null,
    "relates": [],
    "duplicates": [],
    "supersedes": [],
    "created": "2026-06-02T10:00:00Z",
    "updated": "2026-06-02T14:23:00Z",
    "closed": null,
    "body": "...",
    "comment_count": 2,
    "ready": false,
    "blocked_by": ["proj-3k2"],
    "dangling_deps": [],
    "warnings": [],
    "children_summary": null,
    "source_system": null,
    "external_ref": null
  }
}
```

**`ready` and `blocked_by` are computed at serialization time, never stored.**

### 7.5 List Response Schema (v1)

```json
{
  "v": 1, "ok": true,
  "data": [{ ...item... }],
  "_meta": {
    "took_ms": 4,
    "source": "index",
    "total": 250,
    "returned": 100,
    "offset": 0,
    "warnings": [],
    "stale_index": false
  }
}
```

`total` is the unfiltered count; `returned` is `len(data)` after `--limit`.

### 7.6 Exit Code Table

| Code | Name | Meaning |
|---|---|---|
| 0 | Success | Data returned (possibly empty) |
| 1 | UsageError | Bad flag, unknown subcommand, argument parse error |
| 2 | NotFound | Item does not exist |
| 3 | CycleDetected | `dep add` when a cycle-path is detected; also used with `--fail-on-cycle` flag on `dep cycle` |
| 4 | ValidationError | Bad field value, ID collision, invalid priority |
| 5 | IoError | `.clove/` missing, file unreadable, filesystem error |
| 6 | IndexError | Stale index with `--strict`; index unrecoverable |
| 7 | DaemonError | Daemon communication failure |

### 7.7 Health Check (`clove doctor`)

`clove doctor` validates the whole store for integrity problems that per-file
parsing/`validate_item()` cannot catch on its own — primarily **cross-item**
issues introduced by hand-edits, bad merges, or partial writes. It loads every
file once, builds the graph, and runs the check suite below.

**Checks (M0, file-only):**

| # | Check | Severity | Fixable by `--fix` |
|---|---|---|---|
| 1 | **Unparseable file** (malformed/oversized frontmatter, alias bomb, unknown field) | error | no |
| 2 | **ID/filename mismatch** (`id` ≠ filename stem) | error | no |
| 3 | **Duplicate ID** across two files | error | no |
| 4 | **Invalid field** (priority ∉ 0–4, bad type/status, bad RFC3339, `status`/`closed` invariant broken, unsupported `schema`, list length > `MAX_DEP_ARRAY_LEN`) — runs `validate_item()` | error | no |
| 5 | **Dangling reference** (`deps`/`parent`/`relates`/`duplicates`/`supersedes` → missing ID) | error | no¹ |
| 6 | **Dependency cycle** among `DependsOn` edges (reports each SCC path) | error | no |
| 7 | **Invalid parent** (self-parent, parent → missing, parent cycle) | error | no |
| 8 | **Non-canonical label** (not equal to `normalize_label()` output, or duplicate after normalization) | warning | **yes** (normalize + de-dup + re-sort) |
| 9 | **Unsorted/duplicate list field** (`deps`/relations not sorted/deduped per §2.2) | warning | **yes** (re-sort + de-dup) |
| 10 | **Orphaned comments dir** (`<id>/comments/` with no `<id>.md`) | warning | **yes** (remove dir) |
| 11 | **Config invalid** (`id_prefix`/`id_length`/`default_type` out of spec) | error | no |
| 12 | **Incoherent timestamps** (`updated` < `created`, `closed` < `created`, or any timestamp >24 h in the future) | warning | no |
| 13 | **`.clove/.gitignore` drift** (file absent, or missing a required entry — the cache/socket/pid/lock set of §2.1/§8.2) | warning | **yes** (append the missing canonical entries; user-added lines preserved) |

¹ Dangling/cycle/structural issues are **report-only** — auto-removing a dep
could silently drop intent. `--fix` only performs the clearly-safe repairs
(label normalization, list de-dup/sort, orphaned-dir removal, and
`.clove/.gitignore` top-up). Check 12 is report-only: the *intended* time can't
be inferred. Check 13's canonical entry list lives in `clove_core` and is shared
with `clove init`, so the two never drift.

**M1 extension:** when an index is present, `doctor` also runs the index-health
checks via the non-healing `Index::open` (so problems are reported, not silently
rebuilt away): **schema-version mismatch** (`INDEX_SCHEMA_MISMATCH`, warning),
**internal corruption** (`INDEX_CORRUPT`, error — `PRAGMA quick_check` plus a
contentless-FTS `fts_map`↔`items` row-count cross-check), and **index↔files
divergence** (`INDEX_DIVERGENCE`, warning, counts/hashes via the staleness
machinery). All three are fixable: `--fix` triggers a single `reindex` from the
files (the source of truth) and re-checks. Skipped under `--no-index`.

**M3 extension (daemon footprint):** `doctor` classifies the `daemon.sock`/
`daemon.pid` footprint without mutating it. A **dead** footprint (files present,
nothing answers, process gone) is a fixable `DAEMON_STALE_SOCKET` (`--fix` removes
the corpse files, §8.3). A **live but protocol-incompatible** daemon — e.g. an old
`cloved` still running after a `clove` upgrade bumped the IPC `PROTOCOL_VERSION` —
is a **non-fixable** `DAEMON_VERSION_SKEW` (a restart is the remedy); crucially
`--fix` must never delete a *running* process's socket/pid. A live, healthy daemon
yields no finding.

**Output:** one issue per finding: `{ severity, code, item: <id|path|null>,
message, fixable }`, plus a summary `{ errors, warnings, fixed, checked }`. JSON
mode uses the standard envelope (§7.3) with the issue list in `data`, validated
against `docs/json-schema/v1/doctor.json` (whose `code` enum is the canonical
check taxonomy — adding a check extends it, guarded by the schema round-trip
test).

**Exit codes:** `0` when no error-severity issues remain (warnings alone still
exit 0 — issues are data, mirroring `dep cycle`). With **`--strict`**, any
remaining error → exit `4` (ValidationError), so CI can gate on a clean store.
`--fix` applies safe repairs first, then evaluates the exit condition against
what's left.

**`clove ready` with zero results: exit 0 with `{ "v":1, "ok":true, "data":[] }`.**
Never use non-zero to signal empty result set.

**`clove dep cycle` always exits 0** (cycles are data, not errors). Use `--fail-on-cycle` for
CI to get exit 3.

Clap's default exit code (2 for argument errors) is overridden to match this table. Use
`std::process::ExitCode` (stable since Rust 1.61), never `process::exit()` directly.

### 7.8 Pagination

All list commands support `--limit N` (default 0 = unlimited for `--format json`; 50 for
human) and `--offset N`. Cursor-based pagination is deferred to post-v1; offset is sufficient
for M0–M2.

### 7.9 `clove agent-doc`

Generates a self-contained workflow document suitable for AGENTS.md/CLAUDE.md. Contents:
1. What clove is and its file layout.
2. The pick-work loop with exact command examples.
3. JSON schemas for item/list/error envelopes with annotated examples.
4. Exit codes table.
5. How to add deps and detect cycles.
6. CLOVE_FORMAT env var reference.
7. Embedded `<!-- generated-by: clove vX.Y schema:N -->` comment.

`clove init` prints a one-line hint but does not auto-write AGENTS.md (to avoid clobbering
existing content). `clove agent-doc --check --file AGENTS.md` detects stale embedded schema
version.

---

## 8. Daemon Design

### 8.1 Process Model

- Single long-lived `cloved` process **per `.clove/` directory** (per project) —
  never system-wide. The socket, pid, lock, and Windows pipe/event names are all
  derived from the resolved `.clove/` path, so the daemon is keyed to the project,
  not the cwd, and is reachable from any subdirectory of it.
- `tokio::runtime::Builder::new_multi_thread()` with 2 worker threads (watcher + IPC).
- **One daemon per repository, shared across all worktrees.** `clove` is a
  per-project tracker: work items belong to the *project*, not a branch, so **all
  git worktrees of a project share the main worktree's `.clove/`** (and thus one
  index + one daemon). `find_repo_root` (clove-core `repo.rs`) resolves a linked
  worktree — even one with its own checked-out `.clove/` — to the main worktree's
  `.clove/` via `git rev-parse --git-common-dir`; the daemon keys on that shared
  path, and the per-directory `daemon.lock` makes it a singleton. The main worktree
  stays subprocess-free (its `.git` is a directory); only linked worktrees pay the
  one `git` call. A system-wide multiplexing daemon was evaluated and rejected for
  v1 (no hot-path speedup; large lifecycle/security/blast-radius cost).

### 8.2 Socket / PID Layout

| File | Purpose | Cleanup |
|---|---|---|
| `.clove/daemon.sock` (Unix) / `\\.\\pipe\\clove-<repo-hash>` (Windows) | IPC transport | daemon removes on clean shutdown |
| `.clove/daemon.pid` | PID for `clove daemon stop` | daemon removes on clean shutdown |
| `.clove/daemon.lock` | Prevents two daemons starting simultaneously | held by daemon process |
| `.clove/reindex.lock` | Prevents concurrent `clove reindex` | held for reindex duration |

`.clove/.gitignore` (written by `clove init`) must contain all of the above, plus
`index.db.tmp` (the temporary reindex file from §6.6). See §2.1 for the complete gitignore
entry list.

**Daemon writes `daemon.pid` only after binding the socket** — so the CLI never reads a PID
without a usable socket.

### 8.3 CLI Liveness Detection

1. Check for `.clove/daemon.sock`. If absent → fallback.
2. Attempt connect with 50ms timeout. On `ECONNREFUSED` or timeout → delete stale
   `daemon.sock` and `daemon.pid` → fallback.
3. Send `PING` → on `PONG` → proceed with IPC.

Fallback: direct SQLite access (WAL shared flock) or file-scan.

### 8.4 IPC Protocol

**Transport:** Unix domain socket (macOS/Linux) / Windows named pipe, via the `interprocess`
crate's `LocalSocketListener` / `LocalSocketStream` abstraction.

**Frame format:** 4-byte little-endian length prefix + UTF-8 JSON payload.

**v1 command set:**
```
PING → PONG
QUERY { filter, format, fields } → { ok, data, _meta }
REINDEX → REINDEX_DONE { items_indexed, duration_ms, warnings }
STATUS → { uptime_s, items_indexed, watcher_state, last_event_ms }
```

### 8.5 File-Watcher

- **Crate:** `notify` 6.x stable (NOT 9.0.0-rc.4). Pin to `notify = "6"` with an explicit
  comment in `Cargo.toml` explaining the version choice.
- **Watch:** `.clove/issues/` recursively. Filter to `*.md` events only.
- **Exclude:** `.clove/index.db`, `.clove/index.db-wal`, `.clove/index.db-shm` to prevent
  feedback loops.
- **Debounce:** 200ms per-file (reset timer on each new event for the same path).
- **Batch:** all events within a 200ms window → single SQLite transaction.

### 8.6 Startup Mtime Sweep

Before advertising readiness (writing `daemon.pid`): query `file_mtimes`, scan directory,
find changed files, re-parse and re-index. This covers files changed while daemon was stopped
(e.g., `git pull`). Must complete before daemon marks itself ready.

### 8.7 Git Auto-Sync (Disabled by Default)

Enabled via `[daemon] git_sync = true` in `config.toml`.

Behavior: after successful index update for a file, check `git diff --name-only HEAD -- <path>`.
If modified-but-uncommitted AND frontmatter parses cleanly (malformed-skip rule): run
`git add <path> && git commit -m 'clove: auto-sync <id> [<change_type>]'`.

Skip conditions:
- `.git/MERGE_HEAD` exists (active merge).
- `.git/rebase-merge/` exists (active rebase).
- Frontmatter fails to parse (mid-edit autosave guard).

Track `synced_at` in `file_mtimes` to prevent re-committing on the inotify event generated by
the git index update.

**Never push.** Push is always the user's responsibility.

### 8.8 Daemon Config Section

```toml
[daemon]
git_sync = false          # opt-in auto-commit
watch_debounce_ms = 200   # per-file debounce window
idle_shutdown_min = 240   # default 4h; 0 = never; N = self-terminate after N idle minutes
stats_snapshot_min = 60   # auto-record a `clove stats` history point every N min; 0 = off
```

**Idle self-shutdown defaults to 4 hours.** Every clove command (and watcher
batch) is a heartbeat that resets the idle timer (`DaemonState::mark_event`), so
an actively-used daemon never times out — it only self-terminates after this long
with *no* clove activity at all, keeping process count bounded without a manual
`clove daemon stop`. **There is no auto-restart yet:** after an idle shutdown the
next read falls back to the local path until `clove daemon start` (a future MCP
server would hold a session heartbeat to keep the daemon alive). Set `0` to keep a
daemon running indefinitely; `CLOVED_IDLE_SHUTDOWN_MS` overrides it (sub-minute)
for tests/CI.

### 8.9 SIGTERM / Clean Shutdown

**Unix (macOS/Linux):** Daemon catches SIGTERM via
`tokio::signal::unix::signal(SignalKind::terminate())`.

**Windows:** No SIGTERM. Use `tokio::signal::ctrl_c()` for interactive shutdown. For daemon
stop from the CLI, use a named shutdown event: `CreateEventW` with a well-known name derived
from the repo hash; `clove daemon stop` signals the event; the daemon's event-loop wakes,
runs the shutdown sequence, and exits.

Shutdown sequence (all platforms):
1. Flush pending debounce batches to index.
2. `PRAGMA wal_checkpoint(TRUNCATE)`.
3. Close SQLite connection.
4. Remove `daemon.sock` (Unix) / named-pipe handle (Windows) and `daemon.pid`.
5. Exit 0.

---

## 9. Git / Merge Semantics

### 9.1 Conflict Minimization by Construction

| Scenario | Result |
|---|---|
| Two agents `clove new` on separate branches | Two unique files; zero conflicts |
| Two agents update different fields of same item | Single-line conflict in frontmatter |
| Two agents set same field to same value | Auto-resolved by merge driver (same-value) |
| Two agents append comments to same item | Two distinct comment files; zero conflicts |
| Two agents add different deps to same item | Union-merged by merge driver |

### 9.2 Git Merge Driver

Installed optionally via `clove init --merge-driver`.

**`.gitattributes` (committed):**
```
.clove/issues/*.md merge=clove-item
```
The glob is intentionally narrow (`*.md`, not `**/*.md`): comment files live under
`<id>/comments/` and are append-only, uniquely named, and never merged by the item
driver, so they are deliberately excluded from it.

**`.git/config` (local, not committed):**
```ini
[merge "clove-item"]
  name = clove item merge driver
  driver = clove merge-driver %O %A %B %L
  recursive = binary
```

**`clove merge-driver <ancestor> <ours> <theirs> <marker-size>` algorithm:**
1. Parse all three files' frontmatter.
2. For each scalar field: same-value conflict → accept; standard 3-way otherwise.
3. For each list field: compute `union(ours, theirs) \ (ancestor \ ours \ theirs)` (standard
   three-way set merge), then sort. Dep removal conflict (A removes X, B adds X) → flag
   conflict.
4. For the body: delegate to `git merge-file`.

**Union-merge semantics for `deps`:** if branch A adds `proj-3k2` and branch B adds
`proj-7af`, both survive the merge in sorted order. This is the highest-value semantic merge
operation.

### 9.3 Parallel-Branch ID Safety

Random 8-char Crockford IDs make every `clove new` on every branch produce a globally-unique
filename with overwhelming probability (collision probability at 10,000 items = 0.005%).
`git merge` on parallel branches simply adds both files — no conflict.

### 9.4 Post-Merge Index Behavior

After `git merge` or `git pull`, the SQLite index is stale. Options:
1. **Daemon running:** detects file-watch events, incrementally reindexes automatically.
2. **No daemon:** the next read command's staleness check detects the mtime change and
   incrementally resyncs.
3. **Explicit:** `clove reindex`.

Agents' AGENTS.md should note: "after `git merge` or `git pull`, the index refreshes
automatically on the next command."

---

## 10. Configuration Format

**File:** `.clove/config.toml` (committed to the repo, shared across all clones)

```toml
# Schema version of this config file (not item schema version)
config_schema = 1

# ID prefix (auto-derived from repo name on init, overridable)
id_prefix = "proj"

# Number of random Crockford base32 chars in new IDs (min 4, max 12)
id_length = 8

# Default item type for `clove new` without -t
default_type = "feature"

# Default output format: "human" | "json" | "jsonl"
# Overridden by CLOVE_FORMAT env var and --format flag
default_format = "human"

# Index auto-refresh (set false for agent-controlled repos)
[index]
auto_refresh = true

# Daemon settings
[daemon]
git_sync = false
watch_debounce_ms = 200
idle_shutdown_min = 0
stats_snapshot_min = 60   # auto-record a `clove stats` history point every N min; 0 = off
```

**Validation rules (enforced on every startup, not just on `init`):**
- `id_prefix`: matches `^[a-z][a-z0-9]{0,7}$` (1–8 alphanumeric, starts with alpha).
- `id_length`: 4 ≤ value ≤ 12.
- `default_type`: must be a valid `ItemType` variant.

**Config loading precedence:** compiled-in defaults → `.clove/config.toml` → `CLOVE_*` env
vars (e.g. `CLOVE_ID_PREFIX`, `CLOVE_FORMAT`). No user-global config in v1.

**Repo root discovery:** walk ancestor directories looking for `.clove/`. Implemented as a
pure Rust loop using `camino::Utf8Path` — no subprocess call to `git rev-parse`. Worktree
awareness: use `git2` crate to find the main worktree's `.clove/` if running from a linked
worktree.

---

## 11. Import / Export

### 11.1 tk Import

```
clove import tk <path-to-.tickets-dir>
```

Field mapping (`tk` → `clove`):
- `id` → `id` (preserved as-is)
- `status` → `status`
- `deps` → `deps`
- `type` (task→chore) → `type`
- `priority` → `priority`
- `assignee` → `assignee`
- `external-ref` → `external_ref` (hyphen → underscore)
- `parent` → `parent`
- `tags` → `labels`
- `links` → `relates`
- First `# H1` in body → `title` (stripped from body); filename used as fallback with a warning
- `source_system` = `"tk"`

### 11.2 Beads Import

```
clove import beads <path-to-issues.jsonl>
```

Field mapping (`beads` → `clove`):
- `id` → `id`
- `title` → `title`
- `description` → body
- `status` (deferred → open + label `deferred`) → `status`
- `priority` → `priority`
- `issue_type` (task→chore) → `type`
- `assignee`/`owner` → `assignee`
- `labels` → `labels`
- `dependencies[type=blocks]` → `deps`
- `dependencies[type=parent-child]` → `parent` (first parent only)
- `dependencies[type=related|tracks|etc]` → `relates`
- `external_ref` → `external_ref`
- All unmapped Beads-internal fields → `metadata` JSON blob (stored in `external_ref` as
  `beads-meta:<json>` prefix, or as a structured comment in the body)
- `source_system` = `"beads"`

**Critical:** `.beads/issues.jsonl` includes `comment_count` but NOT comment bodies. Items
with `comment_count > 0` emit a warning to stderr listing the IDs and suggesting
`bd show --json <id>` to extract comment bodies. The importer must NOT silently succeed with
missing comment data.

### 11.3 GitHub Sync

GitHub is reached through **one** command — `clove sync github <owner/repo>`, a
two-way reconcile (the earlier one-way `import github` / `export github` were
removed in favour of it). It is a **cargo-style plugin**: `clove sync github`
resolves and runs the separately-installed `clove-sync-github` binary (which
carries `octocrab` behind `clove-import`'s `github` feature), so the core
`clove`/`cloved` are octocrab-free. See `docs/PLUGIN_SYSTEM.md`.

**Field mapping.** `number` ↔ `external_ref = "gh-<number>"` (the durable link;
clove mints a fresh `CloveId` on pull-create), `title` ↔ `title`, `state`
(`open`/`closed`) ↔ `status` (`closed` → `Closed`), `labels[].name` ↔ `labels`,
`assignees[0].login` ↔ `assignee`, `closed_at` → `closed`, and `body` (minus the
`clove-meta` comment) ↔ body. clove-only fields (`deps`/`priority`/`id`) ride a
`<!-- clove-meta: {...} -->` HTML comment in the issue body. Pulled items get
`source_system = "github"`.

**Link write-back:** a *created* issue's number is written back onto the local
item (`external_ref = "gh-<number>"`), so the next sync UPDATES it rather than
creating a duplicate.

**Two-way reconcile:** one pass that pulls remote changes *and* pushes local
changes, plus bidirectional issue-comment sync. A per-repo last-sync fingerprint
store (`external_ref → {gh_updated_at, local_updated}`, persisted under
`.clove/sync/`, git-ignored) makes "changed since
local_updated}`, persisted under `.clove/sync/`, git-ignored) makes "changed since
last sync" decidable in both directions:

| remote changed | local changed | action |
|---|---|---|
| no  | no  | in sync (skip) |
| yes | no  | pull (update local) |
| no  | yes | push (update remote) |
| yes | yes | **conflict** → resolved by `--prefer` policy |

The conflict policy defaults to `newer` (most recently edited side wins, compared
via GitHub `updated_at` vs the item's `updated`); `local`/`remote` force a side,
`manual` reports without applying. Every conflict is reported regardless. With no
prior sync for an already-linked pair, the planner falls back to a content
comparison so a first sync never silently clobbers a side. Pulls/updates go
through the unified write path (`apply_edit`); comment dedup uses GitHub comment
ids (pull) and stable body hashes (push). `--dry-run` plans without touching
either side; `--no-comments` skips comment reconciliation. A running daemon can
run the sync on a timer (`[daemon] github_sync_interval_min` + `github_sync_repo`).

Fields clove's model can't represent are still preserved on push (using the live
issue + sync state, so no item-schema change): extra GitHub **assignees** a human
added survive a clove push (clove replaces only the assignee it owns — and an
unassign locally *does* clear clove's assignee on GitHub), and a human's close
**`state_reason`** (`not_planned`) is not reset to `completed`.

A non-dry-run sync takes an **advisory lock** (`.clove/sync/github/<repo>.lock`)
for its duration, so a daemon timer can't interleave with a manual `clove sync`
of the same repo and mint duplicate issues; a second concurrent sync fails cleanly
("already in progress").

### 11.4 Export

```
clove export json         → single JSON envelope with all items
clove export jsonl        → one item per line (NDJSON), clove's native item schema
clove sync   github       → two-way GitHub reconcile (pull + push + comments, §11.3)
```

Both `json` and `jsonl` export in clove's **native item schema** — the exact inverse
of `import json`/`jsonl` (§11.5), for backup/restore and cross-repo copy. A
Beads-native export (isomorphic with `.beads/issues.jsonl`) is the `beads` *plugin*
(`clove export beads`), not this built-in.

### 11.5 Native round-trip (`import json`/`jsonl`) and format versioning

`import json` / `import jsonl` are **built-in** (like their export counterparts —
clove's own serialization is core; only *foreign* trackers, §11.1–11.3, are
plugins). They are the exact inverse of `export json`/`jsonl`: a **verbatim,
id-preserving restore**. `clove export json > a.json` then `clove import json
a.json` into another repo reproduces every item exactly — same ids, status
(incl. the `closed` timestamp), type, priority, labels, deps/relations,
`parent`, `source_system`/`external_ref`, `created`/`updated`, and body. This is
a backup/restore and cross-repo copy path (repo→repo transfer of the *files*
still happens via git; this is the serialized-snapshot path).

- **Preserve, don't re-mint.** Unlike the foreign importers (which mint new clove
  ids and set `external_ref` to the source id), the native importer writes each
  item under its existing id via `ItemStore::restore_item` — the same atomic-write
  + validation path as `create`/`update`, but no id minting and no re-stamping.
- **Idempotent.** An id already present is **skipped** by default; `--overwrite`
  restores over it; `--dry-run` plans without writing. The report is
  `{ created, skipped, overwritten }`.
- **Comments are not included.** The export carries `comment_count` only, not
  comment bodies (comments are append-only sidecar files, §2.5), so the native
  round-trip is item-level; comments travel via git.

**Format versioning (for migrations).** The export is self-describing on two axes
so a future data-model change stays readable:

1. **Per-item `schema`** — every exported item carries its frontmatter schema
   version (§2.4). Present in both `json` and `jsonl`.
2. **Container format** — `export json` stamps
   `_meta.clove_export = { format: <EXPORT_FORMAT_VERSION>, item_schema:
   <CURRENT_SCHEMA_VERSION> }`.

On import: a **container `format` newer** than this binary is a hard reject (exit
4, "produced by a newer clove — upgrade"); a **per-item `schema` newer** than
`CURRENT_SCHEMA_VERSION` is a per-item warning-and-skip (so one future item never
aborts a batch, and `jsonl` — which has no container header — still guards each
line). An **older** per-item schema is where a future `v(N-1)→vN` migration
hooks in (identity today, since the only version is `1`). Adding a schema version
therefore only ever *extends* the migration seam; old exports stay importable.

---

## 12. Security and Robustness

### 12.1 Path Traversal Prevention

`CloveId` validates on construction against `^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$`. This
allowlist rejects `/`, `\`, `.`, `%`, control chars, and `..` sequences. Every code path
that maps an ID to a filesystem path uses `CloveId::to_path()` which additionally calls
`std::fs::canonicalize` on the parent directory and asserts the result is a child of the
canonical `.clove/issues/` root.

### 12.2 YAML Injection / Bomb Prevention

Pre-scan frontmatter bytes for `&` (value-position anchor) and `*` (alias). Reject with
`ParseError::AliasNotAllowed` before calling serde_yaml_neo. The 64 KiB frontmatter byte cap is
enforced before any allocation.

### 12.3 Symlink Safety

`ItemStore::scan()` uses `std::fs::read_dir` and skips any `DirEntry` where
`entry.file_type()?.is_symlink()`. Never follows symlinks into or out of `.clove/issues/`.

### 12.4 Concurrent Writer Safety

File locking (`fd-lock`) before read-modify-write. WAL mode + `busy_timeout=5000` for SQLite.
Atomic rename for file writes. See §4 for the complete protocol.

---

## 13. Performance Architecture

### 13.1 Concrete Targets

All wall-clock, measured with `hyperfine --warmup 3 --runs 20`. Cold = OS page cache cleared.

| Operation | Cold | Warm |
|---|---|---|
| `clove version` | < 5 ms | < 3 ms |
| `clove show <id>` (fast path, no graph) | < 5 ms | < 3 ms |
| `clove show <id>` (full, with ready/blocked_by) | < 80 ms | < 25 ms |
| `clove new` | < 10 ms | < 8 ms |
| `clove status <id> closed` | < 10 ms | < 8 ms |
| `clove ls` 100 items, file-scan | < 10 ms | < 5 ms |
| `clove ls` 1,000 items, file-scan | < 50 ms | < 15 ms |
| `clove ready` 1,000 items, file-scan | < 80 ms | < 25 ms |
| `clove ls` 1,000 items, index | — | < 5 ms |
| `clove ls` 10,000 items, index | — | < 10 ms |
| `clove ready` 10,000 items, index | — | < 10 ms |
| `clove search` 10,000 items, FTS5 | — | < 20 ms |
| `clove ready` 100,000 items, index | — | < 100 ms |
| `clove reindex` 1,000 items | — | < 500 ms |
| `clove reindex` 10,000 items | — | < 1,000 ms |

File-scan path at > 50,000 items is out-of-target for the 100 ms bound; the index is
required above that threshold.

### 13.2 I/O Strategy

- **Directory read:** `std::fs::read_dir` — sequential, no parallelism.
- **File reads:** sequential (parallel reads on macOS APFS are 2× slower due to metadata
  serialization; parallel reads are a known performance trap).
- **Parse:** `rayon::par_iter()` if item count > 500; plain `iter()` below that threshold.
  Rayon thread-pool wake-up (~200 µs) exceeds the gain for small sets.

### 13.3 Memory Targets

| Operation | Peak RSS |
|---|---|
| `clove ls` 1,000 items | < 10 MB |
| `clove ls` 10,000 items | < 50 MB |
| `clove ready` 100,000 items (index) | < 8 MB |
| `clove reindex` 100,000 items | < 500 MB |

Body text is never materialized during `ls`/`ready`/`blocked`/`query`. The `scan_lazy()`
path parses only `ItemFrontmatter` (no body allocation). `Item { frontmatter, body: String }`
is constructed only on the full-load path (`clove show`, FTS5 indexing, reindex). This keeps
peak RSS low for bulk scan operations.

### 13.4 Comparative Benchmark Methodology

Use `hyperfine` for wall-clock comparison across tools and `criterion` for internal regression
benchmarks. Both are required.

**Fixture generator** (`cargo xtask bench-fixtures --count N`): deterministic seeded-random
item sets with:
- 25% closed, 65% open, 10% in_progress.
- 20% of items have 1–4 deps.
- 30% of items have at least one label (drawn from 20 realistic values).
- Titles: ~35 chars mean.
- Bodies: 85% short (~60 chars), 10% medium (~200 chars), 5% long (~700 chars).

**Comparative script:** `cargo xtask bench-compare` generates a Markdown table comparing
`clove ls` (scan and index modes), `tk ls` (if installed), and `bd ls` (if installed) against
the same 1,000-item fixture. Results committed to `docs/benchmarks/<version>.md`.

---

## 14. Expert Conflict Resolutions

The following decisions had conflicting or diverging expert recommendations. Resolutions are
recorded here for transparency.

### 14.1 ID Length: 5 chars vs. 8 chars

**Expert 1** (data model): recommended 5 base32-Crockford characters.
**Expert 7** (performance): recommended 8 Crockford characters.
**Expert 9** (verification): recommended 5 base32 characters.
**Expert 6** (git/merge): recommended 5 base32 characters.
**Expert 5** (daemon): noted the 3-char example is insufficient.

**Resolution: 8 Crockford characters.** Expert 7's math is decisive: at 10,000 concurrent
items, 5-char Crockford gives ~0.15% collision probability which, while technically acceptable,
becomes uncomfortable at 100,000 items (0.5%). 8 chars (32^8 ≈ 1.1T) gives 0.005% at 10,000
and 0.5% only at 1,000,000 items. The extra 3 characters are a trivial ergonomic cost. The
Crockford alphabet (uppercase, no I/L/O/U) is used instead of base32 RFC 4648 (a-z2-7)
for visual clarity in filenames and shell completion.

### 14.2 Frontmatter Parser: serde_yaml vs. gray_matter+serde_yaml_neo vs. hand-rolled

**Expert 1** (data model): serde_yaml v0.9+.
**Expert 7** (performance): hand-rolled scanner.
**Expert 8** (Rust architecture): gray_matter + serde_yaml_neo.

**Resolution: two-phase (serde_yaml_neo for parsing, hand-rolled FrontmatterWriter for
serialization).** The performance expert is correct that a hand-rolled scanner at ~1-3 µs/item
vs serde_yaml at ~15 µs/item matters at scale. However, Expert 8's concern about the
deprecated serde_yaml 0.9 is also valid. The compromise: use the `serde_yaml_neo` crate
(crates.io: `serde_yaml_neo`, the Ware fork) for deserialization (correctness and
maintenance), with the YAML alias/bomb pre-scan guard. Cargo.toml entry:
`serde_yaml_neo = "0.9"`. Use a completely hand-rolled `FrontmatterWriter` for serialization
(canonical order, inline-flow lists, deterministic output). This gives correctness on reads
and performance + determinism on writes. A full hand-rolled scanner for the read path is
tracked as a post-M0 optimization if benchmarks show it is needed.

### 14.3 Comment Storage: Flat sidecar vs. Subdirectory

**Expert 1** (data model): `.clove/issues/<id>/comments/` (directory-per-item, item moves to
`item.md`).
**Expert 6** (git/merge): `.clove/issues/<id>.md` stays flat; `.clove/issues/<id>/` subdir
only for comments.
**Expert 9** (verification): same as Expert 6.

**Resolution: Expert 6/9 approach (flat item file + sibling comment directory).** The
directory-per-item approach (Expert 1) would force renaming all existing item files from
`<id>.md` to `<id>/item.md`, creating unnecessary churn. The flat approach keeps item file
paths stable while still making concurrent comments conflict-free.

### 14.4 `dep cycle` Exit Code

**Expert 4** (CLI): exit 0 with data.cycles; `--fail-on-cycle` flag for exit 3.
**Expert 2** (graph engine): exit 0 (analysis commands should not fail on findings).

**Resolution: exit 0 always; `--fail-on-cycle` flag for exit 3.** Unanimous expert agreement.
Documented explicitly in `clove --help` and `agent-doc` to prevent agent pipelines from
accidentally treating cycle detection as a failure.

### 14.5 YAML List Serialization for Empty Arrays

**Expert 1** (data model): omit empty arrays entirely (reduces diff noise).
**Expert 6** (git/merge): serialize as `[]`, never omit (ensures merge driver has a stable
baseline line for union-merge).

**Resolution: Expert 6.** The merge driver's three-way set merge depends on having a stable
line for each list field. If `deps` is omitted when empty, a branch that adds the first dep
produces a new line (add) rather than modifying an existing line, which changes the conflict
semantics. The diff noise from `deps: []` is minimal; the merge-correctness benefit is
concrete.
