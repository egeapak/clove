# Vendored fonts

`DejaVuSansMono.ttf` / `DejaVuSansMono-Bold.ttf` (DejaVu Sans Mono 2.37) are
bundled so the `#[ignore]`d `generate_screenshots` tooling in
`crates/clove-tui/src/snapshot.rs` renders identically on **any** platform with
no system-font dependency. DejaVu is preferred for its broad box-drawing /
geometric-shape coverage (status `○ ◐ ●`, priority `! ↑ • ↓`, borders).

The font loader (`load_fonts`) uses these vendored files **unconditionally** —
it never reads system fonts. That keeps screenshots byte-identical on every
platform (Linux, macOS, Windows, CI) with no machine-to-machine variation and no
system-font dependency to install.

**License:** see `LICENSE` — Bitstream Vera Fonts Copyright (a permissive, free
license that allows redistribution and bundling); the DejaVu changes are public
domain. These are data assets, not linked code, and do not affect the crate's
`cargo deny` license posture.

Source: <https://github.com/dejavu-fonts/dejavu-fonts> (release `version_2_37`).
