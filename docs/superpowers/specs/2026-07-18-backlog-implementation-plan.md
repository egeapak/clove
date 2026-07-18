# Backlog implementation plan — 2026-07-18

Status of the dogfood backlog (GitHub `egeapak/clove` issues `gh-18`…`gh-31`)
after the e2e dogfooding pass. This plan sequences the work and records the
scope decisions taken with the owner.

## Scope decisions (owner-approved)

1. **Large undesigned items** (`gh-19` GitLab bridge, `gh-25` Jira bridge,
   `gh-29` richer history) → **design specs only, defer the build.** Each gets a
   design doc under `docs/superpowers/specs/`; no implementation this pass.
2. **Publish target** (`gh-30` / `gh-22`) → **decide later.** Do the `gh-30`
   crate rename to `clove-cli` now (safe either way); wire **no** publish path.
   `gh-22` (cut & publish v0.1.0) stays entirely with the owner.
3. **Delivery** → **one PR per item** on `egeapak/clove`.

## Execution loop (every item)

`branch off master` → implement + tests → quality gate → commit → push → open PR
→ mark the clove item closed (spec items: comment) → `clove sync github` so the
tracker mirrors GitHub.

**Quality gate:** `cargo fmt --check`, `cargo clippy --all-targets -D warnings`
(scoped to touched crates; default **and** `--no-default-features` where feature
flags change), `cargo test` (touched crate + `--workspace` for cross-cutting
changes), and `cd crates/clove-web/web && npm run check && npm run test` for web
changes. Commits use `--no-gpg-sign` and the repo's `Co-Authored-By` trailer.

## Build + test — one PR each (in order)

### 1. `gh-26` — comment author fallback chain  ·  chore/dx
`author()` in `crates/clove/src/cmd/comments.rs` resolves only
`CLOVE_AUTHOR → GIT_AUTHOR_EMAIL → "unknown"`. Extend to
`CLOVE_AUTHOR → GIT_AUTHOR_EMAIL → git config user.email → git config user.name
→ $USER → "unknown"`. Read git config via `git2` (already a dep) or
`std::process::Command`, best-effort, no hard failure.
**Accept:** with `CLOVE_AUTHOR` unset and a git identity configured, the comment
filename carries that identity; unit test for the resolution order.

### 2. `gh-20` — broaden JSON-schema validation  ·  chore/tests
Add `docs/json-schema/v1/{version,reindex,new}.json` and round-trip validation
tests mirroring the existing five schema families (`tests/*.rs`).
**Accept:** each command's real `--format json` output validates against its
schema in CI.

### 3. `gh-24` — tighten the `ls` perf gate  ·  chore/ci
Lower the `ls` acceptance-gate threshold (~15 ms → ~8 ms) in the bench/gate so a
covering-scan regression is caught. Confirm current headroom locally first.
**Accept:** gate passes at the new bound with margin; the number is documented.

### 4. `gh-18` — make the `github` feature opt-out  ·  chore/build
`crates/clove/Cargo.toml`: `default = ["mcp"]` (drop `github`); add
`full = ["github", "mcp"]`. `crates/clove-import/Cargo.toml`: drop `github` from
`default`. Update CI to build the distribution binary with the intended feature
set and run sync tests under `--features github`; note the toggle in the README.
**Accept:** default `cargo build` links no octocrab/tokio (measure the size
delta); `--features github` restores `clove sync github`; clippy clean in both
default and `--no-default-features`.

### 5. `gh-27` — stats-history web endpoint  ·  feature/web
`GET /api/v1/stats/history` axum handler in `clove-web` reading
`Index::snapshot_history` (JSON envelope, `?since` / `?limit`), plus a SvelteKit
route rendering the series (reuse the timeline throughput chart component).
**Accept:** Rust integration test for the endpoint + a vitest for the view.

### 6. `gh-31` — concurrent TUI model  ·  feature/perf
Move the wholesale re-scan onto a background worker; guard the `Data` /
`Listing` / `DetailPane` sub-structs with locks; drive the existing 10 fps
`is_busy()` cadence with a spinner. Keep canonical order/parity.
**Accept:** insta snapshots stable (or intentionally regenerated); a unit test
for the worker handshake; no main-loop blocking during rescan.

### 7. `gh-21` — MCP push notifications  ·  feature/mcp
Add a subscribe/event RPC to the `cloved` tarpc service that streams
graph-change events; `clove-mcp` emits `notifications/tools/list_changed` on each
(bump `PROTOCOL_VERSION` if the wire changes).
**Accept:** daemon IPC test for the event; MCP e2e asserting a notification
fires after a mutation.

### 8. `gh-30` — rename crate to `clove-cli`  ·  chore/release
`crates/clove/Cargo.toml`: `[package].name = "clove-cli"`, keep
`[[bin]].name = "clove"`. No publish metadata/wiring. All other workspace crate
names are unchanged (verified free on crates.io).
**Accept:** `cargo build` still produces the `clove` binary; `cargo test
--workspace` green.

## Design specs only — one doc PR each (defer build)

### 9. `gh-19` — GitLab bridge design spec
Covers: a `VendorSync` trait boundary that the existing
`clove_import::sync`/`sync_net` reconciliation can drive, the GitLab REST field
mapping (issue ↔ clove), auth/token resolution, and how the pure reconciliation
core is reused. Explicitly designed so Jira reuses the same trait.

### 10. `gh-25` — Jira bridge design spec
Reuses the `gh-19` trait boundary; Jira REST/JQL field mapping, auth, and
reconciliation differences (status categories, custom fields).

### 11. `gh-29` — richer history / changelog design spec
Compares git-log-derived history (no new state) vs. an append-only events log;
recommends an approach and sketches the CLI/web/TUI surface.

## Left to the owner
- `gh-22` — cut & publish v0.1.0 (needs the public/private decision + the owner's
  release action).
- Epics `gh-28` (release prep) and `gh-23` (remaining M4) stay open until their
  deferred children ship; each gets a status comment.
