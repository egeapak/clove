# M3 Acceptance Gates — status

Milestone **M3 — Daemon** is complete and gated. M3 adds an **optional** `cloved`
process and the `clove daemon` controls; it adds **no always-on requirement** —
every read command works identically (just without continuous freshening) when
the daemon is absent, and changes **zero** file-store correctness.

- **T-D01** index prerequisites audit + schema **v3** (`file_mtimes.synced_at`)
- **T-D02** daemon lifecycle: `daemon.lock` guard, pid-after-bind, signal-driven
  clean shutdown (Unix SIGTERM / Windows named event)
- **T-D03** IPC server (`PING`/`QUERY`/`REINDEX`/`STATUS`) + CLI liveness routing
- **T-D04** `notify` file watcher, 200 ms debounce/batch, startup mtime sweep
- **T-D05** `clove daemon start|stop|status`, `[daemon]` config, idle shutdown
- **T-D06** opt-in git auto-sync (behind the default-on `git-sync` feature)
- **T-D07** `clove doctor` daemon-health check (new; M3_PLAN §1.1 CLI review)

All gates below were verified on this branch: `cargo build --workspace`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings` (plus the
default-feature and `cloved --no-default-features` clippy configs),
`cargo fmt --all --check`, and `cargo test --workspace` are clean **except** the
single pre-existing, environment-only failure
`clove-core repo::tests::linked_worktree_resolves_to_main_worktree` (the sandbox
routes `git commit` through a signing server that returns 400 — not a code defect;
tolerated by all milestones). M3's own git-sync tests use `git2` directly, so they
are **not** affected by that hook and pass in the sandbox.

## M3 gate table (VERIFICATION_PLAN.md M3-G01–M3-G10)

| Gate | Asserts | Enforced by | Status |
|---|---|---|---|
| M3-G01 | Daemon IPC `PING`/`PONG` round-trip < 5 ms | `cloved/tests/daemon_ipc.rs::ping_round_trip_is_fast` | ✅ |
| M3-G02 | Startup sweep, 1k items / 50 modified → ready < 500 ms | `cloved/tests/daemon_watch.rs::startup_sweep_1k_50_modified_under_500ms` | ✅ |
| M3-G03 | SIGTERM (Unix) / named event (Windows) → clean exit, no stale sock/pid | `cloved/tests/daemon_lifecycle.rs::sigterm_shuts_down_cleanly_with_no_stale_files` (+ `daemon-windows` CI compiles the event path) | ✅ |
| M3-G04 | Kill -9 → next `clove ls` < 200 ms (stale-socket cleanup + fallback) | `daemon_ipc.rs::stale_socket_recovery_is_fast`, `clove/tests/daemon_routing.rs` (fallback) | ✅ |
| M3-G05 | `clove reindex` → zero watcher batches (feedback-loop prevention) | `daemon_watch.rs::reindex_does_not_trigger_watcher_batches` | ✅ |
| M3-G06 | 10 edits × 10 ms → exactly 1 applied batch (debounce) | `daemon_watch.rs::rapid_edits_debounce_into_one_batch` | ✅ |
| M3-G07 | git auto-sync skips merge/rebase/malformed; `synced_at` no re-commit | `cloved/tests/daemon_git_sync.rs` (5 tests) | ✅ |
| M3-G08 | Second daemon prints "already running", exits non-zero | `daemon_lifecycle.rs::second_daemon_refuses_to_start` | ✅ |
| M3-G09 | All M0+M1+M2 gates still pass | full `cargo test --workspace` (210 pass) | ✅ |
| M3-G10 | `doctor` flags + `--fix`-cleans a dead-daemon footprint; live daemon untouched | `clove/tests/daemon_cli.rs::doctor_flags_and_fixes_stale_daemon_footprint` | ✅ |
| (fallback) | All reads succeed with the daemon absent (no required process) | `daemon_routing.rs` (post-stop fallback), full suite runs daemon-less | ✅ |
| (lean CLI) | `clove` binary links no `tokio`/`notify`/`git2` | `cargo tree -p clove -e normal` (notify/git2 absent; tokio only via the pre-existing M2 `github` feature) | ✅ |
| (cross build) | `cloved --no-default-features` links no libgit2 | `cargo tree -p cloved --no-default-features -i git2` (absent) | ✅ |
| (fuzz) | `ipc_frame` target 30 s clean on the committed corpus | `fuzz/fuzz_targets/ipc_frame.rs` + `fuzz/corpus/ipc_frame/` (CI `fuzz` job) | ✅ |

## Measured (debug, this sandbox)

| Metric | Gate | Measured |
|---|---|---|
| `PING` round-trip | < 5 ms | well under (sub-ms) |
| startup sweep 1k/50 → ready | < 500 ms | < 500 ms |
| stale-socket probe + cleanup | < 200 ms | < 200 ms (50 ms connect timeout + cleanup) |
| debounce 10×10 ms | exactly 1 batch | 1 |

Release numbers are equal or better; the gates are asserted in `cargo test`
(debug) so CI enforces them without a release build.

## Architecture notes / decisions

- **`clove-ipc`** is a new lean crate (serde + a synchronous `interprocess`
  client; **no tokio**) shared by the CLI (client) and `cloved` (server), so the
  daemon's async/watch/git stack never leaks into the `clove` binary.
- **`git-sync` cargo feature** (default-on) gates `git2`, mirroring M2's `github`
  feature, so lean / cross builds stay free of vendored libgit2 (DESIGN §1).
- **Transparent read routing:** when a daemon is live, `ls`/`ready`/`query` route
  through `QUERY` and the daemon returns lean rows the CLI shapes with its own
  renderer — output is byte-identical to the local index path bar
  `_meta.source = "daemon"`. `search`/`blocked` are not daemon-routed and fall
  back to the local path.
- **Windows named-event shutdown** (DESIGN §8.9) is implemented behind
  `#[cfg(windows)]` (`windows-sys`); it is compile-checked by the `daemon-windows`
  CI job. The Unix socket/signal integration tests are `#![cfg(unix)]`.
- **`synced_at` re-commit guard:** the primary guard is git2's diff-vs-`HEAD`
  check (a committed file is no longer dirty, so the next batch skips it);
  `file_mtimes.synced_at` (schema v3) additionally records each sync.

## CLI → daemon read routing

When a daemon is live, the CLI defers index/graph work to it (the daemon holds a
hot index + a cached dependency graph), falling back to the local path when the
daemon is absent. The deferral matrix (see `docs/M3_PLAN.md` §"CLI-surface review"
analysis):

| Command | Routed? | IPC | Daemon work offloaded | Parity |
|---|---|---|---|---|
| `ls` / `ready` / `query` | ✅ | `QUERY` | staleness scan + lean index read | byte-identical (`_meta.source="daemon"`) |
| `search` | ✅ | `SEARCH` | index open + FTS (CLI still reads matched files for full detail) | identical objects |
| `blocked` | ✅ | `GRAPH Blocked` | file scan + graph build + `(priority, topo, id)` order | same set/order |
| `dep tree` | ✅ | `GRAPH Tree` | file scan + graph build + traversal | same tree |
| `dep cycle` | ✅ | `GRAPH Cycles` | file scan + graph build + cycle detection | same cycles |
| `dep add` (cycle pre-check) | ✅ | `GRAPH WouldCycle` | the read-only cycle check (the write stays local) | same decision |
| `reindex` | ✅ | `REINDEX` | rebuild *and reopen* in the daemon (keeps its handle coherent) | same report |
| `show` / `comments` | ✗ | — | single-file read; a round-trip ≥ the work | n/a |
| `new`/`edit`/`set`/`status`/`label`/`assign`/`priority`/`comment` | ✗ | — | writes are one atomic file write, no index work; the daemon's watcher + self-freshening `QUERY` keep reads consistent | n/a |

The daemon's cached graph (`graph_cache.rs`) is built once from files and rebuilt
only when the watcher marks it dirty, so repeated `blocked`/`dep` queries are
served with no rescan. Every routed command degrades to its existing local path
when no daemon is running (gate (fallback) above).

## Testing layers used

Unit (frame codec, protocol serde, daemon state, idle decision), `assert_cmd`
CLI e2e (`daemon start|stop|status`, daemon-routed reads, doctor), real-process
integration (spawned `cloved` + signals + sockets), parity (daemon-source vs
direct-index reads), timing gate tests (`std::time::Instant`), real-`git`
integration via `git2` (auto-sync guards), criterion bench (`bench_frame`,
compiled in CI), cargo-fuzz (`ipc_frame`, 30 s CI replay + seed corpus).
