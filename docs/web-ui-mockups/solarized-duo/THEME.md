# Solarized Duo — `solarized-duo`

A calm, warm, scholarly theme built on Ethan Schoonover's classic **Solarized**
palette. The light variant ("Solarized Light", base3 paper) is the primary; the
dark variant ("Solarized Dark", base03) is its first-class counterpart, exposed
via the top-bar sun/moon toggle and demonstrated in the timeline section.

## Palette

### Base tones (Solarized monotones)
| Hex | Name | Light role | Dark role |
|-----|------|-----------|-----------|
| `#fdf6e3` | base3  | page background (paper) | — |
| `#eee8d5` | base2  | label chips, sunk wells | — |
| `#93a1a1` | base1  | muted text | primary text |
| `#839496` | base0  | — | soft text |
| `#657b83` | base00 | soft text | — |
| `#586e75` | base01 | **primary text** | muted text |
| `#073642` | base02 | headings / strong ink | surface (cards) |
| `#002b36` | base03 | code-block bg | **page background** |

### Accents (Solarized)
| Hex | Name | Mapped to |
|-----|------|-----------|
| `#b58900` | yellow  | priority p2 `•`; light/dark toggle active; in-progress (timeline) |
| `#cb4b16` | orange  | priority p1 `↑`; "today" marker; warm emphasis |
| `#dc322f` | red     | priority p0 `!`; bug type `B`; blocked badges / dep arrows |
| `#d33682` | magenta | epic type `E`; inline `code` text |
| `#6c71c4` | violet  | docs type `D`; epic bar; `ada` avatar |
| `#268bd2` | blue    | **primary action**; feature type `F`; priority p3; open status; "created" series |
| `#2aa198` | cyan    | chore type `C`; `lin` avatar; logo gradient |
| `#859900` | green   | closed status; live-connection dot; "closed" series; logo gradient |

### Role tokens
Both themes are tokenized at `:root` (light) and `.dark` (dark). Key roles:
`--bg`, `--bg-elev`, `--bg-sunk`, `--surface`, `--border`, `--border-strong`,
`--text` / `--text-soft` / `--text-mute` / `--text-faint`, `--accent` (blue),
plus the raw Solarized hexes as named vars (`--yellow` … `--base03`).

**Semantic glyphs:** status `○` open (blue) · `◐` in_progress (yellow) · `●`
closed (green); priority p0 `!` red · p1 `↑` orange · p2 `•` yellow · p3 `•`
blue · p4 `↓` base1; type letters B/F/C/D/E on red/blue/cyan/violet/magenta.

## Typography
System font stacks only (offline). Sans: `-apple-system, BlinkMacSystemFont,
"Segoe UI", Roboto, …`. Mono: `ui-monospace, "SF Mono", Menlo, Consolas, …` for
ids, glyphs, code, timestamps and axis labels. Base size 14px / line-height 1.5;
section headings 17px; detail title 24px (-0.4px tracking, weight 700).

## Spacing & radius
Spacing scale (4px base): 4 · 8 · 12 · 16 · 24 · 32 · 48. Radius scale:
`--radius-sm` 7px (cards/inputs) · `--radius` 10px (buttons/panels) ·
`--radius-lg` 14px (app shells/banner) · `--radius-pill` 999px (chips, toggles,
avatars). Hairline borders use warm parchment tones (`#e7dfc4`/`#dcd3b4`), not
neutral grey, so the paper feel survives every surface.

## Rationale
Solarized was engineered for long, comfortable reading sessions — precise CIELAB
relationships keep every accent legible on both the warm base3 paper and the deep
base03 ink without ever shouting. We lean into that restraint: a soft parchment
canvas, hairlines in warm tan rather than cold grey, and the eight signature
accents carrying *meaning* (status, priority, type) instead of decoration. The
result reads unmistakably "Solarized" and distinctly *not* a generic white SaaS —
scholarly, low-contrast, and easy on the eyes across a full workday. Because both
the light and dark token sets are first-class and structurally identical, the
duo flips instantly via one toggle with zero layout shift.
