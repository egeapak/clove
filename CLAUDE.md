# CLAUDE.md

Guidance for working in this repo. `clove` is a Cargo workspace (`crates/*`):
`clove-core` (model/graph/store), `clove-tui` (the read-only terminal browser),
`clove-web` (the web UI server + embedded SvelteKit SPA, `clove serve`),
plus `clove` (CLI), `cloved`, `clove-index`, `clove-ipc`, `clove-import`.

See `HANDOFF.md` and `docs/IMPLEMENTATION_PLAN.md` for the full design/state, and
`docs/M4_WEB_UI_PLAN.md` for the web UI.

## Web UI (`clove-web`)

The SvelteKit SPA lives in `crates/clove-web/web/` and is built by
`crates/clove-web/build.rs` (`npm run build` when `npm` is present and sources
changed; otherwise a placeholder), gzipped into `dist-gz/`, and embedded via
`rust-embed` (both `dist/` and `dist-gz/` are git-ignored — never commit them). A
Node-free `cargo build` still works (the placeholder is embedded);
`CLOVE_SKIP_WEB_BUILD=1` skips the npm build. Frontend checks:
`cd crates/clove-web/web && npm run check && npm run test` (svelte-check + vitest).
Markdown rendering is micromark + a custom id-autolink extension
(`lib/micromark-clove-id.ts`) — no hand-written markdown regex/sanitizer.

## Quality gate

Before committing UI or logic changes, run (all must be clean):

```sh
cargo fmt && cargo fmt --check
cargo clippy --all-targets -- -D warnings      # or scope with -p <crate>
cargo test --workspace
```

## TUI render snapshots (insta, run in CI)

`crates/clove-tui/src/snapshot.rs` renders the UI to a `ratatui` `TestBackend`
and flattens the cell buffer to **plain text** (no colour/style) so snapshots
stay font- and theme-independent. Every state is captured at three terminal
shapes (portrait/landscape/square) to exercise the adaptive layout.

- These tests **only capture glyphs, not colour** — a pure colour change won't
  alter a snapshot (you must eyeball a screenshot instead; see below).
- After an intentional layout/glyph change, review and accept the new output:

  ```sh
  INSTA_UPDATE=always cargo test -p clove-tui    # regenerate, then inspect the diff
  ```

### Validating colour

Colour is **not** in the render snapshots (kept text-only on purpose, for clean
font/theme-independent diffs). Instead, the colour *semantics* are locked by
unit tests on the style functions in `ui/style.rs` (`tests` module: `priority_style`,
`status_style`, `type_style`) — these assert the `fg`/`bg` each returns, with no
layout or cell coordinates involved, so they don't break when the layout shifts.
**When you change a colour constant, update those tests** (and regenerate the
screenshots to eyeball it).

If you ever want end-to-end "right colour reached the right cell" coverage,
ratatui's `Buffer` `Debug` impl prints a positional `styles:` list, so
`insta::assert_debug_snapshot!(terminal.backend().buffer())` works — but its
diffs are noisy under layout changes, which is why we prefer the style-function
unit tests.

## TUI screenshots (PNG) — how to "see" the terminal

The colour PNGs under `docs/screenshots/` are produced by a manual,
`#[ignore]`d test (`generate_screenshots` in `snapshot.rs`). It renders each
screen's **real cell buffer (colours + bold + dim)** to a PNG by drawing each
glyph with a system monospace font — this is how the at-a-glance screenshots in
this project are made.

Regenerate them with:

```sh
cargo test -p clove-tui generate_screenshots -- --ignored
```

Details worth knowing:

- **Output:** `docs/screenshots/*.png` (e.g. `01-overview.png`,
  `10-portrait-detail.png`). The directory is **git-ignored** — screenshots are
  artifacts for inspection, not committed.
- **Fonts:** `DejaVu Sans Mono` (regular + bold) is **vendored** under
  `crates/clove-tui/assets/fonts/` and loaded unconditionally (via
  `include_bytes!`) — no system-font dependency, so the tool is cross-platform
  (Linux/macOS/Windows/CI) and renders byte-identically everywhere. DejaVu is
  used for its broad box-drawing / geometric-shape coverage (status `○ ◐ ●`,
  priority `! ↑ • ↓`, etc.); a small `subst()` table swaps in look-alikes for any
  glyph it lacks. License: `crates/clove-tui/assets/fonts/LICENSE` (Bitstream
  Vera / DejaVu — free, redistributable).
- **Colours:** `Color::Indexed(n)` is resolved through a built-in xterm-256
  palette and ANSI names map to a One-Dark-ish set, so the PNG closely matches a
  real terminal. If you change a colour constant (e.g. `priority_style`),
  regenerate and **open the PNG to verify** — the text snapshots won't show it.
- **To view a generated PNG** in this harness, `Read` the file path (it renders
  visually), or surface it to the user with the file-send tool.
- **To add/adjust a shot:** edit the `generate_screenshots` body — each block
  builds an `App` from the shared `fixture()`, drives it (set tab, focus, open
  filter/search/help…), and calls `save("NN-name", width, height, &mut app)`.
  Wide shots use `120×34`; portrait `46×40`.

When a change affects what the TUI looks like, regenerate the screenshots and
look at them before claiming the change works.
