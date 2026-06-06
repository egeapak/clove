# clove — Web UI Plan (M4)

> **Status:** Planning. Produced by a multi-agent design/architecture pass
> (web-backend architect + frontend architect + 4 design directions). Pairs with
> `DESIGN.md` (§ refs), `IMPLEMENTATION_PLAN.md` (M4 backlog), and the visual
> mockups under `docs/web-ui-mockups/`. No code has been written yet — the visual
> theme is selected first (see "Design directions"), then the `T-W*` tasks below
> are executed.

## 1. Goal

Add a **web UI** to clove that offers a **Kanban board**, a **filterable item
list**, a **detail view**, a **timeline view**, and room for more (dependency
graph, stats dashboard, problems/cycles view). It is **read + light-write**
(status / priority / assignee / label edits, comments, dep add/rm, create). When
the **daemon** is running it serves the UI and uses its existing **file watcher**
to push **real-time updates** to connected browsers. The **CLI** gains a
dedicated **`clove serve`** subcommand that serves the UI and sets up a watcher,
working with or without a daemon.

## 2. Invariants inherited from clove (non-negotiable)

- **Files are truth.** Every write goes through `clove_core::ItemStore` (atomic
  rename + `fd-lock`, DESIGN §4). The web server never writes SQLite directly and
  never bypasses the file store — this makes web writes concurrent-safe with the
  CLI/daemon and *is* what closes the real-time loop (write → file → watcher → push).
- **`clove-core` purity** — no async / SQLite / IPC / clap / HTTP. The web server
  lives above core.
- **The graph is the parity contract.** `topological_rank` / `ready` / `blocked`
  must agree across file-scan, index, and daemon paths. The web API reuses the
  same code paths; it never reimplements ordering.
- **License cleanliness.** Tree is MIT/Apache/Zlib/Unicode only. New deps stay in
  that set and are pinned in `[workspace.dependencies]`.
- **CI builds the Rust workspace without Node.** Frontend assets are pre-built and
  committed; `cargo build` is hermetic.

## 3. Architecture overview

### 3.1 New crate: `clove-web` (lib)

A library crate housing the HTTP/WebSocket server, JSON DTOs, the service layer,
and the embedded frontend assets. Depended on by **both** binaries:

```
clove-core   (no async/SQLite/IPC/clap/HTTP)
    ↑
clove-index  (rusqlite, depends on clove-core)
    ↑         ↑              ↑
 clove-web  (axum/tower/tokio; depends on clove-core + clove-index)
    ↑                        ↑
clove (CLI)              cloved (daemon)
```

Mirrors how `clove-ipc` is shared by `clove` and `cloved`. Keeps `axum`/`tower`
out of core and out of the binaries directly. No crate cycles (does not depend on
`clove`, `cloved`, or `clove-ipc` — it is a *peer* front door into the engine).

### 3.2 Backend stack

| Crate | Version | Why | License |
|---|---|---|---|
| `axum` | 0.7/0.8 (pin current stable) | router + extractors + native WebSocket (`axum::extract::ws`); built on tokio/hyper/tower | MIT |
| `tower` / `tower-http` | 0.5 / 0.6 (`cors`,`limit`,`trace`,`set-header`) | middleware: timeouts, body limit, CORS, tracing | MIT |
| `tokio` | workspace (already pinned) | runtime — daemon already uses it | MIT |
| `rust-embed` | 8 | embed frontend `dist/` into the binary; disk-load in debug | MIT |
| `mime_guess` | 2 | content-type for embedded assets | MIT |
| `serde`/`serde_json` | workspace | DTOs | MIT/Apache |

WebSocket fan-out uses `tokio::sync::broadcast` (single watcher → many WS clients).
Use **axum's built-in WebSocket**, not a direct `tokio-tungstenite` dep.

### 3.3 Frontend stack

**SvelteKit in SPA mode** (`@sveltejs/adapter-static`, `ssr=false`) **+ Vite + TypeScript.**

- Leanest realistic runtime for a 5+ view app (Svelte compiles components away);
  matches clove's lean ethos and the "no heavy runtime / embeddable" requirement.
- Static output by construction (`build/` → `index.html` + hashed `_app/immutable/*`)
  — exactly the shape the Rust binary embeds; client-side routing with a SPA
  fallback to `index.html`.
- Fine-grained reactivity (Svelte 5 runes) is a natural fit for streaming
  WebSocket upserts into a normalized item map (only affected cards/rows re-render
  → matters at 10k items). First-party stores cover state without a Redux-scale dep.
- **Fallback if a React mandate exists:** Preact + Vite + TanStack Query/Router
  (same architecture below applies). Full React is not recommended on weight grounds.

Supporting libs (all minimal, justified): `@tanstack/svelte-virtual`
(virtualization), a ~100-line native HTML5 drag action (card→column; no heavy DnD
lib), `markdown-it` + `markdown-it-task-lists` + `DOMPurify` (CommonMark matched to
the TUI's `pulldown-cmark` feature set incl. strikethrough + task lists), `uPlot`
for the throughput chart (Gantt is hand-drawn SVG), `Intl.DateTimeFormat` (no
moment/dayjs).

### 3.4 Visual language reused from the TUI

The web UI is a **sibling of `clove-tui`** and mirrors its semantics so the two
feel identical (encode these as CSS tokens generated from / matched to
`crates/clove-tui/src/ui/style.rs`):

- status glyphs `○` open / `◐` in_progress / `●` closed;
- priority glyphs `! ↑ • ↓` for p0–p4 on a red→orange→amber→icy-blue→gray ramp
  (**p2 and p3 share `•` and differ only by hue** — a tested contract);
- single-letter type icons B/F/C/D/E with type colors (bug=red, feature=blue/green,
  chore=gray, docs=magenta, epic=gold);
- short ids (`#42`, prefix dropped);
- All / Ready / Blocked tabs and the facet-filter model
  (`app/listing.rs`, `app/filter_menu.rs`): status/assignee single-select,
  type/priority multi-OR, labels multi-AND.

**Color is never the only signal** — every color is paired with a glyph, so the UI
is colorblind-safe by construction (reuse the TUI's pairing). Themes plug in via
CSS custom properties; dark + light; WCAG AA contrast; full keyboard parity with
the TUI keys + a `?` help overlay.

## 4. Serving modes

### 4.1 Standalone — `clove serve` (no daemon required)

`crates/clove/src/cmd/serve.rs`: discover `Ctx`, build a small multi-thread tokio
runtime (like `cloved/src/lifecycle.rs`), construct `clove_web::AppState`, start a
`notify` watcher (unless `--no-watch`) using the daemon's exact watcher logic
lifted into a reusable form, refresh the index inline per debounced batch and emit
events on the broadcast channel, bind axum to `--host`/`--port`, serve until
Ctrl-C. Works with no daemon and no index (pure file-scan; index is an accelerator).

### 4.2 Daemon-integrated

When `cloved` runs it owns the watcher, hot index, and `GraphCache`. Add an
optional embedded HTTP server task to the daemon's `tokio::select!` loop, gated by
`[web] enabled`/`port` config or `cloved run --serve`. The watcher publishes a web
event onto a shared `broadcast::Sender` right after `graph.mark_dirty()` — reusing
the *exact same* incremental index/graph batch that already keeps the DB exact.
Each served web request calls `DaemonState::mark_event()` so an active web session
is a heartbeat and the daemon won't idle-shut-down under a live browser.

### 4.3 `clove serve` ↔ `clove daemon` interaction

At startup `clove serve` probes for a live daemon (50 ms liveness check):

| State | `clove serve` action |
|---|---|
| Daemon running **and serving web** | print URL, exit 0 (handoff); `--open` opens it |
| Daemon running, web **off** | start own server, **skip own watcher**, poll daemon `STATUS.batches_applied` to know when to broadcast a refresh |
| **No daemon** | full standalone: own runtime + watcher (unless `--no-watch`) |

`clove serve` never auto-starts a daemon and never takes `daemon.lock`/writes
`daemon.pid`. Starting/stopping daemons stays `clove daemon`'s job. The "skip own
watcher when a daemon is present" rule guarantees a single source of freshness.

## 5. API contract

Base path `/api/v1`. Every response uses the **existing JSON envelope (§7.3)**
(`{ v, ok, data, _meta }` / `{ v, ok:false, error:{ code, message, exit } }`).
Item payloads use the **§7.4 item schema verbatim** (reuse `item_json` shaping).
`_meta` carries `took_ms`, `source` (`files|index|daemon`), and for lists
`total`/`returned`/`offset`.

### 5.1 Read

| Method | Path | Params | `data` | Backing |
|---|---|---|---|---|
| GET | `/items` | `status,type,priority,assignee,label*`, `q`, `sort`, `dir`, `mode=list\|ready\|blocked`, `limit`, `offset` | array of items (lean) | `query_list`→file-scan; `q`→`Index::search` |
| GET | `/items/{id}` | `fields` | full item (+`body,comment_count,ready,blocked_by,dangling_deps,children_summary`) | `ItemStore::get` + `GraphStore` |
| GET | `/items/{id}/comments` | `limit` | `[{timestamp,author,body}]` | `list_comments` |
| GET | `/items/{id}/deptree` | `depth,flat` | `DepTreeNode` | `GraphStore::dep_tree` |
| GET | `/board` | `group_by=status`, list filters | `{columns:[{key,label,count,items[]}]}` | grouped graph-annotated list |
| GET | `/graph` | — | `{nodes:[{id,title,status,type,priority,ready}],edges:[{from,to,kind}]}` | `GraphStore` (new whole-graph export) |
| GET | `/stats` | `top,no_epics` | `StatsReport` | `compute_stats` |
| GET | `/stats/history` | `since,limit` | snapshot series | `Index::snapshot_history` |
| GET | `/meta` | — | `{id_prefix,types,statuses,priorities,labels,assignees,daemon:{running,web_addr},source}` | config + distinct-values |
| GET | `/cycles` / `/problems` | — | cycles / dangling / malformed-parents | `GraphStore::all_cycles`, dangling set |
| GET | `/events` | (WS upgrade) | — | broadcast channel |

### 5.2 Write (all through `ItemStore`; each returns the updated item)

| Method | Path | Body | Backing | Key errors |
|---|---|---|---|---|
| POST | `/items` | `{title,type?,priority?,labels?,deps?,parent?,assignee?,body?}` | `ItemStore::create` | 422 `VALIDATION_ERROR`/exit4 |
| PATCH | `/items/{id}` | `{status?,priority?,assignee?,type?}` | `apply_assignments`+`update` (status sets `closed` ts via model invariant) | 404; 422 |
| PUT | `/items/{id}/labels` | `{add?,remove?}` | normalize + update | 422 |
| POST | `/items/{id}/comments` | `{body,author?}` | `add_comment` | 404 |
| POST | `/items/{id}/deps` | `{dep}` | cycle pre-check + update | 409 `CYCLE_DETECTED`/exit3; 422 `SELF_LOOP`; `ALREADY_EXISTS` |
| DELETE | `/items/{id}/deps/{dep}` | — | dep rm path | 404 |
| DELETE | `/items/{id}` | `?force` | `ItemStore::delete` | 409 `HAS_DEPENDENTS` unless force |

The **Kanban drag-to-column** is just a `PATCH /items/{id}` with `{status}` (or
`{assignee}`/`{priority}`/`{parent}` when grouped by those) — no special "board
move" endpoint.

### 5.3 Error → HTTP mapping

Reuse the `CloveError → exit-code` classifier (lift `classify`/`ExitCode` into
`clove-core` so bin + web share one source of truth). Map exit → HTTP, keeping the
`error.exit` field so the contract matches the CLI exit table (§7.6):
NotFound→404, Usage→400, Cycle→409, validation class→422 (DependencyExists/
HasDependents→409), Io/NoRepo→500, Index→500, Daemon→502.

## 6. Real-time push protocol

**WebSocket** (`/api/v1/events`), with **SSE as a degradation fallback** behind a
shared client interface. Frames are serde-tagged like the IPC protocol:

```
{ "event":"hello",        "data":{ "protocol":1, "source":"daemon|standalone", "seq":N } }
{ "event":"item.upserted","data":{ "id":"…", "item": <lean item incl. recomputed ready/blocked_by> } }
{ "event":"item.deleted", "data":{ "id":"…" } }
{ "event":"batch",        "data":{ "changed":[ids], "deleted":[ids], "seq":N } }
{ "event":"stats.updated","data":{ … } }
{ "event":"ping",         "data":{ "ts":… } }
```

- **Debounced batches, not per-file events** — the watcher already coalesces a
  burst into one transaction; the server emits one batch (with recomputed derived
  fields for *all* topology-affected ids) per applied batch. A monotonic `seq`
  lets a reconnecting client detect a gap and do a full refetch.
- **No echo storms / double-apply by construction** — HTTP write handlers only
  write the file; the single watcher is the only event source. Web writes re-enter
  the same loop as CLI/editor/`git pull` changes. `index.db*` is excluded from the
  watcher; the git auto-sync `synced_at` guard prevents spurious re-commit events.

### Frontend reconciliation rules

- Normalized `Map<CloveId, Item>` is the single client cache; board columns, list
  rows, and detail all derive from it. Server-computed fields
  (`ready`/`blocked_by`/`dangling_deps`/`children_summary`/`topological_rank`) are
  treated as authoritative — the client never recomputes the graph in JS.
- **Authoritative server events win.** `item.upserted` replaces the entity
  wholesale; an `updated`-timestamp guard drops out-of-order/stale events.
- **Optimistic UI for the user's own writes** — apply immediately + record a
  pending op `{opId,id,fields,snapshotBefore}`; on success keep it (the watcher
  upsert is idempotent), on failure roll back + toast. Pending ops only protect
  their own in-flight fields against a concurrent upsert.
- **Reconnect** with exponential backoff + jitter; on reconnect, **resync** via a
  full `GET /api/items` snapshot. Offline → "stale" badge + manual refresh + gate
  optimistic writes. When no daemon/watcher exists → poll + manual refresh, badged
  "no live updates."

## 7. Views (UX spec summary)

- **Kanban (`/board`)** — columns by status (default) or assignee/priority/epic
  (URL `?group=`); cards show type icon · short id · priority glyph · title ·
  assignee avatar · label chips · blocked/dangling/ready badge (epics show a
  `children_summary` progress bar). Drag → write (status/assign/priority/parent by
  grouping), optimistic + rollback; accessible keyboard "move mode"; virtualized
  tall columns; empty-column placeholders.
- **List + filters (`/list`)** — columns status/type/id/priority/title/assignee/
  labels/updated; All/Ready/Blocked tabs with live counts; facets exactly matching
  `ViewFilter`/`Facet` (status+assignee single, type+priority OR, labels AND) +
  full-text search (client substring + a "search bodies" server FTS toggle); sort
  cycle `rank|priority|created|updated|id`; **URL-encoded** filter/sort/tab/search
  state (deep-linkable, "copy link", optional saved views); virtualized 10k rows;
  TUI-parity keybindings + `?` help.
- **Detail (`/items/:id`)** — TUI-style two-line header; metadata block; CommonMark
  body (matched to `pulldown-cmark` features + task lists, sanitized, `#id`
  autolink); sub-tabs Overview / Dep tree (from `DepTreeNode`, with `ready`
  highlight + `(cycle)` markers, plus blocks/relates/epic-children) / Comments
  (thread + add-comment, live `comment.added`); inline light-write edits
  (status/priority/assignee/labels/deps, dep-add cycle pre-check); create form;
  deep-linkable `?view=`. Body editing is **read-only in v1** (`edit --field`
  doesn't cover `body`; a body-write path is a follow-up).
- **Timeline (`/timeline`)** — (1) dependency-aware lifecycle **Gantt**: bars span
  created→(closed|now), type-colored, with hard-dep arrows and chain-highlight on
  hover; (2) **throughput** chart created vs closed over 7d/30d/all backed by
  `clove stats --history` (fallback: derive per-day from item timestamps). Zoom/pan,
  shared facet filter, click→detail, linked highlight, reduced-motion aware,
  virtualized/windowed at 10k items.
- **Extras (room for more):** dependency graph (`/graph`, layered DAG, cycle
  highlight), stats dashboard (`/stats`, full `StatsReport`), problems (`/problems`,
  doctor-style cycles/dangling/malformed parents with links).

## 8. Asset embedding & build pipeline (npm-free `cargo build`)

- Frontend builds to `crates/clove-web/dist/`; the compiled assets are **committed**
  (precedent: vendored DejaVu fonts in `clove-tui/assets/`). `clove-web/src/assets.rs`
  uses `rust-embed` over `dist/` — baked into the binary in release; read from disk
  in debug (`debug-embed` off) for fast frontend iteration. SPA fallback serves
  `index.html` for unmatched non-`/api` routes.
- A `cargo xtask build-web` runs the JS toolchain and writes `dist/`; a **separate,
  node-enabled CI job** runs it and fails if `git diff --exit-code crates/clove-web/dist/`
  is dirty (committed assets are stale). The default Rust CI matrix never touches
  Node — keeps `cargo build` hermetic (mirrors the `agent-doc --check` staleness
  pattern). Gate the embed behind a default-on `embed-assets` feature.

## 9. `clove serve` CLI spec

```
clove serve [--port N] [--host ADDR] [--open] [--no-watch]
            [--dev] [--ui-dir PATH] [--token TOKEN] [--allow-non-loopback]
```

Defaults: `--port 7373`, `--host 127.0.0.1` (loopback only). `--allow-non-loopback`
prints a security warning and **requires `--token`**. `--dev`/`--ui-dir` serve the
UI from disk. Long-running/interactive (ignores `--format`; prints the URL banner
to stderr; runs until Ctrl-C). Daemon-detect handoff per §4.3. Daemon web is
enabled via `[web]` config or `cloved run --serve`.

## 10. Security

- Loopback by default; non-loopback requires `--allow-non-loopback` + `--token`
  (constant-time compare; 401 otherwise).
- **CSRF / same-origin for writes** (loopback v1 has no auth): require
  `Content-Type: application/json` on writes; check `Origin`/`Host` and reject
  cross-origin writes via a `tower` layer.
- Path-traversal/symlink safety (DESIGN §12): item access only via the validated
  `CloveId` newtype; static serving is from the embedded set (no FS traversal); in
  `--dev` disk mode restrict to a canonicalized `ui-dir`.
- Body-size limit from `clove_core::limits` (`MAX_BODY_BYTES`; smaller cap for JSON
  control payloads) → 413; request timeouts; bounded broadcast (drop-oldest +
  `lagged` → client refetch). Token never logged.
- Concurrent-write safety is automatic (writes go through `ItemStore`'s `fd-lock` +
  atomic rename; multi-file ops lock in sorted-ID order).

## 11. Task breakdown (`T-W*`)

> Backend (`T-Wb*`) and frontend (`T-Wf*`) tracks; the API contract (§5) and event
> protocol (§6) are the seam between them and should be frozen first.

### Backend

- **T-Wb01** `clove-web` scaffold + `AppState` + envelope/error mapping. Lift
  `classify`/`ExitCode` and `item_json` shaping into `clove-core`. *AC:* each
  `CloveError` → documented HTTP status/`code`/`exit`; envelope validates schema.
- **T-Wb02** Read service + read endpoints (`/items`, `/items/{id}`, `comments`,
  `deptree`, `/board`, `/cycles`, `/meta`). *AC:* filter/pagination/detail correct;
  **parity test**: `/items?mode=ready` == `clove ready --format json`.
- **T-Wb03** Stats + timeline endpoints (`/stats`, `/stats/history`, `/graph`).
  *AC:* validates `stats.json`; history ordered; works with no index.
- **T-Wb04** Write service + endpoints (create/PATCH/labels/comment/dep ±/delete).
  *AC:* returns updated item; cycle→409/exit3; create→GET round-trips; CLI+web
  concurrent-write lock test.
- **T-Wb05** WebSocket events + broadcast plumbing. *AC:* write → matching event
  within debounce+ε; reconnect → higher `seq`; events validate `web-event.json`.
- **T-Wb06** Watcher integration (standalone): lift `cloved` watcher into a
  reusable `on_batch` form. *AC:* one batch event per debounce window; zero events
  from `index.db*` (feedback-loop guard, parallels M3-G05).
- **T-Wb07** Asset embedding + SPA fallback + dev mode + `xtask build-web` +
  staleness CI. *AC:* release build serves embedded UI with no Node; `--dev` from
  disk; staleness check passes.
- **T-Wb08** `clove serve` subcommand + daemon-detect handoff matrix. *AC:* handoff
  exits 0 when daemon serves; non-loopback without token errors.
- **T-Wb09** Daemon-integrated serving (`cloved run --serve`, watcher broadcast
  hook, `mark_event` heartbeat, `STATUS.web_addr`). *AC:* CLI write → live WS event
  via daemon; active web session blocks idle-shutdown.
- **T-Wb10** Security middleware (loopback/token/Origin/body-limit/timeout). *AC:*
  non-loopback requires token; cross-origin write rejected; oversize→413; `../`→422.
- **T-Wb11** Docs/schemas/gates (DESIGN §15 + §12.5; `board.json`/`web-event.json`;
  `M4_WEB_ACCEPTANCE_GATES.md`; agent-doc). *AC:* schemas validated; fmt/clippy/test
  green.

### Frontend

- **T-Wf01** SvelteKit SPA skeleton + embedding handshake (build → `dist/`).
- **T-Wf02** Design tokens + shared primitives (StatusGlyph/PriorityGlyph/TypeIcon/
  ShortId/LabelChip/AssigneeAvatar/BlockedBadge) matching `ui/style.rs` (incl.
  p2≠p3 hue while sharing `•`); dark/light; aria labels.
- **T-Wf03** Entities store + API client + WS/SSE channel + reconciliation reducer
  (optimistic/rollback/out-of-order guard/reconnect-resync).
- **T-Wf04** List + filters view (tabs, facets, search, sort, URL state,
  virtualization, TUI-parity keys).
- **T-Wf05** Detail view + light-write edits + comments + markdown.
- **T-Wf06** Kanban board + drag-to-change (accessible move-mode).
- **T-Wf07** Timeline (Gantt + throughput).
- **T-Wf08** Extras (graph, stats dashboard, problems).
- **T-Wf09** E2E (Playwright against the real `clove serve` binary over a fixture
  repo) + a11y (axe) + responsive hardening.

## 12. Proposed acceptance gates (`M4_WEB_ACCEPTANCE_GATES.md`)

- **W-G01** Envelope/exit parity on every error path.
- **W-G02** Read parity: `/items`, `ready`/`blocked`, `/items/{id}`, `/board` equal
  the CLI JSON for a shared fixture (ids, order, computed fields).
- **W-G03** Writes produce byte-identical on-disk frontmatter to the CLI equivalent;
  cycle/self-loop rejection codes match.
- **W-G04** Real-time: a CLI **or** web write reaches a WS client within
  debounce+250 ms; exactly one batch per window; zero `index.db*` events.
- **W-G05** Daemon integration: `cloved --serve` reuses watcher/index/graph; active
  web session resets idle-shutdown.
- **W-G06** Handoff: `clove serve` detects a web-serving daemon and hands off.
- **W-G07** Offline single-binary: `cargo build` with no Node; release serves
  embedded UI; asset-staleness CI passes.
- **W-G08** Security defaults: loopback-only; non-loopback requires token;
  cross-origin writes rejected; body limit enforced.
- **W-G09** Concurrency: simultaneous CLI + web writes never corrupt a file.
- **W-G10** No new `cargo deny` exposure; clippy `-D warnings`, fmt, `cargo test
  --workspace` green; render snapshots/screenshots regenerated where the UI changed.

## 13. Risks & open questions

1. **`classify`/`ExitCode` and `item_json` shaping live in the `clove` bin** — lift
   into `clove-core` (pure) so bin + web share one source of truth (decide before
   T-Wb01). Risk: scope creep into core.
2. **Reverse deps / whole-graph adjacency / per-item `topological_rank`** aren't all
   exposed today — board badges, timeline arrows, graph view, and `rank` sort need
   them. Add a `/graph` endpoint + extend the item payload. (Biggest cross-team dep.)
3. **Standalone-with-daemon push is coarse** (polls `batches_applied`, emits
   "refetch" not exact ids). Acceptable v1; an IPC `SUBSCRIBE` push makes it exact.
4. **Committed `dist/` churn** — noisy diffs / repo+binary bloat. Mitigated by
   precedent (vendored fonts) + feature gate + staleness CI; alternative
   (release-only asset fetch) rejected (breaks single-binary/offline goal).
5. **Optimistic write vs `git pull` race** — resolves to whatever the file says;
   the "server upsert wins except in-flight fields" rule needs careful tests.
6. **Body editing** — out of scope v1 (read-only body); needs a new write path.
7. **Default port 7373** — confirm a sane default; document override.
8. **Comment JSON shape** isn't in the v1 schema (only `comment_count`) — need a
   stable `{author,timestamp,body}` shape.
9. **Two-watchers / probe race** — the "skip own watcher when a daemon is present"
   rule must reliably dedupe; needs a test.

## 14. Design directions (visual themes — select one)

Four high-fidelity mockups were produced under `docs/web-ui-mockups/<key>/`
(`showcase.html` + `THEME.md`), each covering all four views with shared fixture
data:

1. **Midnight IDE** (`midnight-ide`) — dark, developer/IDE-native, One-Dark sibling
   to the TUI, monospace metadata, dense.
2. **Linear Light** (`linear-light`) — bright, minimal modern-SaaS (Linear/Height),
   airy whitespace, a single refined accent, keyboard-first.
3. **Solarized Duo** (`solarized-duo`) — the classic Solarized palette (warm paper
   light + dark counterpart), calm, scholarly, developer-beloved.
4. **Vibrant Glass** (`vibrant-glass`) — bold gradients + glassmorphism, frosted
   panels, energetic and premium.

The selected theme becomes the default token set (§3.4); the others can ship as
alternate themes since theming is just a CSS-variable swap.
