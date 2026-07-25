# Plugin registry — implementation plan (Stages 1–3)

> **Revision 2 (2026-07-25)** — revised after a four-lens adversarial review
> (Rust/API correctness, codebase fit, security, completeness). 40 findings; the
> material ones are folded in below and summarized in §8. Two changes are
> load-bearing enough to call out up front:
>
> 1. **The install root is no longer `~/.clove`** (§4A). `clove-core`'s
>    `find_repo_root` accepts *any* ancestor containing a `.clove/` directory,
>    with no marker check, so creating `~/.clove/bin` makes `$HOME` itself
>    resolve as a clove repository. **Reproduced:** with `~/.clove/bin` present,
>    `clove ls` from a deep subdirectory of `$HOME` stops reporting "no clove
>    repository found" and instead targets `$HOME/.clove/issues`, and `clove new`
>    tries to write items there. The root moves to an XDG-style path.
> 2. **Work items E (git source) and F (install/uninstall/update) are deferred**
>    out of this stage — see §7 for the rationale. What ships is discovery:
>    resolution, `list --all`, and `search`.
>
> **Status:** Implementation plan. Executes
> [`2026-07-24-crates-io-plugin-registry-design.md`](2026-07-24-crates-io-plugin-registry-design.md)
> (crates.io as the registry) and the surviving parts of
> [`docs/PLUGIN_REGISTRY.md`](../../PLUGIN_REGISTRY.md) (§7 search path, §5
> install semantics). Phase 1 of `PLUGIN_REGISTRY.md` (§2 compat probe, §3
> enriched list, §6 dynamic help) already shipped in `cdea402` — this plan covers
> everything that remains.

## 0. Live re-verification (2026-07-25)

Every empirical claim in the design was re-checked against the live API before
planning. All hold:

| Claim | Verified |
|---|---|
| `GET /crates/cargo-subcommand` | **200** |
| `GET /crates/clove-sync-gitlab` | **404**, body `{"errors":[{"detail":"crate ... does not exist"}]}` |
| Anonymous request (no `User-Agent`) | **403** — confirms the `RELEASE.md` §2 bug |
| `GET /crates/clove-plugin` | **404** — still unpublished, discovery deferred as designed |
| `reverse_dependencies` join hazard | **Confirmed** — `dependencies[].crate_id` is `"cargo-subcommand"` for all 11 rows; `versions[].crate` yields the real dependents (`cargo-apk`, `cargo-xcodebuild`, `cargo-so`, …) |
| `per_page=200` | **HTTP 400** — cap is 100 |
| `ureq` feature resolution | `rustls,json` selects `_ring` + `rustls-webpki-roots`, **not** aws-lc — the design's pin matches the default, and the pin still guards against unification |

### 0.1 Three findings the design does not cover

1. **`/crates/{name}` already returns `bin_names`.** The per-version object
   carries `bin_names`, `yanked`, `published_by`, `repository`, `description`,
   `rust_version`, `has_lib`. **Consequence: gate 2 (`bin_names` matches the
   dispatch convention) is satisfiable from the name probe alone** — it does not
   need reverse-deps, so it works today, pre-publish. The crate object also
   carries `default_version` / `newest_version` / `max_stable_version` for
   version selection.
2. **Reverse-deps rows are *versions*, not crates.** One dependent crate can
   appear several times (one row per version that depends on `clove-plugin`).
   **Dedup by `versions[].crate`, keeping the highest semver**, or `list --all`
   will show duplicates. Not mentioned in the design; the sample happened to be
   1:1, which is exactly how this bug ships unnoticed.
3. **`dependencies[].kind` must be filtered to `"normal"`.** A `dev`- or
   `build`-dependency on `clove-plugin` does not make a crate a plugin. Gate 1 is
   "depends on `clove-plugin`" in the runtime sense; an unfiltered join lets any
   crate that merely *tests* against `clove-plugin` appear as an installable
   plugin.

### 0.2 Two gaps in the design, resolved here

- **Bare-name resolution is undefined.** §3.1 says `plugin install gitlab`
  "under the `sync` mux constructs `clove-sync-gitlab`", but `plugin install` has
  no mux context. It needs an explicit candidate ladder — deferred to Stage 2
  with the ordering constraint recorded in §5.
- **Gate 1 is unevaluable pre-publish.** Gate 1 is defined via reverse-deps
  membership, but `clove-plugin` is unpublished, so *every* crates.io install
  would fail it. This is one of the reasons install is deferred (§7); the
  three-valued return in §3.2 is what makes the distinction expressible when it
  does land.

## 1. Scope

Renamed to **stages** because "Phase 3" already means two different things
(design §7 = live discovery; `PLUGIN_REGISTRY.md` §8 = prebuilt-binary
download). Both are addressed; neither name is reused.

### Stage 1 — discovery (this plan, implemented now)

| # | Work item |
|---|---|
| A | Install root + `<clove-home>/bin` on the search path |
| B | `ureq` transport + `Fetch` seam + crates.io client |
| C | TTL cache |
| D | `plugin list --all` + `plugin search` + degradation |
| G | Docs + `RELEASE.md` `-A` fix |
| H | CI: `aws-lc-sys` absence assertion |

### Stage 2 — install (deferred; §7)

Work items E (git source) and F (install/uninstall/update). The full
requirement set, including everything the security review surfaced, is recorded
in §5 so nothing is lost.

### Stage 3 — live discovery (no code)

When `clove-plugin` is published, reverse-deps starts returning rows. There is
no "unavailable special-case" to remove, provided D distinguishes *absent
registry* from *empty registry* — see §3.2.

**`PLUGIN_REGISTRY.md` §8's own Phase 3** (prebuilt download, target-triple
detection, sha256 verify, generated `plugins.json`, `CLOVE_PLUGINS`) is
**superseded, not scheduled**: `cargo install` replaces it, and `release.yml`
already bundles all three plugin binaries into the release tarball. §4G marks it
so in the doc rather than leaving a promise nobody will build.

## 2. Module layout

```
crates/clove/src/
  clove_home.rs            (new)  install-root resolution — shared by plugin.rs + registry
  plugin.rs                (edit) search_dirs() gains <clove-home>/bin
  registry/
    mod.rs                 (new)  RegistryPlugin, Fetch, FetchError, name validation
    http.rs                (new)  UreqFetch — the ONLY file that names ureq
    crates_io.rs           (new)  exists() probe + reverse_dependents() join
    cache.rs               (new)  TTL cache over an injected clock
  cmd/plugin.rs            (edit) list --all / search
  cli.rs                   (edit) PluginAction + its doc comments
  main.rs                  (edit) route the plugin action  ← was missing in rev 1
```

**Containment rules.**
1. `ureq` is named in `http.rs` and nowhere else; every other module depends on
   the `Fetch` trait, so all tests are offline by construction.
2. **`registry/` is CLI-only.** It must never be called from `cloved` or
   `clove-web`: ureq is blocking/sync and both of those are tokio/axum
   (design §9). Stated as a rule so it is not re-derived later by accident.

`main.rs` was absent from revision 1's file list. It matters: the current
routing arm is `Commands::Plugin(_) => cmd::plugin::run(f)` — the action is
**discarded**, so adding subcommands would compile cleanly and silently do
nothing.

## 3. Types and contracts

### 3.1 The `Fetch` seam

```rust
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("HTTP {code}")]
    Status { code: u16, retry_after: Option<Duration>, body: Option<String> },
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub trait Fetch {
    /// `Ok(None)` is an authoritative 404; `Err` is everything else.
    fn get(&self, url: &str) -> Result<Option<String>, FetchError>;
}
```

A single opaque `message` was insufficient: it needs `Debug` for tests to
compile, `Display`/`Error` to be a `#[source]`, and — the real reason —
crates.io **rate-limits with 429 + `Retry-After`**. The name ladder issues up to
four sequential probes and reverse-deps paginates, so 429 is a live path that
must back off rather than surface as "network unavailable".

ureq 3 returns `Err(StatusCode(404))` and **discards the body** unless
`http_status_as_error(false)` is configured. `http.rs` must set that, or the
404-vs-transport distinction the trait exists to preserve is lost at the very
bottom of the stack.

### 3.2 Absent registry ≠ empty registry

```rust
/// `Ok(None)` = `clove-plugin` itself is not published (registry absent).
/// `Ok(Some(vec![]))` = published, but nothing depends on it yet.
pub fn reverse_dependents(f: &dyn Fetch, of: &str)
    -> Result<Option<Vec<RegistryPlugin>>, FetchError>;
```

Revision 1 collapsed these, which made §1's Stage-3 requirement ("treat an
absent registry as a normal empty result") contradict the install gate's
requirement ("distinguish *not a dependent* from *cannot tell*"). The
three-valued return satisfies both, and post-publish it stops a transient empty
response from silently downgrading a genuine reject.

### 3.3 `RegistryPlugin`

```rust
pub struct RegistryPlugin {
    pub crate_name: String,
    pub latest: Option<semver::Version>,        // highest non-yanked
    pub latest_yanked: Option<semver::Version>, // highest overall, if all yanked
    pub description: Option<String>,
    pub repository: Option<String>,
    pub bin_names: Vec<String>,
    pub published_by: Option<String>,
    pub downloads: u64,
}
```

`version: String` + `yanked: bool` was unrepresentable: if `version` is
non-yanked by construction then `yanked` is dead, and a crate whose versions are
*all* yanked (routine after a bad release) has no value to put there.

**`semver` is a new dependency.** It is not in any workspace `Cargo.toml` and is
not reachable from clove-cli's normal tree. Plain string comparison is wrong in
the obvious way — `"0.10.0" < "0.9.0"` lexically, and the recorded hazard
fixture contains exactly that pair — so the comparison mechanism has to be
named, not left to the implementer. Unparseable `num` sorts lowest, never panics.

### 3.4 Input validation (one central validator)

Every one of these is interpolated into a URL or a subprocess argv, so they are
validated in `registry/mod.rs` — mirroring how `plugin.rs::is_valid_segment`
guards *dispatch* centrally for every caller:

- **crate/plugin names**: crates.io's own rule, `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`,
  and percent-encoded into the URL. Without it, a name containing `/` or `..`
  path-traverses after URL normalization onto an unrelated endpoint, which the
  ladder reads as "resolved".
- **anything reaching a subprocess argv must not begin with `-`.** This is the
  sharpest one and it belongs in Stage 1 even though the subprocess calls are in
  Stage 2, because the validator is shared: `git clone --template=<dir>` runs
  hooks from a local directory, `--upload-pack=<cmd>` executes a command, and
  `cargo --config source.crates-io.replace-with=…` redirects the whole download
  to another registry.

### 3.5 Error modeling

Add one variant to `clove_types::CloveError`:

```rust
Registry { message: String }   // → ("REGISTRY_ERROR", 5)
```

Verified non-breaking: the enum is `#[non_exhaustive]`, so every out-of-crate
match already has a wildcard (`clove-web/src/error.rs:54`,
`cloved/src/ipc.rs:474`); the only exhaustive match is `error_code` in the
defining crate. `CloveError` does **not** ride the tarpc wire (it is not
`Serialize` — `Io` carries `std::io::Error`; `cloved` converts to `RpcError`),
so there is no IPC protocol-version impact. Exit 5 is within
`docs/json-schema/v1/error.json`'s `maximum: 7`.

## 4. Work items (Stage 1)

### A. Install root + search path

**The root is not `~/.clove`.** `clove-core`'s `find_repo_root`
(`crates/clove-core/src/repo.rs:43`) accepts any ancestor containing a `.clove/`
*directory* — no marker file, no content check — so `~/.clove/bin` makes `$HOME`
resolve as a repository for every command run beneath it. Reproduced: the error
goes from `no clove repository found … (run clove init)` to clove targeting
`$HOME/.clove/issues`.

```
$CLOVE_HOME                          explicit override, wins
else  $XDG_DATA_HOME/clove           if set
else  ~/.local/share/clove           Unix default
else  %APPDATA%\clove                Windows default
```

Binaries land in `<root>/bin`. Fixing `find_repo_root` to require a marker was
considered and rejected for this stage: it changes discovery semantics for every
existing repository, which is a much larger blast radius than choosing a
different directory.

**Search-path order** — `<root>/bin` goes **after** `$CLOVE_PLUGIN_PATH`, not
before it:

```
current-exe dir → $CLOVE_PLUGIN_PATH → <clove-home>/bin → $PATH
```

Revision 1 put it before. `$CLOVE_PLUGIN_PATH` is the user's *explicit* opt-in
directory (`PLUGIN_SYSTEM.md` §5); a binary pulled from the internet must not
outrank a deliberate local override.

`clove_home.rs` owns resolution. `cmd/setup.rs::home_dir()` moves here, but its
error must be re-classed: it hardcodes `field: "--global"` → `VALIDATION_ERROR`
exit 4, so a registry call on a machine with no `$HOME` would report *"invalid
--global"* and exit 4, contradicting §3.5. `setup.rs` re-wraps with its own
`--global` context.

**Test hermeticity** — revision 1's justification was wrong and is corrected
here. Existing plugin tests do *not* break: `search_dirs()` already walks the
current-exe dir and `$PATH`, and the tests are consequently membership-based
(`.find(|p| p["name"] == "echo")`, `contains`), not set-equality. The real
reason to pin the root in tests is narrower and still mandatory:
`assert_cmd` inherits `$HOME`, so an unpinned test would read — and later write
— a developer's real install root.

### B. Transport + crates.io client

Dependencies go in `[workspace.dependencies]` with a rationale comment, then
`.workspace = true` in the leaf crate — the convention every other dep in
`crates/clove/Cargo.toml` follows. **`rustls` is already pinned** at
`Cargo.toml:87` with `features = ["ring"]`; declaring a second, differently
featured `rustls` in the leaf crate is precisely the feature-unification hazard
the pin exists to prevent. Amend the existing entry instead.

Three consequences revision 1 missed:

- **`deny.toml` must gain `CDLA-Permissive-2.0`.** Verified against live
  crates.io: `webpki-roots` is CDLA-Permissive-2.0, which is not in the
  allow-list, so the `cargo-deny` CI job red-lights on the first commit of this
  item. Every other new crate (`ureq`, `ureq-proto`, `rustls-pki-types`,
  `zeroize` → MIT/Apache; `untrusted` → ISC; `subtle` → BSD-3-Clause; `ring` →
  Apache-2.0 AND ISC) is already covered.
- **`crates/clove/Cargo.toml:18-21` claims the host carries "zero network
  dependencies"** — that comment becomes false and must be updated with it.
- **CI runs `cargo check -p clove-cli --no-default-features`** for "the leanest
  cross builds". That arm now links TLS; the comment stating the intent must be
  amended.

**TLS root store.** Bundled `webpki-roots` ignores `SSL_CERT_FILE`/`SSL_CERT_DIR`
and the platform store, so in any environment with a TLS-intercepting egress
proxy — *including the one this plan was written in* — discovery fails with an
opaque TLS error while `cargo`, `git`, and `curl` all work. Mitigation: keep
bundled roots (the design's measured choice) **and** additionally load
`SSL_CERT_FILE`/`SSL_CERT_DIR`/`CARGO_HTTP_CAINFO` when set, honor
`HTTPS_PROXY`/`NO_PROXY`, and make the TLS failure message name the CA/proxy
cause rather than letting §4D's degradation swallow it as an ordinary outage.

### C. Cache

`<clove-home>/registry-cache.json`, `{ schema, fetched_at, plugins }`, TTL 24h,
`--refresh` forces, corrupt cache is ignored and refetched. Written
**temp+rename** so a torn write cannot happen (three lines; the self-healing
path handles it either way, but preventing beats recovering). The clock is a
parameter, not `Utc::now()` inline.

**The cache is for `list --all`/`search` only.** When Stage 2 lands, install
gates must fetch evidence live — otherwise anyone who can write that file, or
set `$CLOVE_HOME`, decides what counts as a verified plugin.

### D. `list --all` + `search`

One flat `data` array; human output prints *Installed* / *Available* sections.

**`status` stays the compat verdict.** Revision 1 added `available` to the same
field that already carries `ok`/`outdated`/`no_info`/`needs_newer_clove`, where
`outdated` means "host > plugin's `max_clove_plugin_api`" — an API verdict.
Overloading it leaves no way to say "installed, but crates.io has a newer
release". Registry freshness is orthogonal: `installed: bool` plus
`latest_version: Option<String>` and `update_available: bool`.

Two carrier bugs to fix while here:

- **`print_jsonl_items` takes no `meta` argument**, so `_meta.registry_error`
  vanishes in `--format jsonl` and a discovery failure becomes
  indistinguishable from "no available plugins". Add a meta-carrying variant, or
  emit the warning to stderr for that format.
- **`render_human` returns early when the installed set is empty**, so
  `--all` on a clean machine would print nothing at all — no header, no
  Available section.

`search <text>` filters locally (substring, case-insensitive); crates.io's `?q=`
is the unusable fuzzy one.

### G. Docs

Beyond revision 1's list (`PLUGIN_REGISTRY.md` §1/§4/§5, design-doc status,
`RELEASE.md:64`):

- **Land the design doc on this branch.** It lives only on
  `origin/design/crates-io-plugin-registry`, so every `design §N` citation here
  is unresolvable for anyone reading this branch.
- `docs/PLUGIN_SYSTEM.md` §5 — the normative search-path list, cited by number
  from `plugin.rs:71`.
- `CLAUDE.md:17` — repeats the search path inline.
- `docs/DESIGN.md` §7.6 — the authoritative exit table gains `REGISTRY_ERROR`.
- `crates/clove/src/cmd/plugin.rs` module doc — "a read-only view".
- `cli.rs:148,159` doc comments — they render in `clove --help`.
- `crates/clove/Cargo.toml:18-21` — the "zero network dependencies" claim.
- **`cmd/agent_doc.rs`** — hand-maintained, has **no `clove plugin` entry at
  all**, and its exit-code table describes 5 as "i/o or missing `.clove/`".
  `--check` only validates a schema marker, so this rots silently.
- `README.md` command table + the docs-table row describing `PLUGIN_REGISTRY.md`
  as "the registry manifest schema", which stops being true.
- `CHANGELOG.md` — `## [Unreleased]` exists and is empty.

### H. CI

Revision 1's command **fails today**, and for a different reason than it
claimed: the reachable path is clove-cli's own `jsonschema` **dev-dependency**
→ reqwest → rustls(default) → `aws-lc-sys`, not octocrab in `clove-sync-github`.
`-p` does not scope `-i`; only an edge filter does.

```sh
test -z "$(cargo tree -p clove-cli -e normal -i aws-lc-sys 2>/dev/null)"
```

Exit code cannot be the signal: absent-from-lockfile exits 101, present-but-
filtered-out exits 0 with "nothing to print" on stderr. `aws-lc-sys` stays in
`Cargo.lock` via `clove-sync-github`, so the check asserts **empty stdout**.

## 5. Stage 2 requirements (deferred, recorded so they survive)

The security review found the install path materially under-specified. Recording
the full set here is the point of deferring rather than dropping it.

**Trust model — the gates are shape checks, not trust checks.** All four are
attacker-forgeable at zero cost: depending on `clove-plugin` is one Cargo.toml
line, `bin_names` is just naming your `[[bin]]`, and the `--clove-plugin-info`
probe is a JSON string to print. Worse, that probe — which revision 1 called
"the strongest of the four" — runs **after** `cargo install` has already
executed the crate's `build.rs` and proc macros as the user. The design's
`✓ verified clove plugin` string must not ship; it reads as "clove vetted this".
Correct wording is "matches the clove plugin convention (not audited)".

**Sequencing.** `plugin install` must not ship before the first-party crate
names are published/reserved. Today `clove-sync-github` is unregistered, so
`clove plugin install github` — the flagship command — resolves to whoever
claims it first. The broken `RELEASE.md` name-check (403 → every name reports
TAKEN) means the team's own pre-flight cannot currently tell a squat from noise,
which is why §4G's one-character fix is Stage 1 work.

Also required, none of it in revision 1:

- **`--bin <name>`** on `cargo install`. Without it a crate declaring extra
  `[[bin]]`s drops all of them into the install root; one consented install of
  an unrelated plugin can plant `clove-sync-github`, which then receives the
  full inherited environment — `GITHUB_TOKEN` included — on the next
  `clove sync github`. `plugin list` dedups first-match-wins, so the shadowed
  legitimate binary would not even appear.
- **Rollback.** Gate 3 runs post-install; refusing without `cargo uninstall`
  leaves the rejected binary on the search path.
- **Non-TTY refuses** (exit 4, "re-run with `--yes`"). `PLUGIN_REGISTRY.md` §5's
  "non-TTY/JSON proceeds (scriptable)" must be explicitly killed, not left
  standing — it is exactly the unattended-code-execution case. Prompt only when
  the format is human and stdin+stderr are TTYs, read from `/dev/tty`, and treat
  EOF as No.
- **Provenance from `.crates2.json`.** Verified real: a `cargo install --root R`
  writes `R/.crates2.json` with `installs["<pkgid>"].bins`, where the pkgid
  carries `registry+…` vs `git+<url>?tag=…#<sha>` vs `path+…`. This is the only
  correct basis for `uninstall` (which the design marks **network: none**, and
  which cannot use the name ladder because that is four HTTP probes) and for
  `update` (which must know whether to re-clone or re-resolve).
- **`cargo uninstall` takes the *package* name, not the bin name** — verified;
  in this repo `clove-plugin-echo` produces bin `clove-echo`. Map bin → pkgid
  via `.crates2.json`.
- **`--version =X.Y.Z`** must be passed, or all the version selection in §3.3 is
  decorative — cargo re-resolves and picks its own. Relatedly, `--tag`/`--rev`
  are git-only, so gate 4 does not apply to registry installs, and
  `--allow-yanked` has no cargo-side expression at all.
- **`update` must prompt** with an old→new diff. Silently `--force`-jumping a
  third-party crate to any newer version — possibly from a compromised account —
  contradicts "every install prompts", and `--force` also defeats cargo's own
  bin-collision guard.
- **Git hardening**: timeouts on `ls-remote`/`clone` (the probe path is bounded
  at 500 ms; these are unbounded), `GIT_TERMINAL_PROMPT=0` so a 401 cannot
  surface a credential prompt the user attributes to clove, and confining
  member-glob expansion to the clone root.
- **Migration**: every current instruction says `cargo install clove-sync-github`,
  which lands in `~/.cargo/bin`. `uninstall`/`update` must detect a plugin
  resolved from outside the install root and say so rather than failing opaquely.
- **The workspace scan over-matches.** Verified on this repo: filtering members
  by "has a `clove-plugin` dependency key" yields **five** hits — including
  `crates/clove` itself (the host, package `clove-cli`, bin `clove`) and the
  `publish = false` echo fixture — against the three the design's example shows.
  Filter on the produced bin name's shape, not the dep key alone.
- **Windows**: crates.io `bin_names` are unsuffixed; the resolver looks for
  `.exe`. Gate 2 compares suffix-insensitively.
- **Ladder ordering** contradicts dispatch: §3.2 tries `sync` first
  unconditionally, but `mux_candidates` prefers the *dedicated* mux, so
  `plugin install beads` and `clove import beads` would disagree about which
  binary is authoritative. Also, the ladder's 4th rung (`clove-<name>`, the
  generic-plugin case `run_as` supports) can never satisfy a gate 2 phrased as
  `clove-<mux>-<source>`.
- **Multi-binary bundles** (one friendly name → several binaries,
  `PLUGIN_REGISTRY.md` §1/§5) become unrepresentable under "first 200 wins".
  Acceptable — the umbrella-fallback model made one-binary-many-capabilities the
  real pattern — but say so rather than letting it lapse silently.

## 6. Test plan (Stage 1)

- **No network in any non-`#[ignore]`d test.** `Fetch` is a trait; fixtures are
  recorded from the live responses in §0 and committed under
  `crates/clove/tests/fixtures/registry/`.
- Dedicated tests for each §0.1 hazard — join direction, `kind` filter, version
  dedup — plus the **403-without-User-Agent** fixture, which regression-guards
  the single most surprising crates.io behavior.
- Semver ordering specifically: the hazard fixture contains `0.2.0` / `0.10.0`
  so a string comparison fails the test.
- Absent-vs-empty registry (§3.2), both branches.
- Cache: fresh hit, TTL expiry, `--refresh`, corrupt-cache recovery.
- Degradation: `list --all` still prints Installed and reports the error, in
  **all three** output formats.
- Name validation: `..`, `/`, a leading `-`, and over-length input.
- Hermeticity: every plugin test pins the install root to a temp dir.
- One `#[ignore]`d live test against real crates.io.

**Quality gate** (CLAUDE.md), all clean before commit:
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --workspace`, plus `cargo deny check` for item B. No `clove-web`
change, so the npm checks are not required.

## 7. Why Stage 2 is deferred

Not scope-trimming for its own sake — three independent reasons converge:

1. **It is sequencing-blocked.** `plugin install` against crates.io should not
   ship before the first-party names are published, and publishing is an owner
   action outside this change.
2. **It is where all the code-execution risk lives.** Stage 1 makes HTTP GETs
   and prints results. Stage 2 builds and runs third-party code. Of the four
   reviews' findings, nearly every high-severity one is in Stage 2, and they are
   not independent — argv validation, the TTY rule, `--bin`, rollback, and
   provenance are one coherent design that wants to land together.
3. **Stage 1 is independently useful and independently verifiable.** Resolution,
   `list --all`, and `search` work today against live crates.io; nothing about
   them is provisional or waiting on a publish.

Shipping a half-hardened installer would be worse than shipping none.

## 8. Findings ledger

Four reviews, 40 findings. Folded in above: the install-root hijack (§4A), the
search-path inversion (§4A), the CI assertion (§4H), `deny.toml` (§4B),
`main.rs` routing (§2), workspace dep convention (§4B), structured `FetchError`
+ 429 (§3.1), absent-vs-empty (§3.2), semver (§3.3), the yanked-representation
hole (§3.3), central input validation (§3.4), `status` overloading (§4D), the
jsonl `_meta` hole and `render_human` early-return (§4D), the `home_dir()` error
class (§4A), the corrected hermeticity rationale (§4A), TLS roots and proxies
(§4B), atomic cache write (§4C), the CLI-only rule (§2), the Phase-3 name
collision (§1), and the full docs list (§4G).

Recorded against Stage 2 (§5): the trust-model relabel, publish sequencing,
`--bin`, rollback, the non-TTY rule, `.crates2.json` provenance, `cargo
uninstall` naming, `--version`, `update` prompting, git hardening, migration,
the workspace over-match, Windows suffixes, ladder ordering, and bundles.

Deliberately not adopted: `clove doctor` plugin checks (convenience, not
correctness — `plugin list` already surfaces compat status) and a `$CLOVE_HOME`
lock for concurrent installs (cargo takes its own package-cache lock; the cache
is self-healing).
