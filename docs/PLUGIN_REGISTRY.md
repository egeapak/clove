# Plugin registry, install & discovery

> **Status:** Design — implementation-ready (planned by a design team, decisions
> adopted). Builds on the cargo-style plugin dispatch in
> [`PLUGIN_SYSTEM.md`](PLUGIN_SYSTEM.md). Adds: an enriched `clove plugin list`,
> a curated registry (`clove plugin list --all`), `clove plugin install/uninstall/
> update`, and **dynamic, plugin-aware `--help`** for the `import`/`export`/`sync`
> multiplexers. **The core dispatch path stays network- and dependency-free** —
> everything here lives only on the `plugin install` / `list --all` path.

## Goal

Today plugins are installed by hand (`cargo install --git … clove-sync-github`)
and `clove plugin list` shows only `name` + `path`. This makes plugins
first-class and discoverable:

```
clove plugin list                 # installed plugins: version, what they provide, compat
clove plugin list --all           # + available plugins from the curated registry
clove plugin install github       # cargo-install (or download) the binary(ies) for `github`
clove plugin uninstall github
clove plugin update [--all]
clove import --help               # shows built-in providers AND installed import plugins
```

## Adopted decisions (from the design team)

1. **Manifest fetch — bundled + shell-out.** The registry is a TOML file compiled
   into `clove` (`include_str!`); it works fully offline. `--refresh` fetches the
   live copy by shelling out to `curl` (fallback `gh api`), matching clove's
   existing "shell out to `gh`/`git`" precedent. **No TLS/HTTP crate is added to
   the core.**
2. **Install v1 — `cargo install --git --tag`.** Build the plugin from the exact
   clove tag, so `CLOVE_PLUGIN_API`/`CLOVE_SCHEMA` compatibility is guaranteed by
   construction (same workspace revision). Prebuilt-binary download and crates.io
   publishing are deferred (Phase 3 / future); the manifest schema already carries
   the fields to enable them without a redesign.
3. **Install location — a clove-managed root.** `cargo install --root
   $CLOVE_HOME` (default `~/.clove`) puts binaries in `$CLOVE_HOME/bin`, which is
   added to the §5 search path (after the current-exe dir, before
   `$CLOVE_PLUGIN_PATH`/`$PATH`). cargo's `--root` bookkeeping gives a precise
   installed-set for clean `uninstall`/`update`, and it's the future download
   target.
4. **Phased delivery** (below).

## 1. Registry manifest

Committed at `registry/plugins.toml`, compiled into `clove` via `include_str!`
(the offline default), and reachable live at
`raw.githubusercontent.com/egeapak/clove/<ref>/registry/plugins.toml`.

```toml
# registry/plugins.toml
schema = 1                       # registry-format version (migratable)

[source]
git = "https://github.com/egeapak/clove"   # default install source for all binaries

[[plugin]]
name           = "github"                  # friendly: `clove plugin install github`
description    = "Two-way GitHub Issues sync"
min_plugin_api = 1                         # must be <= host CLOVE_PLUGIN_API
  [[plugin.binary]]
  bin      = "clove-sync-github"           # binary resolve() must find
  crate    = "clove-sync-github"           # cargo package (crates.io / --git)
  provides = ["sync:github"]               # dispatch tokens (cross-checked vs --clove-plugin-info)
  # future prebuilt download (Phase 3):
  # [plugin.binary.download]
  # url    = ".../releases/download/v{version}/clove-sync-github-{target}{ext}"
  # sha256 = ".../releases/download/v{version}/clove-sync-github-{target}.sha256"

[[plugin]]
name = "tk"
description = "Import from a tk .tickets/ directory"
min_plugin_api = 1
  [[plugin.binary]]
  bin = "clove-import-tk"
  crate = "clove-import-tk"
  provides = ["import:tk"]

[[plugin]]
name = "beads"
description = "Import from a Beads issues.jsonl"
min_plugin_api = 1
  [[plugin.binary]]
  bin = "clove-import-beads"
  crate = "clove-import-beads"
  provides = ["import:beads"]

# A future multi-binary entry: one friendly name → several binaries.
# [[plugin]]
# name = "gitlab"
#   [[plugin.binary]]  bin="clove-sync-gitlab"    provides=["sync:gitlab"]   crate="clove-sync-gitlab"
#   [[plugin.binary]]  bin="clove-import-gitlab"  provides=["import:gitlab"] crate="clove-import-gitlab"
#   [[plugin.binary]]  bin="clove-export-gitlab"  provides=["export:gitlab"] crate="clove-export-gitlab"
```

TOML matches the house style (`config.toml`; the parse-only `toml` crate is
already a dependency). A friendly `name` maps to an array of binaries; install
iterates them and reports per-binary status (no silent half-install).

## 2. Host↔plugin compat fields (`--clove-plugin-info`)

Extend the plugin metadata (`clove-plugin`) so `plugin list` and dispatch can
gate compatibility. All are compile-time constants (plugin and host share the
`clove-plugin` crate, so the built-against value is known):

| Field | Meaning |
|---|---|
| `clove_plugin_api` | the contract version the plugin was built against |
| `min_clove_plugin_api` / `max_clove_plugin_api` | host-contract range it tolerates (defaults: the built value; `max` may be generous per the forward-compat rule) |
| `max_schema` | highest on-disk item `CLOVE_SCHEMA` a store-touching plugin can handle |

`plugin list` compares the host `CLOVE_PLUGIN_API = H` to the plugin's range:
`min ≤ H ≤ max` → **ok**; `H > max` → **outdated** (runs, with a warning);
`H < min` → **needs newer clove** (dispatch refuses, exit 4); probe fails →
**unknown** (legacy plugin, still listed/run).

## 3. `clove plugin list` (enriched, offline)

`plugin::list()` (the pure stat-walk) stays the discovery primitive; a new
enrichment layer probes each `clove-*` binary with `--clove-plugin-info`
(bounded by a short per-probe timeout so a hung/old plugin can't wedge the
command). A plugin that doesn't answer is still listed (name/binary/path,
`status:"no_info"`, `commands` derived from the name heuristic).

Human:

```
NAME          VERSION  RUN AS               ABOUT
sync-github   0.1.0    clove sync github    Two-way GitHub sync
import-tk     0.1.0    clove import tk      Import from a .tickets/ dir
frobnicate    —        clove frobnicate     (no metadata)
```

JSON (additive over today's `{name,path}`, one object per plugin; `provides`
→ `commands` via `sync:github` ⇒ `clove sync github`):

```json
{ "name":"sync-github", "binary":"clove-sync-github", "path":"…",
  "version":"0.1.0", "about":"Two-way GitHub sync", "provides":["sync:github"],
  "commands":["clove sync github"], "installed":true, "status":"ok" }
```

## 4. `clove plugin list --all`

Merges the enriched installed set with the registry's available plugins
(bundled manifest; `--refresh` fetches live). One flat `data` array with
`installed` + `status ∈ {ok,no_info,available}` as the discriminator (keeps JSONL
clean); human output shows an *Installed* and an *Available* section. A
registry-fetch failure **degrades** — the Installed section still prints, with the
error as a warning (`_meta.registry_error`).

## 5. `clove plugin install / uninstall / update`

- **`install <name>`** resolves `name` **only** through the curated manifest,
  then for each binary runs `cargo install --locked --git <source.git> --tag
  v<CLOVE_VERSION> <crate> --root $CLOVE_HOME`. After each, it probes
  `--clove-plugin-info`, verifies `provides` matches the manifest, and checks
  `min_plugin_api ≤ CLOVE_PLUGIN_API`.
- **Confirmation.** A TTY human run prompts with the exact command
  (`--yes` skips); non-TTY/JSON proceeds (scriptable); `--force` reinstalls.
- **`uninstall <name>`** → `cargo uninstall --root $CLOVE_HOME <crate>` per binary
  (clean because of the managed root). **`update [<name>|--all]`** reinstalls with
  `--force`.
- **Failure modes:** unknown name → exit 4 (lists available); no `cargo` → exit 5
  with a rustup pointer; network fail → exit 5; already installed → no-op (exit 0,
  `--force` to override); incompatible after install → warn (error under
  `--strict`) and flag in `list`.
- **Trust:** curated-only by default (no arbitrary URLs); an escape hatch
  (`--git <url>`) prints a loud "un-curated code" warning; `cargo install` builds
  arbitrary build scripts — the same trust model as any `cargo`/`git`/`kubectl`
  plugin, bounded by the first-party manifest. Downloads (Phase 3) verify sha256.

## 6. Dynamic plugin-aware `--help`

`clove import --help` (and `export`/`sync`) must list built-in **and** installed
plugin providers, but clap's derive `after_help` is a compile-time string. The
solution: **intercept `<mux> --help` in argv before `Cli::try_parse()`**, reusing
the existing *"global flags precede the provider; everything after the provider is
forwarded raw"* rule.

In `main()`:

```
if let Some(mux) = mux_help::detect(&argv) { mux_help::render(mux); return SUCCESS; }
let cli = Cli::try_parse() … // unchanged for every other argv
```

- **`detect(argv)`** skips the global flags (a shared const list of the
  `global=true` flags, pinned by a drift-guard unit test against the `Cli`
  derive), takes the first non-flag token; if it's `import|export|sync` **and**
  the next token is `-h|--help`, returns that mux. So:
  - `clove import --help` → `--help` is in the provider slot → **intercept**.
  - `clove import tk --help` → `--help` is *after* the provider → **not**
    intercepted → forwarded to `clove-import-tk`'s own help (unchanged).
  - `clove --help` / `clove --clove-dir sync import --help` → handled correctly by
    the skipper.
- **`render(mux)`** rebuilds clap's help for that subcommand with a runtime
  `after_help`: `Cli::command().mut_subcommand(mux, |c| c.after_help(section))`
  then `render_help()`. clap stays the single source for usage/args; only the
  trailer is dynamic.
- **`section`** = built-in providers (json/jsonl; none for sync) + installed
  `clove-<mux>-*` plugins (filtered so only that mux's plugins are probed), each
  `--clove-plugin-info`-probed with a timeout, rendered as
  `github  clove-sync-github 0.1.0 — Two-way GitHub sync  (clove sync github)`,
  plus the existing "globals precede the provider" note.

**Lazy + self-correcting + no cache:** the probe cost is paid only on an actual
`<mux> --help`, only for that mux's plugins; a freshly-installed plugin appears on
the next run with nothing to invalidate. A per-probe timeout bounds a broken
plugin.

## 7. Search path & install root

`plugin::search_dirs()` gains `$CLOVE_HOME/bin` (default `~/.clove/bin`,
overridable via `$CLOVE_HOME`/`$CLOVE_PLUGIN_HOME`), inserted **after** the
current-exe dir and **before** `$CLOVE_PLUGIN_PATH`/`$PATH`. This is where
`cargo install --root` and future downloads land, so `resolve()` finds installs
with no user `PATH` edits, and cargo's `--root` metadata enumerates them for
`uninstall`/`update`.

## 8. Phased delivery

- **Phase 1 (offline, no install):** the `--clove-plugin-info` compat fields (§2),
  the enriched `clove plugin list` (§3), and the dynamic `--help` (§6). No network,
  no registry, no install — pure UX + discovery. Independently valuable and
  low-risk.
- **Phase 2:** the registry manifest (§1), `plugin list --all` (§4), and
  `plugin install/uninstall/update` via `cargo install --git --tag` (§5), plus the
  `$CLOVE_HOME/bin` search-path addition (§7).
- **Phase 3:** prebuilt-binary download (target-triple detection + sha256 verify)
  and the release-pipeline changes that emit per-plugin artifacts + a generated
  `plugins.json` (bundle untouched), driven by a `CLOVE_PLUGINS` variable.

## 9. Implementation surface (host-only; no new published crate)

- `crates/clove-plugin/src/run.rs` — the compat fields on `PluginInfo` + its JSON.
- `crates/clove/src/plugin.rs` — `probe_info` (spawn `--clove-plugin-info` +
  timeout), `EnrichedPlugin`/`list_enriched`, `run_as`, and the `$CLOVE_HOME/bin`
  search-dir (Phase 2).
- `crates/clove/src/mux_help.rs` (new) — `detect`/`render`/`render_provider_section`
  + the global-flag list & drift-guard test.
- `crates/clove/src/registry/{manifest,fetch,install}.rs` (new, Phase 2) — pure
  parse/lookup/status; curl/gh `--refresh` + cache; `cargo` install/uninstall.
- `crates/clove/src/cmd/plugin.rs` + `cli.rs` — the expanded `PluginAction`
  (`List{all,refresh}`, `Install`, `Uninstall`, `Update`) + rendering.
- `crates/clove/src/main.rs` — the mux-help interception at the top of `main()`.
- `registry/plugins.toml` (new committed file); `docs/PLUGIN_SYSTEM.md` §5/§6.2/§7
  + `README.md` updates.
