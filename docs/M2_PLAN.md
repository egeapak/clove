# clove — M2 (Interop) Phased Implementation Plan

> **Status:** Authoritative phased plan for Milestone **M2 — Interop**.
> Cross-references `DESIGN.md` (§9, §11), `IMPLEMENTATION_PLAN.md` (T-M01–T-M05,
> T-X01–T-X03), and `VERIFICATION_PLAN.md` (V-I14–V-I16, M2-G01–M2-G06).
> M2 builds on **completed M0 (file core) + M1 (SQLite index)**; it adds no new
> always-on requirements and never changes file-store correctness.

---

## 1. Scope & current state

**Goal of M2:** make `clove` a no-lock-in tracker — migrate work *in* from tk,
Beads, and GitHub, *out* to JSON/JSONL/GitHub, and merge item files cleanly on
parallel branches via a real 3-way merge driver.

**M2 tasks (from `IMPLEMENTATION_PLAN.md`):**

| Task | Deliverable | DESIGN ref |
|---|---|---|
| T-M01 | `clove import tk <.tickets dir>` | §11.1 |
| T-M02 | `clove import beads <issues.jsonl>` | §11.2 |
| T-M03 | `clove import github`, `clove export github` | §11.3 |
| T-M04 | `clove export json`, `clove export jsonl` | §11.4 |
| T-M05 | `clove merge-driver <O> <A> <B> <L>` | §9.2 |

**What already exists (no work needed, just consumed):**

- `clove-import` crate is a wired but empty stub (`clove-core` + serde + serde_json
  + camino + anyhow deps present); `octocrab 0.51`, `tokio`, `git2 0.21` pinned in
  the workspace.
- `ItemFrontmatter` already carries `source_system: Option<String>` and
  `external_ref: Option<String>`, and `FrontmatterWriter` already emits them in
  canonical order (after the relation lists, before the closing `---`).
- `clove init --merge-driver` already writes the `.gitattributes` entry and the
  `[merge "clove-item"]` `.git/config` stanza pointing at `clove merge-driver` —
  but the **subcommand itself does not exist yet** (the driver is a dangling
  reference until T-M05 lands).
- The CLI envelope, exit codes (§7.6), `--format json|jsonl`, JSON schemas,
  `item_json` shaping, `assert_cmd` e2e harness, criterion benches, cargo-fuzz
  targets, and `cargo xtask` are all in place from M0/M1 and are reused verbatim.

**Decisions locked for M2 (don't relitigate):**

- **Idempotency key = `external_ref`.** Re-import skips any incoming item whose
  `external_ref` matches an existing item's `external_ref`. Built once as a shared
  pre-scan, reused by all three importers.
- **Unmapped Beads fields** are stashed as a `beads-meta:<json>` prefix inside
  `external_ref` (DESIGN §11.2 first option) — keeps it in a single first-class
  field, round-trippable, no schema change. (The "structured comment in body"
  alternative is rejected: it pollutes the body and the merge-file path.)
- **`export jsonl` is isomorphic with Beads' `issues.jsonl`** (DESIGN §11.4), so
  "re-import of own export" is exercised through `clove import beads`. No separate
  `clove import json` command is added (not in the §7.2 CLI surface).
- **GitHub network tests are gated behind `GITHUB_TOKEN`** and skipped when unset
  (CI/sandbox have no token); all GitHub field-mapping + clove-meta codec logic is
  covered by **offline** unit tests that never touch the network.
- **Merge driver writes the merged result to the `%A` (ours) path** and uses the
  git contract: **exit 0 = clean**, **exit 1 (nonzero) = conflict** (git then keeps
  the file with whatever conflict content the driver wrote). Body conflicts are
  delegated to `git merge-file` so standard conflict markers appear.

**Out of scope for M2** (deferred, already noted in HANDOFF): M3 daemon, M4
TUI/web/vendor bridges. The `metadata`-as-body-comment alternative. Any change to
the item schema version (M2 introduces no new frontmatter fields).

---

## 2. Shared architecture (built in Phase 0, used by all importers)

```
crates/clove-import/src/
  lib.rs        // re-exports; ImportOutcome, ExportFormat, shared error
  plan.rs       // ImportPlan { would_create, would_skip, conflicts }, ImportReport
  map.rs        // status/type/priority coercion + normalize_label passthrough,
                // external_ref idempotency index builder over an ItemStore scan
  tk.rs         // T-M01
  beads.rs      // T-M02
  github.rs     // T-M03 (feature-gated tokio/octocrab)
  export.rs     // T-M04 json/jsonl writers (github export lives in github.rs)

crates/clove/src/cmd/
  import.rs       // `clove import <tk|beads|github> <src> [--dry-run]`
  export.rs       // `clove export <json|jsonl|github> [--out FILE] [--dry-run(gh)]`
  merge_driver.rs // T-M05
```

- **`Importer` trait:** `fn plan(&self, src, ctx) -> Result<ImportPlan>` (pure, no
  writes — drives `--dry-run`) and `fn apply(plan, store) -> Result<ImportReport>`
  (writes via `ItemStore::create`/atomic write path). `--dry-run` runs `plan` only
  and emits the §11.3 `{would_create, would_skip, conflicts}` envelope; the import
  path runs `plan` then `apply`.
- **One coercion point** (`map.rs`) for `task→chore`, status mapping, priority
  clamping/validation, and label normalization (reusing
  `clove_core::normalize_label`) so all importers agree and the file store only
  ever receives canonical, valid items.
- **Reuse, do not reinvent:** items are created through the existing
  `ItemStore`/`FrontmatterWriter` atomic write path; output uses the existing CLI
  envelope + `item_json` shaping; tests use the existing `assert_cmd` harness and
  fixture generator.

---

## 3. Phases

Phases are ordered low-risk → high-risk and dependency-respecting. **Every phase
ends at an acceptance gate**: the listed tests pass, `cargo clippy --all-targets
-D warnings` and `cargo fmt --check` are clean, and prior milestones' gates still
pass. Each phase is its own commit (or small commit set) on
`claude/charming-hamilton-cqkHx`.

### Phase 0 — Scaffolding & CLI wiring (no behavior yet)

**Build:** `clove-import` shared layer (`plan.rs`, `map.rs`, `Importer` trait,
idempotency index); add `Import`, `Export`, `MergeDriver` variants to `cli.rs`
`Commands` with their arg structs; dispatch in `main.rs`/`cli.rs`; each handler
returns a typed `NotYetImplemented` until its phase. Add `octocrab`/`tokio` to
`clove-import` under a `github` feature so non-GitHub builds stay lean.

**Tests:** `clove import --help`, `clove export --help`, `clove merge-driver
--help` parse and list subcommands; dispatch reaches the right (stub) handler;
full M0/M1 suite still green.

**Gate P0:** workspace builds on stable + MSRV; clippy/fmt clean; CLI surface
matches DESIGN §7.2 (`import tk|beads|github`, `export json|jsonl|github`,
`merge-driver`).

### Phase 1 — `clove export json|jsonl` (T-M04)

**Build:** `export.rs` writers. `export json` = one standard envelope with a
`data` array of full `ItemJson` (all items, sorted by `(priority, topo rank, id)`
to match list ordering, deterministic). `export jsonl` = one item object per line
(NDJSON), Beads-isomorphic field set. `--out FILE` writes atomically (tempfile +
rename); default stdout. Both honor `--no-index` (file-scan source of truth).

**Tests (`tests/export.rs`, assert_cmd):**
- JSONL: every line parses as standalone JSON; line count == item count.
- JSON: envelope validates against the list JSON schema; `data` length == items.
- Determinism: two exports byte-identical; stable ordering.
- `--out` writes the file and writes nothing to stdout.
- Empty repo: `export jsonl` emits zero lines, exit 0; `export json` emits empty
  `data` array.

**Bench (`benches/bench_export.rs`):** export 10k-item fixture → JSONL throughput
(target informational, < 200 ms warm; no hard gate, recorded in M2 gates doc).

**Gate P1:** all export tests pass; benches compile; (full "re-import own JSONL is
idempotent" round-trip is completed in Phase 4 once the Beads importer exists, and
is listed there).

### Phase 2 — `clove merge-driver` (T-M05)  ← highest-value semantic merge

**Build:** `merge_driver.rs`. `clove merge-driver <ancestor> <ours> <theirs>
<marker-size>`:
1. Parse all three files into `ItemFrontmatter` + body (tolerant of a missing
   ancestor for add/add). On any unparseable side → exit nonzero **without**
   clobbering (let git fall back to default conflict markers).
2. **Scalars** (title, status/closed, type, priority, assignee, parent,
   source_system, external_ref, created/updated): if ours==theirs → take it; if
   one side==ancestor → take the other (clean 3-way); if both changed differently
   → conflict.
3. **Lists** (labels, deps, relates, duplicates, supersedes): three-way set merge
   `union(ours,theirs) \ (ancestor \ ours \ theirs)`, then sort + dedupe. A
   *removal/add* conflict on the **same element** (A removes X, B keeps/adds X) →
   flag conflict on that field only.
4. **Body:** delegate to `git merge-file -p -L ... --marker-size=<L>`; non-zero
   from it → body conflict.
5. Write merged frontmatter (via `FrontmatterWriter`, canonical order) + merged
   body to `%A`. Exit 0 if fully clean, else exit 1 (conflict markers embedded in
   the conflicting field/body region per git convention).

**Tests (`tests/merge_driver.rs`, real git via `git2`/`git` CLI installing the
driver from `clove init --merge-driver`):**
- **V-I14** same-value status (both → closed): `git merge` exit 0, final
  `status==closed` with valid `closed` timestamp.
- **V-I15** dep union (A adds proj-AAA, B adds proj-BBB): merged deps
  `[proj-AAA, proj-BBB]` sorted, no conflict markers.
- **V-I16** dep removal conflict (A removes proj-OLD, B adds proj-NEW to
  `[proj-OLD]`): conflict reported, isolated to `deps`.
- Divergent scalar (A→in_progress, B→closed): conflict reported.
- Clean disjoint-field merge (A edits title, B edits priority): exit 0, both kept.
- Body change on one side only: merged cleanly; both-sides body edit → markers.
- Unparseable side → nonzero, original file not silently lost.

**Property test (proptest):** for random `(ancestor, ours, theirs)` label/dep sets,
the set-merge result equals the mathematical three-way union and is sorted/deduped
(commutative in ours/theirs for the non-conflict case).

**Fuzz (`fuzz/fuzz_targets/merge_driver.rs`):** arbitrary 3 byte-blobs as
O/A/B → driver never panics, never escapes the ours path, terminates.

**Gate P2 (M2-G05/G05a/G05b):** V-I14/15/16 pass against real `git merge`; property
test passes; fuzz target runs 30 s clean; same-value conflicts auto-resolve.

### Phase 3 — `clove import tk` (T-M01)

**Build:** `tk.rs`. Read each `*.md` in the given `.tickets/` dir; split
frontmatter/body; map fields per §11.1 (`task→chore`, `tags→labels`,
`links→relates`, `external-ref→external_ref`); extract first `# H1` from body as
`title` (strip it from body), filename stem as fallback **with a stderr warning**;
`source_system="tk"`; `external_ref` defaults to the tk id when absent (idempotency
key). Generate fresh `CloveId`s (tk ids are preserved only in `external_ref`).
`--dry-run` plans only.

**Tests (`tests/import_tk.rs` + fixtures `tests/fixtures/tk/`):**
- 5 representative tickets (with deps, tags, links, H1 title, one without H1) →
  correct mapping, labels normalized, type coercion.
- `--dry-run` writes zero files; reports `would_create`.
- Idempotent re-run: second import skips all (matched on `external_ref`),
  `would_skip` populated, zero new files.
- Missing-H1 fixture emits the filename-fallback warning to stderr.

**Fuzz (`fuzz/fuzz_targets/import_tk.rs`):** arbitrary markdown+frontmatter bytes →
no panic (reuses the parse hardening from M0).

**Bench:** import 1k tk tickets (informational).

**Gate P3 (M2-G01, partial M2-G04):** tk fixture tests pass; `--dry-run` writes
nothing; idempotent re-run skips all.

### Phase 4 — `clove import beads` (T-M02)

**Build:** `beads.rs`. Define thin `BeadsIssue` deserialization struct. Read
`issues.jsonl` line-by-line. Map per §11.2: `description→body`,
`issue_type task→chore`, `status deferred→open` **+ label `deferred`**,
`dependencies[type=blocks]→deps`, `[parent-child]→parent` (first only),
`[related|tracks|…]→relates`, `owner/assignee→assignee`. Unmapped fields →
`external_ref = "beads-meta:<json>"`. **`comment_count>0` → stderr warning listing
the IDs** (must not silently succeed). `source_system="beads"`. `--dry-run`.

**Tests (`tests/import_beads.rs` + fixtures `tests/fixtures/beads/issues.jsonl`):**
- Sample JSONL covering each dependency type, deferred status, task type, owner.
- `comment_count>0` line → warning with that ID.
- All items get `source_system="beads"`; unmapped fields preserved in
  `external_ref` `beads-meta:` blob.
- `--dry-run` writes nothing; idempotent re-run skips all.
- **Round-trip (closes T-M04):** `clove export jsonl > out.jsonl` then
  `clove import beads out.jsonl` into a fresh repo is idempotent / lossless on the
  mapped fields.

**Fuzz (`fuzz/fuzz_targets/import_beads.rs`):** arbitrary JSONL bytes → no panic,
malformed lines reported not crashed.

**Bench:** import 10k-line JSONL (informational).

**Gate P4 (M2-G02, completes M2-G04):** beads fixture tests pass; warnings emitted;
`source_system`/metadata correct; dry-run writes nothing; JSONL round-trip
idempotent.

### Phase 5 — `clove import github` + `clove export github` (T-M03)

**Build (feature `github`):** `github.rs` using `octocrab` on a `tokio` runtime
created inside the command (CLI stays sync elsewhere). **Export:** map
`title`/body/`assignee`/`labels`; encode `deps`/`priority`/`id` as a
`<!-- clove-meta: {json} -->` HTML comment appended to the issue body. **Import:**
fetch all issues; `number→id` as `gh-<number>`, `state→status`,
`labels[].name→labels`, `assignees[0].login→assignee`, `closed_at→closed`; parse
`clove-meta` for `deps`/`priority`; `source_system="github"`,
`external_ref="gh-<number>"`. Idempotent (skip matched `external_ref`).
`--dry-run`.

**Tests (`tests/import_github.rs`):**
- **Offline unit tests (always run):** clove-meta encode→decode round-trip;
  `GitHubIssue → Item` mapping over hand-built fixtures (no network); idempotency
  filter logic.
- **Network integration test (gated):** `#[ignore]`-by-default / skipped unless
  `GITHUB_TOKEN` set — roundtrip export then import against a scratch repo,
  `--dry-run` writes nothing, re-import idempotent.

**Gate P5 (M2-G03):** offline mapping/codec tests pass everywhere; network
roundtrip passes when `GITHUB_TOKEN` is present (documented as token-gated; CI
green via skip).

### Phase 6 — Consolidation, docs, full-gate re-run

**Build/docs:**
- Extend `clove agent-doc` to mention `import`/`export`/`merge-driver` and the
  "after merge the index auto-refreshes" note (DESIGN §9.4) — satisfies the PRD's
  "agent onboarding doc generation" line for M2. Keep the idempotency test green.
- Commit all fuzz seed corpora; wire the new benches and fuzz targets into
  `cargo xtask test-all` / the CI 30 s-per-target fuzz job.
- Write `docs/M2_ACCEPTANCE_GATES.md` (mirroring `M1_ACCEPTANCE_GATES.md`) with the
  measured numbers and the token-gated GitHub caveat.
- Update `HANDOFF.md` (state → "M2 complete and gated") and
  `IMPLEMENTATION_PLAN.md` M2 status.

**Gate P6 (M2-G06 + final):** **all** M0 and M1 acceptance gates re-run green;
clippy/fmt clean; the full M2 gate table below is satisfied; the only tolerated
failure remains the pre-existing environment-only `repo::tests::
linked_worktree_resolves_to_main_worktree` (sandbox git-signing artifact).

---

## 4. M2 acceptance-gate summary (maps to VERIFICATION_PLAN.md)

| Gate | Source | Phase | Pass condition |
|---|---|---|---|
| M2-G01 | T-M01 AC | P3 | tk import fixture tests pass |
| M2-G02 | T-M02 AC | P4 | Beads import fixture tests pass |
| M2-G03 | T-M03 AC | P5 | GitHub mapping/codec offline; roundtrip with `GITHUB_TOKEN` |
| M2-G04 | all importers | P3+P4(+P5) | every `--dry-run` writes **zero** files |
| M2-G05 | V-I14 | P2 | merge driver auto-resolves same-value conflict |
| M2-G05a | V-I15 | P2 | dep union merge, no markers |
| M2-G05b | V-I16 | P2 | dep removal/add → conflict, isolated to `deps` |
| M2-G06 | full suite | P6 | all M0+M1 gates still pass |
| (export) | T-M04 | P1/P4 | JSONL lines valid; own-JSONL re-import idempotent |
| (fuzz) | T-X02 | P2–P4 | merge-driver + import-tk + import-beads targets 30 s clean |

**Testing layers used:** unit (mapping/codec/set-merge), `assert_cmd` CLI e2e
(every command, JSON-schema-validated), real-`git` integration (merge driver),
proptest (set-merge invariants), criterion benches (export/import throughput,
informational), cargo-fuzz (3 new parser/merge targets, 30 s CI replay + seed
corpora).

---

## 5. Risks & mitigations

- **GitHub network in CI/sandbox:** no token → integration test skips; all logic is
  offline-unit-tested so coverage doesn't depend on the network.
- **Merge-driver git contract subtleties** (exit codes, marker size, where the
  result is written): pinned by real `git merge` integration tests, not mocks.
- **`octocrab`/`tokio` weight:** isolated behind the `github` cargo feature so the
  default `clove` binary and MSRV/cross builds stay lean; verify `cargo check
  --target x86_64-pc-windows-msvc` still passes.
- **Idempotency correctness:** single shared `external_ref` index + `--dry-run`
  `would_skip` reporting, asserted by re-run-skips-all tests in every importer.
- **Scope creep:** no schema bump, no new frontmatter fields, no `import json`
  command beyond the §7.2 surface.

---

## 6. Execution order (commits on `claude/charming-hamilton-cqkHx`)

P0 scaffolding → P1 export → P2 merge-driver → P3 tk → P4 beads → P5 github →
P6 docs/gates. Each phase committed independently with its tests; push at phase
boundaries. No PR is opened unless explicitly requested.
