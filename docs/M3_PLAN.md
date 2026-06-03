# clove — M3 (Daemon) Phased Implementation Plan

> **Status:** Authoritative phased plan for Milestone **M3 — Daemon**.
> Cross-references `DESIGN.md` (§8 Daemon Design, §6.3/§6.4 consistency, §8.7 git
> auto-sync), `IMPLEMENTATION_PLAN.md` (T-D01–T-D06, T-X01–T-X03), and
> `VERIFICATION_PLAN.md` (M3-G01–M3-G09, §9 CI matrix incl. `daemon-windows`).
> M3 builds on **completed M0 (file core) + M1 (SQLite index) + M2 (interop)**.
> The daemon is **optional and never required**: the CLI works identically (just
> without continuous freshening) when it is not running. M3 changes **zero**
> file-store correctness and adds **no** new always-on requirement.

---

## 1. Scope & current state

**Goal of M3:** an optional `cloved` process, one per repository, that watches
`.clove/issues/`, keeps the SQLite index incrementally fresh in the background,
answers fast IPC queries, and (opt-in) auto-commits clean item edits — while the
files remain the single source of truth and the CLI degrades gracefully to its
existing direct/index/file-scan paths whenever the daemon is absent.

**M3 tasks (from `IMPLEMENTATION_PLAN.md` §"M3 — Daemon"):**

| Task | Deliverable | DESIGN ref |
|---|---|---|
| T-D01 | M1 prerequisites audit (atomic rename, `BEGIN IMMEDIATE`, `file_mtimes`) | §6.3, §8.6 |
| T-D02 | Daemon skeleton + signal handling + lock/pid lifecycle | §8.1, §8.2, §8.9 |
| T-D03 | IPC server (frame protocol, PING/QUERY/REINDEX/STATUS) + CLI liveness | §8.3, §8.4 |
| T-D04 | File watcher (notify), debounce/batch, startup mtime sweep | §8.5, §8.6 |
| T-D05 | `clove daemon start\|stop\|status`, `[daemon]` config, idle shutdown | §7.2, §8.8 |
| T-D06 | Git auto-sync (opt-in, never push) | §8.7 |

**What already exists (consumed, not rebuilt):**

- `cloved` crate is a wired stub (`crates/cloved/src/main.rs` prints "not
  implemented"; depends on `clove-core` + `clove-index`).
- Workspace pins are present and version-justified for everything M3 needs:
  `notify = "6"` (NOT 9-rc), `interprocess = "2"`, `tokio = { features=["full"] }`,
  `git2 = "0.21"`, `anyhow`, `fd-lock = "4"`.
- **Index prerequisites are already satisfied** (verified for T-D01): the
  `file_mtimes` table exists (`db.rs`); `upsert_item` runs in a single
  `BEGIN IMMEDIATE` transaction with FTS5 in-txn sync; item writes use atomic
  rename (`clove-core::write`); `reindex` already does `PRAGMA wal_checkpoint(TRUNCATE)`.
- `clove-index` exposes the exact primitives the daemon reuses: `upsert_item`,
  `check_staleness` / `check_staleness_fast` / `apply_staleness`, `reindex`,
  `query_list` / `query_items` / `search`, `Index::open`/`open_or_create`.
- `clove-core::config` already defines `DaemonConfig { git_sync, watch_debounce_ms,
  idle_shutdown_min }` with defaults `(false, 200, 0)` and parses the `[daemon]`
  TOML section (M3 only **consumes** it; no new config keys).
- `clove init` already writes `.clove/.gitignore` with `daemon.sock`, `daemon.pid`,
  `daemon.lock`, `reindex.lock`, `index.db.tmp` — so M3 introduces no new ignore
  entries.
- The CLI envelope, exit codes (§7.6, incl. the reserved `DaemonError = 7`), JSON
  schemas, `assert_cmd` e2e harness, criterion benches, cargo-fuzz layout, and
  `cargo xtask` are all in place from M0/M1/M2 and are reused verbatim.

**Decisions locked for M3 (don't relitigate):**

- **IPC protocol lives in a new lean `clove-ipc` crate**, not in `clove-core`
  (which DESIGN §1 mandates stay "no async, no IPC") and not by making the CLI
  depend on `cloved` (that would pull `tokio` into the lean `clove` binary). The
  crate holds: the wire types (serde structs/enums), the **4-byte LE length-prefix
  framing codec**, and a **synchronous blocking client** (`interprocess`
  `LocalSocketStream`, no tokio). `cloved` (server) drives the same protocol on its
  tokio runtime. This keeps `tokio`/`notify`/`git2` entirely out of the default
  `clove` binary while giving both sides one source of truth for the wire format.
- **`git2` is gated behind a default-on `git-sync` cargo feature** on `cloved`,
  exactly mirroring M2's default-on `github` feature on `clove-import`. Reason:
  DESIGN §1 notes `git2`'s vendored libgit2 C build blocks the macOS→Windows
  cross-check. With the feature, `cargo check --no-default-features -p cloved`
  stays libgit2-free and cross-clean; `git_sync = true` with a feature-less binary
  fails fast with a clear "built without git-sync support" error.
- **The daemon is the index's continuous freshener; the CLI is still allowed to
  read the index/files directly.** When the daemon is alive the CLI routes reads
  through `QUERY` (and may skip its own staleness scan, since the daemon owns
  freshness); when it is absent the CLI uses today's exact path (auto-refresh ≤20
  stale, else file-scan). No read ever *requires* the daemon — that is the PRD's
  "nothing required but the binary and the files" guarantee and is gate-enforced
  (M3-G04 fallback).
- **Schema bumps to v3** to add `file_mtimes.synced_at INTEGER` (nullable),
  required by §8.7/T-D06 to suppress the re-commit feedback loop. The index is a
  gitignored, rebuildable cache, so the bump auto-rebuilds on open (existing M1
  v1→v2 behavior); no data migration is written. Done **once** in Phase 0 so no
  mid-stream migration is needed.
- **Detached-spawn contract:** `clove daemon start` spawns `cloved` as a detached
  child (Unix: double-fork + `setsid`; Windows: `CREATE_NO_WINDOW |
  DETACHED_PROCESS`) and waits up to 5s for `daemon.pid` to appear. `cloved` writes
  `daemon.pid` **only after** binding the socket AND finishing the startup sweep
  (so "pid present" ⇒ "ready"). This is the §8.2 invariant.
- **Shutdown is signal-driven, not socket-command-driven:** Unix `SIGTERM`
  (`clove daemon stop` sends it to the pid); Windows a named event derived from the
  repo hash (`CreateEventW`/`OpenEventW`+`SetEvent`). The shutdown sequence (flush
  debounce batches → `wal_checkpoint(TRUNCATE)` → close SQLite → remove
  sock/pid → exit 0) is identical on both platforms.

**Out of scope for M3** (deferred): M4 TUI/web/vendor bridges, richer
history/changelog. No new frontmatter fields. No mandatory daemon. No `git push`
(auto-sync only ever `add`+`commit`, per §8.7). No multi-repo/global daemon (one
per `.clove/`, §8.1).

---

## 2. Shared architecture (built in Phase 0, used by the rest)

```
crates/clove-ipc/                 NEW — lean, no tokio
  src/lib.rs        // re-exports
  src/protocol.rs   // Request/Response enums (PING, QUERY{filter,format,fields},
                    //   REINDEX, STATUS) + payload structs; serde; protocol VERSION
  src/frame.rs      // read_frame/write_frame: 4-byte LE length prefix + UTF-8 JSON,
                    //   MAX_FRAME bound; std::io::{Read,Write} generic (sync)
  src/client.rs     // DaemonClient (blocking interprocess LocalSocketStream):
                    //   connect_with_timeout(50ms), ping(), query(), status(),
                    //   reindex(); stale-socket cleanup helper

crates/cloved/src/                 (tokio runtime; default-on `git-sync` feature)
  main.rs           // arg parse (run mode), build 2-worker runtime, call lifecycle
  lifecycle.rs      // T-D02: daemon.lock flock, socket bind, pid write-after-ready,
                    //   signal wiring (unix SIGTERM / windows named event),
                    //   shutdown sequence
  ipc.rs            // T-D03: accept loop, per-conn frame read → dispatch → respond
  watcher.rs        // T-D04: notify watch, per-path 200ms debounce, batch→one txn,
                    //   startup mtime sweep, db-file exclusion
  reindexer.rs      // shared apply-changes helper over clove-index (used by watcher
                    //   + REINDEX handler + startup sweep)
  git_sync.rs       // T-D06 (feature `git-sync`): clean-edit auto-commit, skip
                    //   guards, synced_at bookkeeping
  state.rs          // DaemonState { Index handle, counters, watcher_state,
                    //   last_event_ms, started_at } shared via Arc<Mutex<…>>

crates/clove/src/cmd/
  daemon.rs         // T-D05: `clove daemon start|stop|status`; detached spawn;
                    //   stop via signal + poll; status via DaemonClient
crates/clove/src/
  context.rs (edit) // liveness probe → route reads through DaemonClient when alive,
                    //   else today's index/file path (unchanged fallback)
```

- **Reuse, do not reinvent:** the daemon never re-implements parsing, indexing, or
  querying — it calls `clove-core` (parse/store) and `clove-index`
  (`upsert_item`/`apply_staleness`/`reindex`/`query_*`). Its only new logic is
  *orchestration*: watch → debounce → batch-apply, IPC framing, lifecycle, and
  git-sync gating.
- **Single write path preserved:** all index mutations still go through
  `clove_index::upsert_item` / `apply_staleness` (the §6.3 encapsulated path); the
  daemon adds no direct SQL writes.
- **Concurrency model:** SQLite WAL + `BEGIN IMMEDIATE` already make
  CLI-writer / daemon-writer / CLI-reader safe. CLI write-through events the
  watcher, but re-`upsert` is idempotent and `synced_at`/mtime tracking suppresses
  redundant work — documented and tested, not newly engineered.

---

## 3. Phases

Phases are ordered low-risk → high-risk and dependency-respecting. **Every phase
ends at an acceptance gate**: the listed tests pass, `cargo clippy --all-targets
-D warnings` (default, `--all-features`, and `-p cloved --no-default-features`)
and `cargo fmt --check` are clean, and **all prior milestones' gates still pass**.
Each phase is its own commit (or small commit set) on
`claude/affectionate-lamport-k5b1u`. No PR is opened unless explicitly requested.

### Phase 0 — Prereq audit, schema v3, `clove-ipc` scaffold, CLI wiring (T-D01)

**Build:**
- **T-D01 audit (verify, mostly no new code):** assert atomic rename on all item
  writes, `BEGIN IMMEDIATE` on all index writes, and `file_mtimes` presence — add a
  small `#[test]` codifying each invariant so a future regression is caught.
- **Schema → v3:** add `file_mtimes.synced_at INTEGER` (nullable); bump
  `SCHEMA_VERSION` to 3; keep the existing v-mismatch auto-rebuild. (Consumed only
  in Phase 5, created now to avoid a mid-stream migration.)
- **`clove-ipc` crate:** `protocol.rs` wire types + `PROTOCOL_VERSION`, `frame.rs`
  codec with a `MAX_FRAME` guard, `client.rs` blocking `DaemonClient` skeleton
  (connect-with-timeout + stale-socket cleanup; methods return typed responses).
  Add to workspace members and `[workspace.dependencies]` (path+version, like the
  other internal crates, to stay off cargo-deny's wildcard list).
- **`cloved` deps:** add `tokio`, `notify`, `interprocess`, `anyhow`, `fd-lock`,
  `clove-ipc`, and `git2` under a default-on `git-sync` feature; `main.rs` parses a
  minimal arg surface (`cloved run --clove-dir …`) and exits cleanly (still no
  watcher/IPC).
- **`clove daemon` subcommand skeleton:** add `Daemon { start|stop|status }` to
  `cli.rs`; `cmd/daemon.rs` dispatches to typed `NotYetImplemented` handlers
  (exit-mapped to `DaemonError = 7`). `clove` gains a dep on `clove-ipc` only (no
  tokio).

**Tests:**
- T-D01 invariant tests (atomic-rename, `BEGIN IMMEDIATE`, `file_mtimes`/`synced_at`
  columns present).
- `frame.rs` round-trip unit tests (encode→decode arbitrary payloads; oversize
  length prefix rejected, not panicked; truncated frame → `Err`, not hang).
- `protocol.rs` serde round-trip for every Request/Response variant.
- `clove daemon --help` / `start|stop|status --help` parse and list subcommands;
  dispatch reaches the stub handler.

**Gate P0:** workspace builds on stable + MSRV; **`cargo check -p clove` pulls in
no `tokio`/`notify`/`git2`** (lean-CLI assertion via `cargo tree`); `cargo check
-p cloved --no-default-features` is libgit2-free; clippy/fmt clean in all configs;
schema-v3 auto-rebuild test passes; full M0/M1/M2 suites green.

### Phase 1 — Daemon skeleton, lock/pid lifecycle, clean shutdown (T-D02)

**Build:** `cloved/lifecycle.rs`.
1. Acquire `.clove/daemon.lock` via `fd-lock` advisory flock; if held →
   print `daemon already running` to stderr, exit non-zero (two-daemon guard).
2. Build `tokio::runtime::Builder::new_multi_thread().worker_threads(2)`.
3. Bind the IPC listener (Unix socket `.clove/daemon.sock`; Windows named pipe
   `\\.\pipe\clove-<repo-hash>`). On a pre-existing-but-dead socket, unlink+rebind.
4. **Write `daemon.pid` only after** the socket is bound (and, from Phase 3
   onward, after the startup sweep) — the §8.2 readiness invariant.
5. Signal wiring: Unix `tokio::signal::unix::signal(SignalKind::terminate())`;
   Windows a named shutdown event (`CreateEventW` from the repo hash) selected in
   the event loop. Both converge on one `shutdown()` future.
6. **Shutdown sequence:** flush pending debounce batches (no-op until Phase 3) →
   `PRAGMA wal_checkpoint(TRUNCATE)` → close SQLite → remove `daemon.sock` +
   `daemon.pid` → release `daemon.lock` → exit 0.

**Tests (`crates/cloved/tests/daemon_lifecycle.rs`):**
- **SIGTERM (Unix):** spawn `cloved run`, wait for pid file, send `SIGTERM`, assert
  exit 0 **and** sock+pid removed (gate M3-G03).
- **Two daemons:** start one, second invocation prints "daemon already running"
  and exits non-zero (gate M3-G08).
- **Windows shutdown (`#[cfg(windows)]`):** signal the named event, assert clean
  exit + cleanup (runs in the `daemon-windows` CI job; M3-G03 on `windows-latest`).
- pid-after-bind ordering test: pid file never observed before the socket accepts.

**Gate P1 (M3-G03, M3-G08):** clean SIGTERM/named-event shutdown leaves no stale
sock/pid; second daemon refused.

### Phase 2 — IPC server + CLI liveness/routing (T-D03)

**Build:**
- `cloved/ipc.rs`: accept loop; per connection `read_frame` → deserialize Request →
  dispatch → `write_frame(Response)`. v1 handlers: **PING→PONG**;
  **STATUS** → `{uptime_s, items_indexed, watcher_state, last_event_ms}` from
  `DaemonState`; **QUERY{filter,format,fields}** → run `clove_index::query_list`/
  `query_items`/`search` and return the standard `{ok,data,_meta}` envelope
  (`_meta.source = "daemon"`); **REINDEX** → call `clove_index::reindex`, return
  `{items_indexed, duration_ms, warnings}`. Malformed/oversize frame → typed error
  response, connection dropped, daemon stays up.
- `clove` client routing (`context.rs` + `client.rs`): on a read command, probe
  liveness — `daemon.sock` present? connect (50ms timeout)? `PING`→`PONG`? On any
  failure (`ECONNREFUSED`/timeout/no-pong) **delete stale `daemon.sock`+`daemon.pid`
  and fall back** to today's index/file path. On success, send `QUERY` and emit its
  envelope (CLI may skip its own staleness scan — the daemon owns freshness).

**Tests:**
- IPC PING/PONG round-trip unit/integration (gate M3-G01 timing asserted in the
  bench/gate test, < 5ms).
- **Stale-socket recovery (M3-G04):** start daemon, `SIGKILL` it (leaves
  sock+pid), run `clove ls`; assert it completes < 200ms (connect-timeout +
  cleanup + fallback) **and** removes the stale files.
- QUERY parity: `clove ls`/`ready`/`query`/`search` via daemon return the same
  ids/ordering as the direct index path (reuse the M1 file↔index parity harness,
  extended with a daemon source).
- REINDEX over IPC returns a correct report; STATUS fields are well-formed.
- Daemon survives a malformed frame (fuzz seed) without dropping the listener.

**Bench (`benches/bench_ipc.rs`):** PING and QUERY round-trip latency (gate
M3-G01: PING/PONG < 5ms; QUERY recorded).

**Gate P2 (M3-G01, M3-G04):** IPC round-trip < 5ms; stale-socket → next `clove ls`
< 200ms with cleanup; daemon-routed reads match direct-read results.

### Phase 3 — File watcher, debounce/batch, startup sweep (T-D04)  ← highest-value

**Build:** `cloved/watcher.rs` + `cloved/reindexer.rs`.
- **Startup mtime sweep (runs before pid write):** query `file_mtimes`, scan
  `.clove/issues/`, diff by mtime/hash (reuse `clove_index::check_staleness` +
  `apply_staleness`), re-index changed/added/deleted files in one transaction. Only
  after this does lifecycle write `daemon.pid` (covers files changed while stopped,
  e.g. `git pull`).
- **Watch:** `notify` 6.x recursive on `.clove/issues/`, **filter to `*.md`**,
  **explicitly exclude** `index.db`, `index.db-wal`, `index.db-shm`, `index.db.tmp`
  (feedback-loop prevention).
- **Debounce:** per-path 200ms (`config.daemon.watch_debounce_ms`), timer reset on
  each new event for the same path.
- **Batch:** all paths whose debounce fires within a window → a **single**
  `BEGIN IMMEDIATE` transaction via the shared `reindexer`. Update `DaemonState`
  counters + `last_event_ms`.
- Malformed-file guard: a path whose frontmatter fails to parse is skipped (logged),
  never crashes the watcher (reuses M0 parse hardening).

**Tests (`crates/cloved/tests/daemon_watch.rs`):**
- **Feedback-loop regression (M3-G05):** run `clove reindex` while the daemon runs;
  assert **zero** `index.db*` events are processed (counter unchanged).
- **Debounce batching (M3-G06):** write a file in 10 chunks 10ms apart → exactly
  **1** SQLite update observed.
- New-file / edit / delete each reflected in the index within one debounce window.
- **Startup sweep correctness:** stop daemon, mutate 50 of 1k files out-of-band,
  restart → index matches files when pid appears.
- Malformed mid-edit file does not crash the watcher; a later valid write indexes.

**Bench (`benches/bench_daemon_startup.rs`):** 1k items, 50 modified → time to
"ready" (pid written). Gate M3-G02: < 500ms.

**Gate P3 (M3-G02, M3-G05, M3-G06):** no feedback loop; 10×10ms → 1 update;
startup sweep (1k/50) → ready < 500ms.

### Phase 4 — `clove daemon` subcommands, config, idle shutdown (T-D05)

**Build:** `clove/cmd/daemon.rs` + `cloved` idle logic.
- **`clove daemon start`:** spawn `cloved run` detached (Unix: double-fork +
  `setsid`, redirect std streams to `/dev/null`; Windows: `CreateProcess` with
  `DETACHED_PROCESS | CREATE_NO_WINDOW`); poll for `daemon.pid` up to 5s; print
  `{ok, pid}` envelope. If already running (lock held / live socket), report it and
  exit 0 (idempotent start).
- **`clove daemon stop`:** read `daemon.pid`; Unix → `SIGTERM`; Windows → signal the
  named event; poll for pid-file removal up to 5s; on timeout, escalate guidance
  (no `SIGKILL` by default). Stopping when none runs → clean no-op message.
- **`clove daemon status`:** liveness probe → `STATUS` over IPC → pretty-print
  (human) or `{ok,data}` (json); when absent, report "not running".
- **Idle shutdown:** when `config.daemon.idle_shutdown_min > 0`, a tokio interval
  task self-terminates the daemon after N minutes with no watcher/IPC activity
  (useful for CI), running the standard shutdown sequence.

**Tests (`crates/clove/tests/daemon_cli.rs` + `cloved` unit):**
- Functional `start` → `status` (reports running, items_indexed) → `stop`
  (pid/sock gone) round-trip via `assert_cmd`.
- `start` idempotency (second start reports already-running, exit 0).
- `stop` with no daemon → friendly no-op.
- **Idle shutdown:** set `idle_shutdown_min` tiny; with tokio time paused/advanced,
  assert the daemon self-terminates and cleans up.
- `status --format json` validates against the standard envelope schema.

**Gate P4 (T-D05 AC):** `start|stop|status` functional and idempotent; idle
shutdown fires; JSON envelopes valid.

### Phase 5 — Git auto-sync (T-D06, feature `git-sync`)

**Build:** `cloved/git_sync.rs` (compiled only under the default-on `git-sync`
feature). After a successful index update for `<path>`, when
`config.daemon.git_sync == true`:
1. **Skip guards:** `.git/MERGE_HEAD` present (active merge), `.git/rebase-merge/`
   or `.git/rebase-apply/` present (active rebase), or frontmatter fails to parse
   (mid-edit autosave guard) → skip, no commit.
2. Only if the file is modified-but-uncommitted (`git status`/`git2` diff vs
   `HEAD`): `git add <path>` + `git commit -m "clove: auto-sync <id> [<change>]"`
   via `git2` (no subprocess).
3. **Record `file_mtimes.synced_at`** = the post-commit mtime so the inotify event
   produced by git's own index update does **not** re-trigger a commit.
4. **Never push** — push stays the user's responsibility (§8.7).
- Feature-off build: `git_sync = true` yields a clear "this binary was built
  without git-sync support" error at startup; `cargo check -p cloved
  --no-default-features` is libgit2-free.

**Tests (`crates/cloved/tests/daemon_git_sync.rs`, `#[cfg(feature = "git-sync")]`):**
- **Skip-during-rebase:** create `.git/rebase-merge/`, edit an item → no commit
  (gate M3-G07).
- **Skip-during-merge:** `.git/MERGE_HEAD` present → no commit.
- **Malformed-skip:** write invalid frontmatter → no commit; fix it → one commit.
- **synced_at suppresses re-commit:** one clean edit → exactly one auto-commit, and
  the resulting git-index event does **not** produce a second commit.
- **Never-push:** assert no network/remote interaction (no `push` refspec touched).
- Sandbox note: these make real `git commit`s; like M2's merge-driver tests they
  must tolerate the sandbox's commit-signing artifact (or set
  `commit.gpgsign=false` / `-c` in the test's git env) so they pass deterministically.

**Gate P5 (M3-G07):** auto-sync respects all skip guards; `synced_at` prevents the
re-commit loop; never pushes; feature-off build is clean and fails fast on
`git_sync=true`.

### Phase 6 — Fuzz, benches, docs, full-gate re-run

**Build/docs:**
- **Fuzz target `fuzz/fuzz_targets/ipc_frame.rs`:** arbitrary bytes → the frame
  decoder + `protocol` deserializer never panic, reject oversize length prefixes,
  and terminate (with a committed seed corpus: valid PING/QUERY/STATUS frames,
  truncated frame, huge length prefix, non-UTF8, junk JSON). Wire into the 30s/target
  CI fuzz job and `cargo xtask test-all`.
- **Benches wired:** `bench_ipc` (round-trip) and `bench_daemon_startup` (sweep)
  compile in CI (`cargo bench --no-run`); their gate assertions run as `gate_*`
  `#[test]`s using `std::time::Instant` (the M0/M1 pattern), enforcing M3-G01/G02.
- **CI:** add the `daemon-windows` job (`cargo test -p cloved -- daemon_`) per
  VERIFICATION_PLAN §9 so the Windows named-event shutdown is gate-enforced.
- Extend `clove agent-doc` with one line on the optional daemon (`clove daemon
  start` to keep the index hot; never required) — keep its idempotency test green.
- Write **`docs/M3_ACCEPTANCE_GATES.md`** (mirroring `M2_ACCEPTANCE_GATES.md`) with
  the measured numbers, the `git-sync` feature/cross-build caveat, and the Windows
  named-event note.
- Update `HANDOFF.md` (state → "M3 complete and gated"), `IMPLEMENTATION_PLAN.md`
  M3 status (✅ per task), and `VERIFICATION_PLAN.md` M3 gate statuses.

**Gate P6 (M3-G09 + final):** **all** M0+M1+M2 acceptance gates re-run green;
clippy/fmt clean in every feature config; the full M3 gate table below is
satisfied; the only tolerated failure remains the pre-existing environment-only
`repo::tests::linked_worktree_resolves_to_main_worktree` (sandbox git-signing
artifact) — plus, where unavoidable, the same signing caveat on the new
`git-sync` commit tests.

---

## 4. M3 acceptance-gate summary (maps to VERIFICATION_PLAN.md M3-G01–G09)

| Gate | Source | Phase | Pass condition |
|---|---|---|---|
| M3-G01 | T-D03 AC | P2 | Daemon IPC PING/PONG round-trip < 5ms |
| M3-G02 | T-D04 AC | P3 | Startup sweep: 1k items, 50 modified → ready < 500ms |
| M3-G03 | T-D02 AC | P1 | SIGTERM (Unix) / named event (Windows) → no stale sock/pid, exit 0 |
| M3-G04 | T-D03 AC | P2 | Kill with SIGKILL → next `clove ls` < 200ms (cleanup + fallback) |
| M3-G05 | T-D04 AC | P3 | No `index.db*` events processed after `clove reindex` |
| M3-G06 | T-D04 AC | P3 | 10 chunks × 10ms → exactly 1 SQLite update (debounce) |
| M3-G07 | T-D06 AC | P5 | git auto-sync skips during rebase/merge/malformed; synced_at no re-commit |
| M3-G08 | T-D02 AC | P1 | Second daemon prints "already running", exits non-zero |
| M3-G09 | full suite | P6 | All M0+M1+M2 gates still pass |
| (fallback) | PRD §5.2 | P2 | All reads succeed with daemon absent (no required process) |
| (lean CLI) | DESIGN §1 | P0 | `clove` binary pulls in no tokio/notify/git2 |
| (fuzz) | T-X02 | P6 | `ipc_frame` target 30s clean on committed corpus |

**Testing layers used:** unit (frame codec, protocol serde, debounce timer, git-sync
skip-guards), `assert_cmd` CLI e2e (`daemon start|stop|status`, daemon-routed reads,
JSON-schema-validated), real-process integration (spawned `cloved` + signals +
sockets; `#[cfg(windows)]` named-event path), parity (daemon-source vs direct-index
reads, reusing the M1 harness), criterion benches + `gate_*` timing tests
(M3-G01/G02), real-`git` integration (auto-sync guards), cargo-fuzz (new `ipc_frame`
target, 30s CI replay + seed corpus). Idle-shutdown uses tokio paused-time.

---

## 5. Risks & mitigations

- **Pulling `tokio`/`git2` into the lean CLI:** avoided by the `clove-ipc` crate
  (sync client, no tokio) and the default-on `git-sync` feature on `cloved`;
  enforced by a P0 `cargo tree` assertion and `--no-default-features` checks.
- **`git2` vendored libgit2 cross-build (macOS→Windows):** isolated behind
  `git-sync`; `cargo check -p cloved --no-default-features` stays libgit2-free, as
  DESIGN §1 requires.
- **`notify` platform variance (FSEvents/inotify/ReadDirectoryChangesW):** pin to
  6.x (NOT 9-rc, per the Cargo.toml comment); test behavior, not the backend; the
  **startup mtime sweep is the correctness backstop** for any missed/coalesced
  event, so freshness never depends solely on the watcher.
- **Detached spawn & signal testing in CI:** Unix double-fork + `setsid` with
  std-stream redirection; Windows named-event path gated by the dedicated
  `daemon-windows` job; tests poll the pid-file contract rather than racing on
  timing.
- **Feedback loops (CLI write-through, git-sync's own git-index event):** db-file
  exclusion + idempotent `upsert` + `synced_at` bookkeeping; regression-tested by
  M3-G05 and the synced_at no-re-commit test.
- **Daemon writer vs CLI writer/reader concurrency:** relies on the existing
  WAL + `BEGIN IMMEDIATE` guarantees (M1); no new locking invented; a P2 parity
  test exercises concurrent daemon-refresh + CLI read.
- **Sandbox git-signing artifact** breaking the new auto-sync commit tests:
  mirror M2's tolerance (or disable signing in the test git env) so the gate is
  deterministic in CI while remaining a real `git commit` test.
- **Scope creep:** no schema fields beyond `synced_at` (v3), no push, no
  multi-repo daemon, no new CLI surface beyond `daemon start|stop|status`.

---

## 6. Execution order (commits on `claude/affectionate-lamport-k5b1u`)

P0 prereq+ipc-scaffold+schema-v3 → P1 lifecycle/shutdown → P2 IPC+liveness →
P3 watcher/debounce/sweep → P4 `clove daemon` CLI+idle → P5 git-sync →
P6 fuzz/bench/docs/gates. Each phase is committed independently with its tests;
push at phase boundaries. No PR is opened unless explicitly requested.
