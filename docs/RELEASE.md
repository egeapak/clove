# Release runbook — clove v0.1.0

The owner-only steps to cut a clove release: publish the workspace to crates.io
**and** ship pre-built binaries via a GitHub Release + Homebrew tap. Tracks
issue **#22**.

Publishing is a **one-way, mostly-irreversible** action (a version can be
*yanked* but never re-uploaded, and a name, once taken, is yours forever). Read
the whole runbook once before running any `cargo publish`.

> **Naming context.** The crates.io name `clove` is taken by an unrelated ML
> framework, so the CLI crate ships as **`clove-cli`** (the installed command is
> still `clove` — `[[bin]] name = "clove"`). The ten core crate names were
> verified **free** on crates.io on 2026-07-18; the four crates the plugin system
> added (`clove-plugin` + the three `clove-{sync-github,import-tk,import-beads}`
> plugins) must be re-verified too. Re-verify all fourteen at publish time (step 2).

---

## 0. Prerequisites (one-time)

- A crates.io account (login via GitHub at <https://crates.io>) that has
  **verified an email address** — crates.io rejects publishes otherwise.
- An API token from <https://crates.io/settings/tokens> with the
  **`publish-new`** and **`publish-update`** scopes.
  ```sh
  cargo login          # paste the token when prompted (stored in ~/.cargo/credentials.toml)
  ```
- A clean checkout of `master` at the commit you intend to release, with the
  full toolchain (`cargo`, and **`npm`** — see the web-UI note in step 4).
- `gh` authenticated with `repo` scope (for the GitHub Release / tag push).

---

## 1. Pre-flight — the release must be green

Run the full quality gate on the exact commit you will tag. **Do not publish a
red tree.**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
( cd crates/clove-web/web && npm ci && npm run check && npm run test )
```

Confirm the working tree is clean except the untracked dogfood store:

```sh
git status --short        # expect only: ?? .clove/  (and any local ignores)
git rev-parse HEAD        # note this SHA — it's what you tag
```

---

## 2. Re-verify crate names are still free

Names can be claimed by anyone at any time. Right before publishing:

```sh
for c in clove-types clove-core clove-plugin clove-index clove-import clove-ipc \
         clove-mcp clove-tui clove-web cloved clove-cli \
         clove-sync-github clove-import-tk clove-import-beads; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "https://crates.io/api/v1/crates/$c")
  echo "$c -> HTTP $code ($([ "$code" = 404 ] && echo FREE || echo TAKEN))"
done
```

All fourteen must report **FREE (404)** for a first release. If any is TAKEN,
**stop** — resolve the collision (rename that crate, or contact the owner)
before continuing, exactly as was done for `clove` → `clove-cli`.

---

## 3. Publish order (bottom-up, dependency-respecting)

Each crate must be on crates.io **before** anything that depends on it, because
`cargo publish` rewrites path deps to registry version deps and verifies the
build against the registry. The workspace's internal edges are:

```
clove-types        → (none)
clove-core         → clove-types
clove-plugin       → clove-types, clove-core
clove-index        → clove-types, clove-core
clove-import       → clove-types, clove-core
clove-ipc          → clove-types, clove-core
clove-tui          → clove-types, clove-core
clove-mcp          → clove-types, clove-core, clove-ipc
clove-web          → clove-types, clove-core, clove-index
cloved             → clove-types, clove-core, clove-index, clove-ipc, clove-web
clove-cli          → clove-types, clove-core, clove-plugin, clove-index, clove-import, clove-ipc, clove-mcp, clove-tui, clove-web
clove-sync-github  → clove-types, clove-core, clove-plugin, clove-import (github)
clove-import-tk    → clove-types, clove-core, clove-plugin, clove-import
clove-import-beads → clove-types, clove-core, clove-plugin, clove-import
```

A valid topological publish order:

1. `clove-types`
2. `clove-core`
3. `clove-plugin`
4. `clove-index`
5. `clove-import`
6. `clove-ipc`
7. `clove-tui`
8. `clove-mcp`
9. `clove-web`
10. `cloved`
11. `clove-cli`
12. `clove-sync-github`
13. `clove-import-tk`
14. `clove-import-beads`

> `xtask` (`publish = false`), the `clove-plugin-echo` test fixture
> (`publish = false`), and the `fuzz/` crate (a separate excluded workspace) are
> **not** published — skip them. The three `clove-{sync-github,import-tk,import-beads}`
> plugins carry `publish = true` and go last: they depend on `clove-import` and
> `clove-plugin`, and nothing depends on them.

Internal deps already declare both `path` **and** `version = "0.1.0"` (see
`[workspace.dependencies]` in the root `Cargo.toml`), which is exactly what
crates.io requires — no Cargo.toml surgery is needed before publishing.

---

## 4. Publish, one crate at a time

> **Web-UI gotcha — read before publishing `clove-web`.** The embedded SvelteKit
> SPA lives in the git-ignored `crates/clove-web/dist-gz/`, so it is **not** part
> of the packaged `.crate`. When a user runs `cargo install clove-cli`,
> `clove-web/build.rs` rebuilds the SPA **only if `npm` is on their machine**;
> without npm they get the placeholder page. Two acceptable stances:
>
> - **Accept it** (recommended for v0.1.0): `cargo install` users who have Node
>   get the real UI; everyone else uses the pre-built GitHub Release binaries
>   (step 6), which always embed the real SPA. Document this in the README.
> - **Ship the built SPA in the crate**: add an `include = [...]` to
>   `crates/clove-web/Cargo.toml` covering `dist-gz/**`, run `npm run build`
>   before packaging, and publish with `--no-verify`. Heavier; defer unless a
>   Node-free `cargo install` with a working UI is a hard requirement.

Publish the leaf first as a **dry run** to catch metadata/packaging problems
without uploading (dry-run of non-leaf crates fails until their deps are live,
so only the leaf is meaningfully dry-runnable up front):

```sh
cargo publish -p clove-types --dry-run
```

Then publish for real, in order. Modern cargo **waits** for each crate to become
available on the index before returning, so the next publish resolves cleanly:

```sh
cargo publish -p clove-types
cargo publish -p clove-core
cargo publish -p clove-plugin  # after clove-core; needed by clove-cli + the plugins
cargo publish -p clove-index
cargo publish -p clove-import
cargo publish -p clove-ipc
cargo publish -p clove-tui
cargo publish -p clove-mcp
cargo publish -p clove-web     # see the web-UI gotcha above
cargo publish -p cloved
cargo publish -p clove-cli
cargo publish -p clove-sync-github
cargo publish -p clove-import-tk
cargo publish -p clove-import-beads
```

If a publish fails midway, fix the cause and **resume from the failed crate** —
the already-published crates are permanent and must not (and cannot) be
re-uploaded at the same version.

**Sanity-check installs from the registry** once `clove-cli` is live:

```sh
cargo install clove-cli    # installs the `clove` command
cargo install cloved       # optional daemon
clove version
```

---

## 5. Tag the release

Version is already `0.1.0` (inherited via `[workspace.package]`). Tag the
released commit and push — this is what drives the binary builds in step 6.

```sh
git tag -a v0.1.0 -m "clove v0.1.0"
git push origin v0.1.0
```

---

## 6. GitHub Release binaries (CI-driven)

Both CI systems trigger on a `v*` tag and build the native binaries — `clove` and
`cloved` plus the three plugin binaries (`clove-sync-github`, `clove-import-tk`,
`clove-import-beads`) — **carrying the real embedded SPA** (each job runs
`npm run build` into `crates/clove-web/dist`, then compiles with
`CLOVE_SKIP_WEB_BUILD=1`). Shipping the plugins in the release archive is what
lets a binary-install user run `clove sync github` / `clove import beads` without
a separate `cargo install`:

- **`.github/workflows/release.yml`** (GitHub Actions, canonical) — builds Linux,
  macOS arm64 + x86_64, and Windows; uploads tarballs/zips **plus SHA256
  checksums** to the GitHub Release for the tag. `permissions: contents: write`
  is already set.
- **`.woodpecker/release.yml`** (`when: event: tag`) — cross-builds the macOS
  `universal2` + Windows-gnu artifacts; its publish step is commented out
  pending a forge token secret.

After the tag push:

1. Watch the run: `gh run watch` (or the Actions tab).
2. Confirm the GitHub Release for `v0.1.0` has all platform archives + `.sha256`
   files attached.
3. Edit the Release notes (changelog / highlights) and publish it.

---

## 7. Homebrew tap (optional, for `brew install`)

Create a tap repo `egeapak/homebrew-clove` (so users run
`brew install egeapak/clove/clove`). Add a formula built from the macOS Release
tarball + its published SHA256:

```ruby
# Formula/clove.rb
class Clove < Formula
  desc "Fast, git-native, dependency-aware work-item tracker"
  homepage "https://github.com/egeapak/clove"
  version "0.1.0"
  license "MIT OR Apache-2.0"

  on_macos do
    url "https://github.com/egeapak/clove/releases/download/v0.1.0/clove-v0.1.0-macos-universal2.tar.gz"
    sha256 "PASTE_FROM_THE_RELEASE_.sha256_FILE"
  end

  def install
    bin.install "clove"
    bin.install "cloved"
    # The plugins must land on PATH next to `clove` so `clove sync github`,
    # `clove import tk`, and `clove import|export beads` resolve (PLUGIN_SYSTEM §5).
    bin.install "clove-sync-github"
    bin.install "clove-import-tk"
    bin.install "clove-import-beads"
  end

  test do
    assert_match "clove", shell_output("#{bin}/clove version")
  end
end
```

Adjust the asset filename to match what the Release actually produced (see the
`stage`/artifact names in `release.yml`). Update `url`/`sha256`/`version` on
every future release.

---

## 8. Post-release

- **Announce** the Release / crates.io links; close issue **#22** (and its
  parent epic #28 "Prepare clove for first public release" once its checklist is
  done).
- **Open the next dev cycle**: bump `[workspace.package] version` to the next
  pre-release (e.g. `0.2.0-dev`) in the root `Cargo.toml` so `master` is never
  mistaken for a published version.
- **Reconcile the tracker** if you closed issues by hand:
  `GITHUB_TOKEN="$(gh auth token)" clove sync github egeapak/clove`.

---

## Rollback / mistakes

- **Bad version already published?** `cargo yank --version 0.1.0 clove-cli`
  (repeat per crate). Yanking prevents *new* dependents from selecting it but
  does **not** delete it — existing `Cargo.lock`s still resolve. There is no
  un-publish; fix forward with `0.1.1`.
- **Wrong tag?** Delete and re-push before the CI finishes, or cut a new tag:
  `git tag -d v0.1.0 && git push origin :refs/tags/v0.1.0`.

---

## Alternative: automate it

For future releases, consider a workspace-aware tool instead of the manual
sequence in steps 3–5:

- [`cargo-workspaces`](https://crates.io/crates/cargo-workspaces) —
  `cargo workspaces publish` computes the order and publishes changed crates.
- [`release-plz`](https://release-plz.dev) — automates version bumps,
  changelogs, and publishing from CI on merge to `master`.

Either removes the hand-ordered `cargo publish` list and the "resume from the
failed crate" risk; adopt one once the release cadence justifies the setup.
