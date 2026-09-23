# One per-user daemon ("hub") serving many projects

> **Status:** Design + implementation plan — 2026-09-23, targeting clove 0.1.1.
> Reverses DESIGN.md §8.1's "one `cloved` per `.clove/` — never system-wide"
> (the user approved the reversal). DESIGN.md §8 is rewritten alongside this.
>
> **Revised by the review fix round (same day); DESIGN.md §8 is the live
> description and wins where the two disagree.** Materially:
> - **No connection binding.** The handshake carries only the protocol version;
>   every project-scoped call carries a `Project { clove_dir, load }` — the
>   caller's own `.clove/`, made absolute and canonical *by the client* — and the
>   hub resolves it per call, refusing a relative path (`BAD_PROJECT`). `HubRpc`
>   is gone: `ping`/`hub_status`/`attach`/`detach` are `CloveRpc` methods, and
>   `detach` names the caller's own project. No RPC, MCP tool, or web API takes a
>   parameter that names another project (§2.3/§2.4 below describe the
>   superseded per-connection design).
> - **Project-agnostic spawn.** `cloved run` has no `--clove-dir`; a hub is spawned
>   bare, working from its own private runtime directory (never `/`, never the
>   spawner's directory), with a pinned minimal environment (no `GITHUB_TOKEN`; the
>   daemon's timed sync uses `gh auth token`) — §2.6 below is superseded.
> - **Exit decision is atomic with admission** (one lock); `ensure_daemon` waits
>   out a `SHUTTING_DOWN` hub and uses `hub.lock` as the liveness oracle.
> - **Security:** peer-user checks on both ends, symlink-safe runtime dir and
>   `.clove/` files, Windows per-user runtime dir + SID-keyed pipe + SID DACL +
>   server verification, periodic GitHub sync only to a project's own remote,
>   stable per-path web slugs, event-socket Origin must match host *and* port,
>   CSP on HTML.
>
> **Revised again by the second review round.** Materially:
> - **Per-project token.** `Project` also carries the project's
>   `.clove/daemon.token` (random, `0600`, per clone, git-ignored, created by
>   the first client that needs it); the hub refuses a mismatch with
>   `BAD_TOKEN`. It scopes automated clients per project — a gate for
>   automation, not a security boundary against the local user. Hub-wide calls
>   and the web UI are not token-gated.
> - **Slugs are `<name>-<path hash>`**, a function of the repository alone
>   (stable across restarts, never another repository's).
> - `script-src` pins the SPA's inline scripts by hash (no `'unsafe-inline'`),
>   the picker runs none, every response is `nosniff`; symlinked directories
>   under `.clove/` are refused; the sync remote gate reads only the work
>   tree's own `.git`; Windows checks read the pipe handle, not a pid.
>
> **Third review round.** Protocol **8** (the token is a wire change;
> `Project` refuses unknown fields). A token is trusted only as a private
> (`0600`, owned) regular file — a committed one is replaced — its creation
> git-ignores it in older repositories, and the hub reads it on every call.
> Teardowns wait as long as the caller's own deadline and a stop reports
> "still stopping" when they outlast it; a stop during a load wins; a hub
> stopped mid-load has exited when its pid file goes.
>
> **Fourth review round.** A load arms the project's watcher before its
> startup sweep and only then counts as started. Only a loading client
> writes the token (reads never do), a symlinked `.clove` gets no token work,
> and a token is trusted only if clove recorded issuing it (a per-user record
> under the clove home); `clove doctor` flags a tracked token. `clove daemon
> stop` always asks the hub (a loading project is stopped too); a hub whose
> last project stopped slowly exits once it is down; `stop --all` tells a
> restarted daemon from one that did not stop; a reindex cut off by a stop
> rebuilds locally after the daemon's.

## 1. Why

The per-project model made three things worse than the hot-path argument that
chose it made better:

1. **Web reachability.** Every project daemon competes for port 7373; exactly one
   wins, so every other project's web UI is unreachable while it runs.
2. **Process sprawl.** The MCP heartbeat keeps each project's daemon alive and idle
   shutdown defaults to 4h, so an agent-heavy day leaves one `cloved` per repo.
3. **`sun_path`.** The socket lived at `<repo>/.clove/daemon.sock`; a deep repo
   path overflows macOS's 104-byte `sockaddr_un` limit and the daemon cannot bind
   (#54 moved it to a runtime dir; the hub keeps it there).

The rejection argued "no hot-path speedup". That is still true — and irrelevant:
the hub is not faster, it is *one process and one port*.

## 2. Design

### 2.1 Process + files

One `cloved` per **user** (per runtime dir). Everything the hub owns lives in the
per-user runtime dir (the shared resolver from #54; `CLOVE_RUNTIME_DIR`
overrides it — every test sets it so parallel tests never share a hub):

| File | Purpose |
|---|---|
| `<run>/hub.sock` (Unix) / `\\.\pipe\clove-hub-<hash(run)>` (Windows) | IPC |
| `<run>/hub.pid` | hub pid, written after bind (+ any `--clove-dir` preload) |
| `<run>/hub.lock` | hub single-instance flock |

Each **slot** (a loaded project) still takes its own `<repo>/.clove/daemon.lock`,
so a project is served by at most one daemon of any version — a 0.1.0 daemon, or
a second hub under a different runtime dir, makes the attach fail cleanly
(`PROJECT_LOCKED`) and the client falls back to direct reads.

`cloved run` runs the hub. `cloved run --clove-dir X` runs the hub and preloads X
before advertising readiness (and exits 1 if X cannot load) — this keeps scripts
and the existing test harness shape working.

### 2.2 Slots

`Slot` = today's per-project daemon, minus the process: the `Dispatcher` (index,
state, graph cache, ids), the slot lock, the web `AppState` + its watcher, and a
supervised task set (file watcher, snapshot loop, github-sync loop, idle
watchdog). A slot supervisor `select!`s the slot's cancel token against its
`JoinSet`: the first task to end — normally or by panic — tears **only that
slot** down (checkpoint WAL, drop the web route, close its connections, release
`daemon.lock`). The hub and every other slot keep running.

Loads are serialized **per project** (a `OnceCell` per key) so two clients
attaching the same project at once do not both try the lock. A load that fails
(lock held, index unopenable, bad path) fails that attach only.

Idle: each slot evicts itself after its project's `[daemon] idle_shutdown_min`
(`CLOVED_IDLE_SHUTDOWN_MS` still overrides). The hub exits after it has had zero
slots for `CLOVED_HUB_GRACE_MS` (default 60 s). Detaching the last slot exits
the hub immediately.

### 2.3 Addressing: a hello frame, then the unchanged service

The project is bound **per connection**. The first length-delimited JSON frame is
a `Hello`; the hub answers one `Welcome`; then the *same* framed stream is handed
to tarpc. The 16 `CloveRpc` methods are unchanged.

```text
client → {"hello":"attach","protocol":7,"clove_dir":"/r/.clove","load":false}
hub    → {"welcome":"ok","protocol":7}                         → CloveRpc
       | {"welcome":"err","protocol":7,"code":"NOT_LOADED",…}  → close
client → {"hello":"control","protocol":7}
hub    → {"welcome":"ok","protocol":7}                         → HubRpc
```

- `load: false` (every read probe) never starts serving a project — the same
  semantics as today, where a project has daemon service only after
  `clove daemon start` / MCP auto-start. `load: true` (`ensure_daemon`) loads it.
- `HubRpc` (control): `ping`, `hub_status` (pid, uptime, web addr, per-project
  list), `detach(clove_dir)`.
- The hub canonicalizes `clove_dir` so `/tmp` vs `/private/tmp` or a worktree's
  resolved path never create two slots for one project.
- `PROTOCOL_VERSION` → **7**. The handshake carries it; a mismatch is a
  `PROTOCOL_MISMATCH` welcome (proof of life for `doctor`/`stop`), and the client
  falls back — safe because the daemon is a cache (§8.4).

### 2.4 Clients

`DaemonClient::probe(clove_dir)` keeps its signature: hub socket → hello(attach,
load=false). `ensure_daemon(clove_dir)` probes the hub, spawns `cloved run`
detached if none, waits for `hub.pid`, then hello(attach, load=true). Every
caller (engine, MCP, CLI) is untouched. New: `HubPaths` (explicit runtime dir, for
in-process tests), `probe_at`, `HubClient` (control).

CLI:

- `clove daemon start` — ensure hub + load this project.
- `clove daemon stop` — detach this project (hub exits if it was the last).
  If a **legacy (≤0.1.0)** daemon serves the project (`.clove/daemon.sock`
  answers `ping` with any version), stop *that* one (SIGTERM / named event) —
  the upgrade path for users with a live old daemon.
- `clove daemon stop --all` — stop the hub (signal/event, after the hub proves
  itself alive through a welcome).
- `clove daemon status` — this project's slot status plus `hub` (pid, web addr,
  projects).
- `clove doctor` — legacy `.clove/daemon.{sock,pid}` corpses stay a fixable
  `DAEMON_STALE_SOCKET`; a live legacy daemon is `DAEMON_LEGACY`; hub corpses in
  the runtime dir are fixable; a mismatched hub is `DAEMON_VERSION_SKEW`.
- `clove serve` — if a hub runs, load this project into it and hand off to its
  `/p/<slug>/` URL (one port for everything); otherwise standalone as before.

### 2.5 Web: one port, one prefix per project

The hub binds one listener when the first web-enabled project loads
(`CLOVED_WEB_PORT`, else that project's `[web] port`, 7373 by default; ephemeral
fallback from #55). As built, lazily: binding at hub start would have ignored
`[web] port` entirely and held the port for a hub serving only web-disabled
projects. Routes:

- `/p/<slug>/api/v1/…` and `/p/<slug>/…` — dispatched to that slot's router (the
  existing `build_router`) with the prefix stripped. Slots register/unregister
  their router on load/unload.
- `/api/v1/projects` — `{projects: [{slug, name, root, url}]}`.
- `/` — with exactly one web-enabled slot, **redirect** (and any other unprefixed
  path, so old bookmarks/deep links keep working); otherwise a small server-side
  project picker. Unprefixed `/api/*` with ≠1 slots is a 404.
- `/_app/*` — the shared immutable assets.

Slug = slugified repo basename; on collision with a loaded slot, `-<6 hex of the
path hash>`. A project with `[web] enabled = false` is not mounted.

SPA: SvelteKit reads its runtime base from `__sveltekit_<hash>.base` in the
fallback `index.html`; the slot router serves `index.html` with `base:
"/p/<slug>"` injected, so every `{base}` link already works. `api.ts` builds its
base from `$app/paths` `base`, and the WS URL is `${host}${base}/api/v1/events`.
`+layout.svelte` gets a project switcher (shown when `/api/v1/projects` lists >1).

`STATUS` gains `web_url` (`http://127.0.0.1:7373/p/<slug>/`); `web_addr` stays
`host:port`. `clove serve`'s hand-off and `daemon status` use `web_url`.

### 2.6 Environment

The hub is spawned by whichever client first needed it and keeps **that**
environment: `CLOVED_DISABLE_WEB`, `CLOVED_WEB_PORT`, `CLOVED_IDLE_SHUTDOWN_MS`,
`CLOVED_HUB_GRACE_MS`, `CLOVED_GITHUB_SYNC_MS`, and `GITHUB_TOKEN` (inherited by
every project's github-sync child). Per-project behaviour comes from each
`.clove/config.toml`. Documented in DESIGN §8.8; `clove daemon stop --all`
restarts it with a fresh environment.

### 2.7 Windows

Pipe `clove-hub-<fnv(runtime dir)>` and event `clove-hub-shutdown-<same>`: per
user (the runtime dir is per user) and isolatable in tests. If `interprocess`
exposes it, the pipe gets an owner-only security descriptor. Not runnable here —
only `cargo check --target x86_64-pc-windows-gnu`.

## 3. File-by-file

| File | Change |
|---|---|
| `clove-ipc/src/lib.rs` | `HubPaths` (sock/pid/lock, socket name, event name) on the #54 runtime dir; legacy `.clove/daemon.*` helpers kept as `legacy_*` |
| `clove-ipc/src/hub.rs` (new) | `Hello`/`Welcome`, codes, `HubRpc` service, `HubStatus`/`ProjectInfo`, hello framing helpers |
| `clove-ipc/src/protocol.rs` | v7; `StatusResponse.web_url` |
| `clove-ipc/src/client.rs` | hello-then-tarpc connect; `probe`/`probe_at`/`attach`; `HubClient`; health over the hub; legacy probe |
| `clove-ipc/src/spawn.rs` | spawn `cloved run` (no dir); ensure = probe → spawn → wait `hub.pid` → attach(load) |
| `cloved/src/main.rs` | `run [--clove-dir]` |
| `cloved/src/hub.rs` (new) | hub registry, per-key load, accept loop w/ hello, control service, grace exit |
| `cloved/src/slot.rs` (new) | slot load (from old `lifecycle::run` body), supervisor, teardown |
| `cloved/src/lifecycle.rs` | hub lock/bind/pid/signals/teardown only |
| `cloved/src/state.rs` | `web_url` |
| `clove-web/src/hub.rs` (new) | `HubWeb` registry + router (prefix dispatch, projects, picker, redirect) |
| `clove-web/src/assets.rs`, `lib.rs` | base-injected `index.html` per slot (`AppState::with_base_path`) |
| `clove-web/web/src/lib/api.ts`, `store.svelte.ts`, `routes/+layout.svelte` | base-derived API/WS URLs, project switcher |
| `clove/src/cli.rs`, `cmd/daemon.rs` | `stop --all`, status with hub, legacy stop |
| `clove/src/cmd/serve.rs`, `cmd/doctor.rs` | hand-off via hub; hub/legacy health |
| `docs/DESIGN.md` §8 | rewritten |
| tests | below |

## 4. Tests (TDD — written first)

- `cloved/tests/daemon_hub.rs` (new): two projects on one hub (both answer, each
  its own items); slot failure isolation (a project whose index cannot open fails
  its attach, and a loaded slot whose `issues/` dir vanishes is unloaded, while the
  other keeps answering); per-slot idle eviction then hub exit after grace;
  a project locked by a legacy daemon (`daemon.lock` held) → `PROJECT_LOCKED`,
  probe `None`, the other project unaffected; `detach` one vs hub shutdown;
  concurrent loads of one project yield one slot.
- `cloved/tests/daemon_{lifecycle,ipc,watch,git_sync}.rs`: runtime-dir isolation,
  readiness = `hub.pid`, protocol v7.
- `clove/tests/daemon_cli.rs`: `start` two projects → one hub pid; `stop` A keeps
  B; `stop --all`; status lists both; doctor legacy/hub footprints.
- `clove/tests/daemon_routing.rs`, `mcp.rs`, `sort_order.rs`, `filter_parity.rs`:
  `CLOVE_RUNTIME_DIR` per test.
- `clove-web/tests/hub.rs` (new): `/p/<slug>` API routing; cross-project isolation
  (a write in A never shows in B); root redirect with one slot, picker with two;
  `/api/v1/projects`; injected base in `index.html`; unknown slug 404; host guard.
- `cloved` hub web end-to-end: two slots on one port, `STATUS.web_url` per project.
- vitest: `apiBase('')`/`apiBase('/p/x')`, `eventsUrl` for http/https + base.

## 5. Risks

- **Blast radius.** One hub process now serves every project, so a hub crash
  takes all projects' acceleration down at once. Mitigated by per-slot
  supervision; the daemon remains a cache, so clients degrade to direct reads.
- **Shared environment** (§2.6), notably one `GITHUB_TOKEN` for every project.
- **Two-worker runtime** shared by all slots; blocking work already runs on the
  blocking pool.
- **Base injection** depends on SvelteKit's `__sveltekit_<hash> = { base: "" }`
  fallback shape; a test pins it against the embedded build.
- **Windows** is compile-checked only.
