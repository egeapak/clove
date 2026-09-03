# crates.io as the clove plugin registry

> **Status:** Design — approved 2026-07-24. Supersedes the curated-manifest
> approach in [`docs/PLUGIN_REGISTRY.md`](../../PLUGIN_REGISTRY.md) §1/§4/§5.
> Builds on the cargo-style dispatch in [`PLUGIN_SYSTEM.md`](../../PLUGIN_SYSTEM.md).
>
> **Revised by
> [`2026-07-25-plugin-registry-implementation-plan.md`](2026-07-25-plugin-registry-implementation-plan.md).**
> That plan is the live one; where the two disagree it wins. Materially: install
> (§5, §5.1) is **deferred to its Stage 2** and is not implemented; the
> `✓ verified clove plugin` string in §5 **must not ship** (the four gates are
> shape checks, not trust checks — "matches the clove plugin convention (not
> audited)"); §5's `--yes` "skips for scripts/CI" is inverted to *non-TTY
> refuses*; and the §4.2 binary-size figure measured ~1.54 MB in practice, not
> ~1.011 MB. §7's "Phase 2 — `plugin install <name>` works" describes work that
> has not landed.
>
> **Publishing to crates.io is deferred.** Phases 1–2 are buildable and testable
> today; Phase 3 (live discovery) activates when `clove-plugin` is published, with
> no redesign. See §7.

## 1. Goal

Drop `registry/plugins.toml` — the hand-curated manifest — and use **crates.io
itself** as the registry. A plugin becomes discoverable by publishing, not by
being added to a list clove ships. No curation bottleneck, no manifest to keep in
sync with reality.

The core dispatch path stays **network-free and offline**: `clove sync github`
resolves on the filesystem exactly as today. Network access lives only behind
`plugin list --all`, `plugin search`, and `plugin install`.

## 2. Why not search

crates.io has **no prefix search**. `?q=` is fuzzy full-text over
name/description/README:

| Query | Result |
|---|---|
| `?q=clove-sync` | **0 results** |
| `?q=cargo-e` | **93 results** — includes `e_window`, `e_obs`, `startt` |

There is no `name_prefix` filter. So "search crates.io for `clove-<mux>-<source>`"
cannot be a search. It is instead a **deterministic name probe** — which is
strictly better, because the naming convention is total.

Keywords (`?keyword=…`) are exact-match and usable, but **self-asserted**: anyone
can put `clove-plugin` on any crate. Rejected as a trust signal.

## 3. The three primitives

| Concern | Mechanism | Precision |
|---|---|---|
| **Resolution** | `GET /api/v1/crates/clove-<mux>-<source>` | Exact — 200 exists, 404 absent |
| **Discovery** | `GET /api/v1/crates/clove-plugin/reverse_dependencies` | **Authoritative** |
| **Unpublished** | `git ls-remote` + blobless clone | Exact |

### 3.1 Resolution

`clove plugin install gitlab` under the `sync` mux constructs
`clove-sync-gitlab` and issues one request. No ranking, no fuzzy matching, no
false positives. Verified: `clove-sync-gitlab` → 404 with body
`{"errors":[{"detail":"crate ... does not exist"}]}`; `cargo-subcommand` → 200.

A 404 (absent) must be distinguished from a transport failure (network down) —
they produce different user-facing messages and different exit codes.

### 3.2 Discovery

`reverse_dependencies` returns every crate that genuinely depends on
`clove-plugin` — which *is* what makes something a clove plugin. It cannot be
spoofed: a squatter who publishes `clove-sync-gitlab` without depending on
`clove-plugin` never appears.

One request returns both arrays:

- `dependencies[]` — `{version_id, crate_id, req, kind, downloads, …}`
- `versions[]` — `{id, crate, num, bin_names, has_lib, description, repository, published_by, yanked, rust_version, …}`

**Implementation hazard:** `dependencies[].crate_id` is the **depended-on** crate
(always `"clove-plugin"`), *not* the dependent. Join
`dependencies[].version_id` → `versions[].id`; the dependent's identity is
`versions[].crate`. Getting this backwards yields a list of the same name
repeated. Verified against `cargo-subcommand` (11 dependents, correctly resolved
to `cargo-apk`, `cargo-xcodebuild`, `cargo-so`, …).

`per_page` caps at **100** (`per_page=200` → HTTP 400). Paginate for >100.

## 4. Transport: in-app HTTP

**Decision: in-app `ureq`, always on. No `curl` shell-out.** This reverses the
"shell out to curl" decision recorded in `PLUGIN_REGISTRY.md` §1, on measured
evidence.

### 4.1 Cargo-as-a-library was evaluated first and rejected

| Crate | Verdict |
|---|---|
| `crates-io` v0.41 (official cargo helper, 6 lean deps) | **No `reverse_dependencies` method.** Surface is `search`/`publish`/`yank`/`list_owners`. Its `search` is the unusable fuzzy one. Carries `http` but no TLS — needs a caller-supplied curl handle anyway. |
| `tame-index` | Index-only. Serves per-crate `deps`; no reverse mapping. |
| `cargo` v0.98 | Enormous tree, MSRV 1.95, unstable internal API. Disproportionate. |
| `crates_io_api` | Has reverse-deps, but pulls `reqwest` + TLS — heavier than ureq for the same capability. |

`reverse_dependencies` is a **web-API endpoint with no library binding anywhere**.
Since the request must be hand-rolled regardless, a small HTTP client is the
honest choice.

### 4.2 Measured cost

Real `clove` binary, clean clone of master, actual release profile (fat LTO,
`codegen-units=1`, `panic=abort`, `strip`), verified with a live TLS request:

| Variant | Binary | Delta |
|---|---|---|
| Baseline (master, lean) | 6.349 MB | — |
| **ureq + rustls/ring + webpki-roots** | **7.360 MB** | **+1.011 MB (+15.9%)** |
| ureq + rustls/ring + platform-verifier | 7.378 MB | +1.029 MB — *larger* |

27 transitive crates, but **16 are already in clove's tree** (`serde`,
`serde_json`, `http`, `httparse`, `base64`, `bytes`, `log`, `libc`,
`getrandom`, …). Only **11 are new**: `ureq`, `ureq-proto`, `ring`, `rustls`,
`rustls-pki-types`, `rustls-webpki`, `webpki-roots`, `untrusted`, `subtle`,
`zeroize`, `utf8-zero`. MSRV 1.85 — within the workspace's 1.94/1.95 range.

Context: `clov-WRFM0H2S` removed ~3.5 MB of octocrab/TLS. This adds back 29% of
that, with no octocrab and no tokio (ureq is blocking/sync, which suits a CLI).

### 4.3 Pin ring explicitly — do not rely on defaults

```toml
ureq   = { version = "3",    default-features = false, features = ["rustls", "json"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }
```

**This pin is load-bearing.** A default-features rustls unified onto
**`aws-lc-sys`** and tripled a probe binary (0.73 MB → 2.49 MB, 3.4×). ureq's
`rustls` feature happens to select ring today, but any dependency enabling
rustls's default features can silently flip the whole tree onto aws-lc. Pin it,
and add a CI assertion that `aws-lc-sys` is absent from the tree.

`platform-verifier` is a **false economy** — measured +18 KB over bundled roots,
because it adds `security-framework` without removing `webpki-roots`. Use
bundled roots: smaller and more portable.

**Gating: always on**, not feature-gated. One binary, one code path, uniform
behavior, and it removes the "curl not installed" failure mode. Discovery is core
plumbing, not an optional integration — the integrations are already separate
artifacts.

crates.io **requires a `User-Agent`**; anonymous requests receive a misleading
`403` for *every* crate, including ones that plainly exist. Send
`clove/<version> (+https://github.com/egeapak/clove)`.

## 5. Trust and install

**Every install prompts** — first-party and third-party alike. No allowlist, no
blessed-author shortcut: uniform behavior, nothing to maintain, and no "trusted"
path to socially engineer. `--yes` skips for scripts/CI.

```
$ clove plugin install gitlab
  clove-sync-gitlab 0.2.1          ✓ verified clove plugin
  owner:     some-user
  downloads: 41
  repo:      https://github.com/some-user/clove-sync-gitlab

  Installing builds and runs third-party code. Continue? [y/N]
```

Four gates before any install:

1. **Depends on `clove-plugin`** — via reverse-deps membership (crates.io) or the
   parsed `Cargo.toml` (git). The core "is this really a plugin" check.
2. **`bin_names` matches `clove-<mux>-<source>`** — the built binary must actually
   be resolvable by the mux.
3. **Probe `--clove-plugin-info` after build** — check the `clove_plugin_api` /
   `max_schema` range per `PLUGIN_REGISTRY.md` §2. Refuse on incompatible;
   warn on outdated.
4. **Pin to a tag/rev** — prefer explicit `--tag`/`--rev`; warn when installing
   from a moving default branch.

Install target is `cargo install --root $CLOVE_HOME` (already in the §5 search
path), giving precise bookkeeping for clean `uninstall`/`update`.

Yanked versions are skipped by default (`--allow-yanked` to override).

### 5.1 Git source

`install --git <url>` uses **plain `git`** — never `gh`, so non-GitHub forges
work. Measured: `git ls-remote` needs no clone; a blobless `--depth 1` clone
completes in **2.3s** and exposes `Cargo.toml`.

Layout is auto-detected, because clove's own repo is a workspace:

- Single crate with a `clove-plugin` dep → use it.
- **Workspace** → scan members for `clove-plugin` dependents. Exactly one → use
  it. Several → list and prompt.
- `--package <name>` skips the prompt.

```
$ clove plugin install --git https://github.com/egeapak/clove
  workspace with 3 clove plugins:
    1) clove-sync-github   sync|import|export github
    2) clove-import-tk     import tk
    3) clove-import-beads  import|export beads
  which? [1-3, or all]
```

## 6. Commands

| Command | Network | Behavior |
|---|---|---|
| `plugin list` | none | Filesystem scan (today's behavior, unchanged) |
| `plugin list --all` | cached | Installed + available from reverse-deps |
| `plugin search <text>` | cached | Local filter over the discovered set by name/description |
| `plugin install <name>` | yes | Resolve → verify → prompt → `cargo install` |
| `plugin install --git <url>` | yes (git) | Clone → parse → verify → prompt → install |
| `plugin uninstall <name>` | none | `cargo uninstall --root $CLOVE_HOME` |
| `plugin update [<name>\|--all]` | yes | Re-resolve and reinstall |

`list --all` output keeps one flat `data` array with `installed` + `status ∈
{ok, no_info, available}` as the discriminator, so JSONL stays clean
(`PLUGIN_REGISTRY.md` §4). Human output shows *Installed* and *Available*
sections.

**Degradation is mandatory.** A discovery failure — offline, 5xx, unpublished
`clove-plugin` — never breaks the command: the Installed section still prints and
the error surfaces as a warning in `_meta.registry_error`. Dispatch is never
affected.

**Caching:** the reverse-deps result is written under `$CLOVE_HOME` with a
timestamp, reused for ~24h, and `--refresh` forces a fetch. Keeps `list --all`
fast and usable offline after first run.

## 7. Phasing (publishing deferred)

Publishing to crates.io is deferred, so the work splits at a clean boundary.
**Verified:** name-probe and the git path work against live crates.io *today*;
only reverse-deps discovery depends on publishing.

**Phase 1 — foundations (buildable now).**
`registry/crates_io.rs` behind a `Fetch` trait, `registry/cache.rs` over an
injected clock, `registry/git_source.rs`. Add ureq + pinned ring. Name-probe
resolution works live. `install --git` works end-to-end against real repos —
including clove's own workspace, which is the multi-member test case. Full unit
coverage from recorded JSON fixtures; no network in tests.

**Phase 2 — install/uninstall/update (buildable now).**
The four verification gates, the prompt, `$CLOVE_HOME` root, the `--clove-plugin-info`
probe. `plugin install <name>` works for any crate already on crates.io.
`list --all` and `search` ship, degrading cleanly to "registry unavailable" —
which is the honest state pre-publish.

**Phase 3 — live discovery (activates on publish).**
No code change beyond removing the "unavailable" special-case. When
`clove-plugin` is published, reverse-deps starts returning rows and discovery
lights up. Until third parties publish, it correctly returns only first-party
plugins.

This ordering means deferring the publish costs nothing structurally: everything
except the final data source is built and tested first.

## 8. Testing

- **No network in tests.** `Fetch` is a trait; unit tests inject recorded
  crates.io JSON fixtures (including the `dependencies`/`versions` join, the
  404 body, and a 403-without-User-Agent response).
- **Join correctness** gets a dedicated test — the `crate_id` hazard in §3.2 is
  the most likely bug in this design.
- **Cache** tests inject a clock: fresh hit, TTL expiry, `--refresh` override,
  corrupt-cache recovery.
- **Git source** tests run against fixture repos on disk (`file://`), covering
  single-crate, workspace-with-one-plugin, workspace-with-several, and
  no-plugin-found.
- **Degradation** tests assert `list --all` still prints Installed and sets
  `_meta.registry_error` when discovery fails.
- **CI assertion** that `aws-lc-sys` is absent from the dependency tree (§4.3).
- One **`#[ignore]`d live test** hitting real crates.io, run manually.

## 9. Known constraints

- **`clove-plugin` must be published** for discovery; deferred per §7. Today
  `reverse_dependencies` returns `crate does not exist`.
- **Discovery returns only first-party plugins** until third parties publish.
- `per_page` caps at 100; paginate beyond that.
- crates.io requires a User-Agent; anonymous requests 403 on everything.
  *(Side finding: the `RELEASE.md` §2 name-check snippet omits `-A` and will
  report false "TAKEN" for all 14 crates. Separate one-line fix.)*
- `dependencies[].crate_id` is the depended-on crate, not the dependent (§3.2).
- ureq is blocking/sync — correct for a CLI, but discovery must not be called
  from any async context.

## 10. Changes to `docs/PLUGIN_REGISTRY.md`

On implementation: §1 (manifest) is **removed**; §4/§5 are **rewritten** around
crates.io; §2 (compat probe), §3 (enriched list), §6 (dynamic help), §7 (search
path) stand unchanged. The "shell out to curl" decision in the adopted-decisions
list is superseded by §4 here, with the measurements as rationale.
