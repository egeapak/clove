# CLAUDE.md

Guidance for working in this repo. `clove` is a Cargo workspace (`crates/*`):
`clove-core` (model/graph/store), `clove-tui` (the read-only terminal browser),
plus `clove-cli`, `cloved`, `clove-index`, `clove-ipc`, `clove-import`.

See `HANDOFF.md` and `docs/IMPLEMENTATION_PLAN.md` for the full design/state.

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
- **Fonts:** it loads `DejaVu Sans Mono` (regular + bold), falling back to
  `Liberation Mono`; it panics if neither is installed. DejaVu is preferred for
  its broad box-drawing / geometric-shape coverage (status `○ ◐ ●`, priority
  `! ↑ • ↓`, etc.). A small `subst()` table swaps in look-alikes for any glyph a
  font lacks. On Debian/Ubuntu: `apt-get install fonts-dejavu`.
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
