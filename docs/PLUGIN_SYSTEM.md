# Plugin system — cargo-style external subcommands & pluggable integrations

> **Status:** Design — implementation-ready. No plugin mechanism is implemented
> yet. This specifies a **cargo-style external-subcommand** dispatch seam for
> `clove`, so integrations users don't want (GitHub, and future GitLab / Jira /
> Linear) are **separately installable binaries** rather than compile-time
> features baked into one monolithic release artifact. It supersedes the earlier
> feasibility note (the options table in §9 is kept from it).

## 1. Goal

Let a lean `clove` core ship without any network-integration weight, and let a
user *add* an integration afterwards by dropping a binary on `PATH` — the way
`cargo install cargo-nextest` makes `cargo nextest` work. Concretely:

```
clove sync github egeapak/clove      →  exec  clove-sync-github  egeapak/clove
clove sync gitlab grp/proj           →  exec  clove-sync-gitlab  grp/proj
clove frobnicate --wibble            →  exec  clove-frobnicate   --wibble
```

The motivation is real weight: enabling the `github` feature pulls in **~150
transitive crates / ~3.5 MB** (octocrab, hyper, rustls/ring, jsonwebtoken).
Every future tracker adds its own client stack. A plugin seam keeps all of it out
of the core binary and off the machine of anyone who doesn't use it.

**Non-goal:** in-process dynamic loading (`dylib`/WASM). Rust has no stable ABI
and release builds use `panic = "abort"`; §9 records why these are rejected. The
plugin boundary is a **subprocess boundary**, exactly like cargo and git.

## 2. How cargo does it (the reference model)

When you run `cargo foo` and `foo` is not a built-in subcommand, cargo:

1. Searches for an executable named **`cargo-foo`**, looking in the directories
   of `$PATH` **plus** the cargo bin directory (`$CARGO_HOME/bin`) and the
   sysroot libexec dir.
2. Execs it as `cargo-foo foo <remaining args…>` — note argv[1] is the
   subcommand name itself, so the plugin can be invoked both as `cargo foo` and
   directly as `cargo-foo foo`.
3. Exports **`CARGO`** (path to the cargo binary the plugin should call back
   into) and other `CARGO_*` env into the child.
4. Surfaces plugins in `cargo --list` and errors with a "did you mean" /
   "no such subcommand" message (suggesting `cargo install`) when nothing
   resolves.

git uses the same pattern (`git-foo` on `PATH`, `git foo` dispatches to it), as
do `kubectl` plugins (`kubectl-foo`) and `gh` extensions. clove already shells
out to the `gh` CLI for token resolution, so subprocess-based extension has
precedent in the tree.

clap — which clove already uses — supports this natively via
`#[command(external_subcommand)]`; see §4.

## 3. What clove needs beyond cargo's model

cargo subcommands are fully standalone: `cargo-foo` re-reads `Cargo.toml` itself
and shares nothing but the filesystem. clove has two extra requirements:

1. **Two-level dispatch.** The user asked for `clove sync github` →
   `clove-sync-github`, not just `clove github`. `sync` / `import` / `export` are
   *multiplexer* commands that own shared concepts (a `--dry-run` plan, a
   `--prefer` conflict policy, the `owner/repo` target shape). They should stay
   built-in and treat the **provider** (`github`, `gitlab`, …) as the extension
   point. So clove needs both:
   - **generic** external subcommands (`clove <x>` → `clove-<x>`), the cargo case;
   - **provider** subcommands under a multiplexer (`clove sync <p>` →
     `clove-sync-<p>`).
2. **Store access.** A `clove-sync-github` plugin must *read and write the clove
   store* — create items from issues, write `external_ref` back, append comments,
   persist `.clove/sync/` fingerprints. cargo plugins never touch cargo's
   internals; clove plugins mutate the source of truth. §6 defines how, without
   breaking the "unified write path" invariant.

## 4. Dispatch seam (the code change in `clove`)

Today `Commands` (in `crates/clove/src/cli.rs`) is a **closed** clap enum, and
`SyncArgs.tracker` is a closed `ValueEnum` (`SyncTracker::Github`). Both must gain
a fall-through.

### 4.1 Generic external subcommands

Add a catch-all variant to `Commands`:

```rust
#[derive(Subcommand)]
pub enum Commands {
    // …all existing built-ins…
    /// Run an external subcommand plugin (`clove-<name>` on PATH).
    #[command(external_subcommand)]
    External(Vec<String>),   // argv[0] is the subcommand name
}
```

In `main::dispatch`, a new arm handles `Commands::External(argv)`: resolve
`clove-<argv[0]>` (§5), and if found `exec` it (§6). If not found, emit the
standard usage error (exit 1) with an "unknown subcommand — is a plugin
installed?" hint listing what *is* on `PATH` (§7).

`external_subcommand` only fires when the token matches **no** built-in, so it
never shadows real commands and is zero-risk for existing behavior.

### 4.2 Provider fall-through for `sync` / `import` / `export`

These stay built-in but stop hard-coding their provider set. `SyncArgs` becomes:

```rust
#[derive(Args)]
pub struct SyncArgs {
    /// The provider to sync with (built-in or a `clove-sync-<provider>` plugin).
    pub provider: String,
    /// Everything after the provider is forwarded to the plugin verbatim.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}
```

Dispatch for `sync`:

1. If `provider` is a **compiled-in** provider (see §8 on whether any remain),
   run it in-process as today.
2. Otherwise resolve `clove-sync-<provider>` (§5) and exec it with `rest`
   forwarded (§6). The `sync`-level conventions (`--dry-run`, `--prefer`,
   `--no-comments`) are passed through in `rest` and are a **documented contract**
   every sync plugin must honor (§6.3), so the host need not know each provider's
   flags.
3. If neither resolves: exit 4 (`ValidationError`) — "unknown sync provider
   `<p>`; install `clove-sync-<p>`".

`import` and `export` get the identical treatment (`clove-import-<p>` /
`clove-export-<p>`). This is what produces the exact behavior asked for:
`clove sync github egeapak/clove` → `clove-sync-github egeapak/clove`.

### 4.3 Resolution precedence

For a bare `clove <x>`:

1. Built-in subcommand — always wins (a plugin can never shadow a built-in).
2. `clove-<x>` on the plugin search path (§5).
3. Otherwise usage error.

For `clove sync <p>` the analogous order is: built-in provider → `clove-sync-<p>`
→ generic `clove-<p>`? **No** — do *not* fall back to a generic `clove-<p>` for a
provider miss; that would make `clove sync typo` silently run an unrelated
`clove-typo`. A provider miss is an error scoped to the multiplexer.

## 5. Plugin discovery & search path

A candidate binary name is `clove-<segments-joined-by-dash>` with the platform
executable suffix (`.exe` on Windows). Search, in order:

1. **The directory of the running `clove` binary** (`std::env::current_exe()`) —
   so a plugin installed next to `clove` (the common `cargo install` case) is
   found even if that dir isn't on `PATH`.
2. **`$CLOVE_PLUGIN_PATH`** (`:`-separated, or `;` on Windows) — explicit opt-in
   dirs, for dev/testing and non-standard installs.
3. **Every directory on `$PATH`.**

First match wins. Resolution is a pure `stat`-for-executable walk (no exec) so
`clove --list` / help can enumerate cheaply. On Unix, require the file be
executable (`X_OK`); on Windows, match `PATHEXT`. This mirrors cargo's
`current_exe dir + PATH` strategy — importantly it does **not** require the user
to have the cargo bin dir on `PATH`, which is a common friction point.

A small always-compiled module `crates/clove/src/plugin.rs` owns: name
construction, path search, `resolve(name) -> Option<Utf8PathBuf>`, and
`list() -> Vec<PluginInfo>`.

## 6. Host ↔ plugin contract

### 6.1 Invocation

The host **replaces its own process image** where possible (`exec` on Unix via
`std::os::unix::process::CommandExt::exec`; spawn-and-wait, propagating the exit
code, on Windows). Rationale: a plugin like `clove tui`-style or an interactive
auth flow needs the real tty, and streaming output must not be buffered by the
host. For `sync`/`import`/`export` where the host wants to *post-process* the
plugin's result (rare), it spawns and pipes instead — but the default is exec.

argv passed to the plugin (cargo-compatible):

```
clove-sync-github  sync github  <rest…>      # argv[1..3] echo the path taken
```

i.e. the plugin receives the multiplexer + provider (or the bare subcommand for
the generic case) as leading args, then the forwarded tail — so the same binary
works when invoked directly (`clove-sync-github egeapak/clove`) by treating a
leading `sync github` as optional. (This matches cargo passing `foo` as argv[1].)

### 6.2 Environment handed to the plugin

| Var | Meaning |
|-----|---------|
| `CLOVE` | Absolute path to the host `clove` binary, for callback (see §6.4). Mirrors cargo's `CARGO`. |
| `CLOVE_DIR` | The **resolved** `.clove/` directory, so the plugin skips its own discovery and can never disagree with the host about which repo it's in. |
| `CLOVE_ROOT` | The repo root (parent of `.clove/`). |
| `CLOVE_FORMAT` | `human` \| `json` \| `jsonl` — the resolved output format (flag > env > config), so the plugin emits the same envelope the user asked for. |
| `CLOVE_COLOR`, `CLOVE_QUIET` | Forwarded global UX flags. |
| `CLOVE_PLUGIN_API` | Contract version (integer, starts at `1`) so a plugin can refuse a host it's too old/new for. |

The plugin inherits the rest of the environment (so `GITHUB_TOKEN` etc. flow
through unchanged).

### 6.3 Output & exit codes — plugins are first-class clove citizens

A plugin **must** obey the same contracts as a built-in so scripts and agents
can't tell the difference:

- **JSON envelope** (`DESIGN.md` §7.3): `{ v, ok, data, _meta }` on success,
  `{ v, ok:false, error:{ code, message, exit } }` on failure, on **stdout**.
- **Exit-code table** (`DESIGN.md` §7.6): `0` success, `4` validation, `5` I/O,
  `7` for a provider/daemon comms failure, etc.
- **stderr** is human narrative only; `CLOVE_QUIET` suppresses it.

To make conformance cheap (and keep the envelope defined in one place), factor
the envelope writer + exit-code mapping — today private to `crates/clove/src/{output,exit}.rs`
and already backed by `clove_types::error_code` — into a tiny **`clove-plugin`**
support crate that both the host and every first-party plugin depend on. A
plugin's `main` then looks like today's `cmd/sync.rs` body with a `clove-plugin`
harness around it.

The `sync`/`import`/`export` conventions the plugin must accept in its tail args:
`--dry-run` (plan only, write nothing — emit the same `SyncSummary` JSON shape),
`--format` (already in `CLOVE_FORMAT` too), and for sync `--prefer <policy>` /
`--no-comments`.

### 6.4 Store access — preserving the "unified write path"

This is the crux. The DESIGN's central invariant is *one* write path
(`clove_core::apply_edit` / `NewSpec` / `EditRequest`) shared by every surface.
Two ways a plugin can honor it:

**(A) Fat plugin — link `clove-core` (recommended).** The plugin binary depends
on `clove-core` + `clove-types` and opens the store itself using `CLOVE_DIR`,
mutating **through `clove_core::apply_edit`**. Because those mutators live in
shareable crates, a plugin that links them *is* using the unified write path —
the invariant holds at the source level even across the process boundary. This is
almost exactly today's `sync_net.rs`, which already drives `apply_edit` and
persists `.clove/sync/`. The carve-out is mechanical: `clove-sync-github` ≈
today's `sync_net` + a `clove-plugin` `main`, and `clove-import` drops its
`github` feature. **This is the recommended approach** — lowest effort, reuses
the existing entangled network+write code as-is, perfect portability.

Cost: the plugin re-implements repo *open* (trivial — `ItemStore::new(root)` +
`load_config`, both in `clove-core`) and pins a `clove-core` version. On-disk
format is `schema`-versioned (`DESIGN.md` §2.4), so a plugin built against an
older `clove-core` still reads/writes valid files or fails loudly on an unknown
schema — the same guarantee two `clove` binaries already give each other.

**(B) Thin plugin — plan-only, host applies.** The plugin does *network only* and
emits a `SyncSummary`-shaped plan (`pull_create`, `push_update`, comment plan) on
stdout as `NewSpec`/`EditRequest` batches; the host reads it back and applies via
`apply_edit`, keeping **all** mutation literally in the host process. The wire
types already exist and are serializable (`SyncSummary`, `NewSpec`, `EditRequest`,
`ConflictPolicy`). This is the purest form of the invariant but requires
splitting `sync_net.rs` — which today interleaves fetch → apply → `external_ref`
write-back → comment reconcile — into a plan phase and an apply phase with the
process boundary between them. More work, deferred (§10).

Recommendation: **ship (A)**; keep (B) as a future hardening step if untrusted
third-party plugins ever become a goal (a fat plugin runs arbitrary code with the
host's store access — see §7).

## 7. Listing, help, and errors

- **`clove plugin list`** (new built-in): enumerate resolvable `clove-*` binaries
  with their path and, if the plugin supports it, a one-line description. A plugin
  advertises metadata by responding to `clove-foo --clove-plugin-info` with a
  small JSON blob (`{ name, version, provides: ["sync:github"], about }`); this is
  optional and cached nowhere (cheap enough on demand). This also lets
  `clove sync --help` list *installed* providers, not just built-in ones.
- **`clove --list`** appends discovered external subcommands under an "external
  subcommands" heading, cargo-style.
- **Unknown subcommand / provider** errors name the expected binary and suggest
  installation, e.g. `unknown sync provider 'gitlab'; install it with 'cargo
  install clove-sync-gitlab' (or drop clove-sync-gitlab on PATH)`.

## 8. Migration: GitHub becomes the first plugin

The github integration is the proving ground and directly serves the motivation.

1. **Extract `crates/clove-sync-github/`** — a new binary crate = today's
   `clove_import::{github::net, sync_net}` + a `clove-plugin` `main`. It depends
   on `clove-core`, `clove-types`, `clove-import` (for the *pure* `map`/`sync`
   planning + `github` field codec), and the `github`-only deps (octocrab, tokio,
   rustls, fd-lock) unconditionally — no feature gate; the whole crate *is* the
   opt-in unit now.
2. **`clove-import`** keeps its pure layers (`github` field mapping/codec,
   `sync.rs` planning, `plan_comments`) always-compiled and **drops the `github`
   feature** (the `dep:octocrab/tokio/rustls/fd-lock` gate and `sync_net.rs` move
   to the new crate).
3. **`clove-cli`** drops its `github` feature; `cmd/sync.rs` becomes the generic
   provider-dispatch shim (§4.2). The `full` feature loses `github`.
4. **The daemon coupling** (`crates/cloved/src/github_sync.rs` calls
   `sync_net::sync_github` as a library, behind `github-sync`). The daemon can no
   longer link it. Resolve by making the daemon's periodic auto-sync **shell out
   to the plugin** — `cloved` spawns `clove sync github <repo>` (which resolves
   the plugin) on its interval, exactly as a user would. This *removes* octocrab
   from `cloved` too (another lean win) and unifies "unattended sync" with "manual
   sync" onto one code path. `github_sync.rs` shrinks to a `Command::spawn` +
   interval loop; the `github-sync` feature becomes a thin `git-sync`-style gate
   that only decides whether to *spawn*, carrying no network deps.
5. **Release packaging.** Publish `clove` (lean) and `clove-sync-github` as
   separate artifacts / crates. `cargo install clove-sync-github` then makes
   `clove sync github` light up with no core rebuild. The old "two artifacts"
   recommendation (§9a) is subsumed: the lean core *is* the default, and
   integrations are add-ons.

Nothing about the on-disk format, the `SyncSummary`/`SyncState` types, or the
`.clove/sync/` fingerprints changes — only *where the code lives* and *how it's
invoked*.

## 9. Options previously evaluated (retained for the record)

| # | Approach | Portability | Effort | Verdict |
|---|----------|-------------|--------|---------|
| a | **Two release artifacts** (`clove` lean + `clove-full`) | perfect | ~1 day | ✅ interim; subsumed by the plugin design |
| b | **Fat subprocess plugin** (`clove-sync-github` on `PATH`) | perfect | ~3–5 days | ✅ **this design** (§4–8) |
| c | Thin subprocess plugin (child networks; host plans+writes) | perfect | ~1.5–2 wk | ⚠️ future hardening (§6.4B) |
| d | Daemon-IPC-coupled sync method | perfect | ~1 wk | ❌ hard-codes GitHub into the core `tarpc` contract |
| e | Dynamic loading (`dylib`/`cdylib` + `libloading`/`abi_stable`) | fragile | weeks | ❌ no stable Rust ABI; UB under `panic="abort"` |
| f | WASM plugin (wasmtime/extism) | n/a | weeks | ❌ octocrab/ring don't target `wasm32` |

Cargo's feature flag already gives (a) for free at build time; the plugin design
generalizes it to *install time* without any of the ABI/WASM hazards of (e)/(f),
because the boundary is a subprocess.

## 10. Security, versioning, open questions

- **Trust.** `clove` will exec any `clove-*` on `PATH`, exactly like cargo/git/
  kubectl. A fat plugin (§6.4A) runs with full store write access. This is the
  same trust model as any `cargo install`ed subcommand; document it. If untrusted
  third-party plugins ever matter, the thin plan-only boundary (§6.4B) plus a
  capability handshake is the mitigation — deferred.
- **Contract versioning.** `CLOVE_PLUGIN_API` (§6.2) is bumped only on a breaking
  change to the env/argv/envelope contract; the JSON envelope stays `v:1`
  independently. A plugin that sees an unknown `CLOVE_PLUGIN_API` should still
  attempt to run and warn, not hard-fail (forward-compat).
- **Windows exec.** No `execvp`; spawn-and-wait propagating the child exit code,
  and forward Ctrl-C. Interactive plugins get the real console since the host
  isn't holding the pipes.
- **Open questions:** (1) Do any providers stay compiled-in, or is the multiplexer
  provider-set *always* external? (Recommendation: always external — even github —
  so the host has zero network deps and one dispatch path.) (2) Should
  `clove plugin list` cache `--clove-plugin-info` results in the index? (Probably
  not for v1.) (3) Naming for multi-word generic subcommands (`clove foo bar` →
  `clove-foo` with `bar` as an arg, cargo-style, vs. `clove-foo-bar`) — adopt
  cargo's rule: only the **first** unknown token names the binary for the generic
  case; the multiplexer case (§4.2) is the only place a second segment joins the
  name.

## 11. Phased implementation plan

1. **Seam only (no behavior change).** Add `Commands::External` + `plugin.rs`
   (resolve/list), `CLOVE`/`CLOVE_DIR`/… env, exec-or-spawn, and the unknown-
   subcommand error. GitHub stays a feature. Ships the cargo-style generic path
   and `clove plugin list` / `clove --list`. Fully testable with a fixture
   `clove-echo` plugin — no network.
2. **`clove-plugin` support crate.** Extract the envelope + exit-code harness so
   plugins conform trivially; add the `--clove-plugin-info` metadata protocol.
3. **Provider fall-through.** Reshape `SyncArgs`/`ImportArgs`/`ExportArgs` to
   `provider: String + trailing rest`, and dispatch to `clove-sync-<p>` etc.
4. **Extract `clove-sync-github`** (§8 steps 1–3), drop the `github` feature from
   `clove-import`/`clove-cli`.
5. **Rewire the daemon** to spawn the plugin (§8 step 4); drop octocrab from
   `cloved`.
6. **Packaging & docs** (§8 step 5): publish the plugin artifact, update
   `DESIGN.md` §1/§7 and `RELEASE.md`, note the new seam in `CLAUDE.md`.

Steps 1–2 are independently valuable (they unlock *any* third-party subcommand)
and carry no risk to existing github behavior; 3–6 are the github migration.
