# Plugin system — distributing GitHub sync separately

> **Status:** Design note / feasibility. No plugin mechanism is implemented yet;
> this records the options and the recommendation for making the GitHub feature
> installable independently of the core `clove` binary.

## Problem

GitHub sync (`clove sync github`) lives in `clove-import`, gated behind the
`github` / `github-sync` cargo features (opt-out). The feature already keeps the
default *compiled-from-source* binary lean, but the **pre-built release binaries
are built `--features full`**, so every download is monolithic. There is no way
today to ship a lean core and *add* GitHub later, and there is no plugin /
extension seam anywhere in the tree (the daemon IPC in `clove-ipc` is a fixed,
versioned `tarpc` contract with no hook).

The GitHub stack is the reason it matters: enabling `github` pulls in **~150
extra transitive crates / ~3.5 MB** (octocrab, hyper, rustls/ring, jsonwebtoken).

## Current wiring (for reference)

- `crates/clove-import/Cargo.toml` — `github = ["dep:octocrab", "dep:tokio", "dep:rustls", "dep:fd-lock"]`.
- `crates/clove/Cargo.toml` (`clove-cli`) — `full = ["github", "mcp"]`, `github = ["clove-import/github"]`.
- `crates/cloved/Cargo.toml` — `full = ["git-sync", "github-sync"]`, `github-sync = ["dep:clove-import", "clove-import/github"]`.
- Pure, always-compiled: the field mapping + `clove-meta` codec (`github.rs`) and
  the whole reconciliation (`sync.rs`: `plan_sync`, `SyncState`, `plan_comments`).
- Gated: `github.rs::net` (octocrab client) and `sync_net.rs` (the network apply).
- Single entry point: `clove_import::sync_net::sync_github(...)`, a **synchronous**
  call that spins up its own Tokio runtime. Two callers: the CLI
  (`crates/clove/src/cmd/sync.rs`) and the daemon (`crates/cloved/src/github_sync.rs`).
  Neither goes through the daemon IPC.
- **Note:** `sync_net.rs` interleaves network I/O with local writes through the
  unified write path (`clove_core::apply_edit`) and persists state under
  `.clove/sync/`. Network and store-mutation are entangled in one function — this
  shapes where a plugin boundary can be drawn.

## Options evaluated

| # | Approach | Portability | Effort | Verdict |
|---|----------|-------------|--------|---------|
| a | **Two release artifacts** (`clove` lean + `clove-full`) | perfect | ~1 day | ✅ pragmatic default |
| b | Fat subprocess plugin (`clove-github` on `PATH`, JSON over stdio) | perfect | ~3–5 days | ✅ if true separable install is required |
| c | Thin subprocess plugin (child does network only; host plans + writes) | perfect | ~1.5–2 wk | ⚠️ needs `sync_net` refactor |
| d | Daemon-IPC-coupled sync method | perfect | ~1 wk | ❌ couples core protocol to GitHub |
| e | Dynamic loading (`dylib`/`cdylib` + `libloading`/`abi_stable`) | fragile | weeks | ❌ no stable Rust ABI; UB under `panic="abort"` |
| f | WASM plugin (wasmtime/extism) | n/a | weeks | ❌ octocrab/ring don't target wasm |

### (a) Two build artifacts — the cargo feature *is* most of the answer

The mechanism already exists and works cleanly. What it can't do is let someone
who downloaded a pre-built binary add GitHub afterwards. Closing that gap needs
**no new architecture** — just ship two artifacts (a lean `clove` and a
`clove-full`), or a Homebrew `--with-github` variant / a distro sub-package. Zero
risk, perfect portability. This covers ~95% of the real need.

### (b) Fat subprocess `clove-github` plugin — the true-plugin choice

A companion binary, discovered on `PATH` (git-/MCP-style), that the lean core
spawns and talks to over stdio in JSON. It fits this codebase well:

- The boundary types are **already serializable and already exist**
  (`SyncSummary` / `SyncReport` are emitted as JSON today; `EditRequest` /
  `NewSpec` / `ConflictPolicy` are the designed wire types).
- There is precedent: token resolution already shells out to the `gh` CLI.
- Portability is perfect — process spawn + pipes, no ABI, and a plugin panic
  stays isolated in the child (important: release builds use `panic = "abort"`).

The carve-out is mechanical because `sync_net.rs` is already a self-contained
gated module with one public entry point: the new `clove-github` binary is
essentially today's `sync_net` + a thin `main`, and the core replaces its direct
`sync_github(...)` call with "spawn plugin, parse the JSON result." The `github`
feature then drops out of `clove-import` entirely.

### Rejected approaches

- **(d) IPC coupling** — would bump the `tarpc` `PROTOCOL_VERSION` and hard-code a
  `sync_github` method into the core contract: the opposite of decoupling.
- **(e) Dynamic loading** — Rust has no stable ABI, and unwinding across an FFI
  boundary under `panic = "abort"` is UB; the rich return types aren't `#[repr(C)]`,
  so you'd serialize across the boundary anyway and pay the FFI cost for nothing.
- **(f) WASM** — GitHub sync is fundamentally network + TLS + a Tokio runtime;
  octocrab/hyper/rustls/ring do not compile to `wasm32` as-is. WASM would suit
  *pure* pluggable logic (e.g. a custom field-mapper), not the network layer.

## Recommendation

1. **Ship a lean core + a `full` binary as separate release artifacts.** The
   feature flag is done; this is packaging and docs, and it closes the
   "downloaded binary is monolithic" gap with zero architectural risk. **Do this
   first.**
2. **Escalate to a fat subprocess `clove-github` plugin only if** the requirement
   is genuinely an independently *installed* add-on. It is the one approach that
   keeps the 3.5 MB octocrab stack out of the core binary **and** stays
   cross-platform-safe, and the carve-out is low-risk given today's module split.

Do **not** reach for dynamic loading, WASM, or IPC coupling.
