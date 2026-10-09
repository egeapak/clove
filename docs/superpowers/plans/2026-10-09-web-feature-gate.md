# Gate the web server behind a `web` feature; download the SPA at build time

> **Status:** Implemented on `feat/web-feature` (clove 0.1.4), 2026-10-09. Where the
> shipped code differs from this plan, the code and `docs/RELEASE.md` win; the main
> deviations are noted inline.

## Why

Publishing `clove-web` to crates.io needs the built SPA inside the package.
`dist-gz/` is git-ignored, and Cargo reports a force-`include`d ignored file as
an uncommitted change, so every publish needs `--allow-dirty`. The Cargo team
treats this as working as designed (rust-lang/cargo#16872, #12294).

The HTTP/WebSocket API in `clove-web` has one consumer in product code: the SPA
(`crates/clove-web/web/src/lib/api.ts`). Nothing else in the workspace calls it;
tests and the two smoke scripts only check that it answers. So the web server
and the UI are one optional unit, not two.

## Design

- **One feature, `web`, off by default**, on `clove-cli` and `cloved`. It makes
  `clove-web` an `optional` dependency. `full` includes `web`, so release
  binaries keep the UI. Existing precedent: `git-sync`, `github-sync`, `full`.
- **`clove-web` stays unconditional.** Built from the repo it embeds the SPA
  exactly as today. Built from a published package it downloads the SPA.
- **Without `web`:** `cloved` has no HTTP stack; `clove serve` exits with a clear
  error naming `--features web` and the release binaries; `daemon status` shows
  no web URL.
- **SPA acquisition in `build.rs`, in order:**
  1. `web/` sources via npm (existing behaviour; everything is generated into
     `OUT_DIR`, since Cargo's publish verification rejects a build script that
     writes into the package source), else
  2. the packaged-crate case: download `clove-web-dist-v<version>.tar.gz` and its
     `.sha256` from the matching GitHub Release and verify; a failed download or
     mismatch is a hard build error, never a silent placeholder.
  `DOCS_RS` embeds the placeholder so docs.rs (offline) still builds, and
  `CLOVE_WEB_DIST_DIR` (a built `dist/`) / `CLOVE_WEB_DIST_BASE_URL` (a mirror) are
  the offline and packager escape hatches.
- **Integrity:** a checksum asset on the same release (decided 2026-10-09). It
  trusts one host for both files; a pinned in-repo hash was rejected because it
  needs a byte-reproducible tarball across machines and a hash committed before
  the tag.
- **Package contents:** `dist-gz/**` leaves `include`, so no dirty check and no
  `--allow-dirty`. The download client (`ureq`) is an ordinary build-dependency.

## Release ordering (changes from the 0.1.3 runbook)

1. `release.yml` already builds the SPA with npm. New step: pack it into
   `clove-web-dist-v<version>.tar.gz`, write the `.sha256`, upload both to the
   draft Release.
2. Publish the GitHub Release (assets must be anonymously downloadable; drafts
   are not).
3. Only then `cargo publish`, in the existing order. `cargo publish -p clove-web`
   verifies by building from the package, which downloads the assets.
   A dry run of `clove-web` fails before step 2.

## Phases

1. **Feature gating** (`clove-cli`, `cloved`): optional `clove-web`, `web` and
   `full` features, `cfg` on `cmd/serve.rs` and the `hub.rs` web paths, clear
   error without the feature. Tests needing the server get the same `cfg`; the
   default-feature test run must stay green.
2. **`clove-web` packaging:** the `build.rs` acquisition order above, `include`
   change, optional download dependency, `DOCS_RS` fallback, `packaging.rs`
   guards (no `dist-gz` in `include`; the `DOCS_RS` fallback exists; a corrupt
   download fails the build).
3. **Release tooling:** pack-and-upload step in `release.yml`;
   `scripts/release/publish.sh` (runbook order, stops at the first failure,
   refuses unless `HEAD` is the tag and the assets are downloadable);
   `docs/RELEASE.md` §4.
4. **CI and docs:** a default-features job (no `web`) next to `--all-features`;
   README install (lean default plus an explicit `--features web` line and a note
   that release binaries include the UI); DESIGN feature table; CHANGELOG;
   `RELEASE_CHECKLIST.md` (install from crates.io into a temp root with and
   without `--features web`).
5. **Release 0.1.4:** bump, PR, merge, tag, draft, checklist, publish the GitHub
   Release, then the crates.

## Risks

- A published crate whose `--features web` build depends on a GitHub Release asset
  staying available. Deleting a release breaks that version permanently.
- Offline, proxied or vendored `--features web` installs fail unless
  `CLOVE_WEB_DIST_DIR` points at a built `dist/`.
- The default `cargo install` loses the UI; the README must say so plainly.
- 0.1.3 will never exist on crates.io (its GitHub Release stays as is).

## Out of scope

Moving the web server into a separate plugin binary (the daemon serves it
in-process, so that needs an IPC split), and changing the API.
