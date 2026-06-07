# Midnight IDE — theme spec

A dark, developer/IDE-native theme for clove's web UI. A sibling to the existing
One-Dark-ish terminal browser: deep charcoal/navy surfaces, a calm syntax-accent
palette, hairline borders, and a strict typographic split — **monospace for ids
and metadata**, a crisp sans for titles and body.

## Palette (hex)

### Surfaces
| Token | Hex | Use |
|---|---|---|
| `--bg-app` | `#0d1117` | viewport / app background (GitHub-dark base) |
| `--bg-surface` | `#11161f` | panels, columns, table |
| `--bg-elevated` | `#161c28` | cards, rows, dropdowns |
| `--bg-hover` | `#1c2433` | hover / active row |
| `--bg-inset` | `#0a0e14` | code blocks, wells |
| `--border` | `#222b3a` | hairline borders |
| `--border-strong` | `#30394b` | focused / emphasized borders |

### Text
| Token | Hex | Use |
|---|---|---|
| `--fg` | `#e6edf3` | primary text |
| `--fg-muted` | `#9aa7b8` | secondary text |
| `--fg-dim` | `#5f6b7c` | tertiary / metadata |

### Accents (syntax-highlight family)
| Token | Hex | Meaning |
|---|---|---|
| `--accent` | `#58a6ff` | primary blue (links, focus, brand) |
| `--green` | `#3fb950` | feature, closed, success, created |
| `--red` | `#f85149` | bug, p0, blocked |
| `--orange` | `#e3833c` | p1 |
| `--amber` | `#d29922` | p2, in-progress |
| `--icy` | `#79c0ff` | p3 |
| `--purple` | `#bc8cff` | docs |
| `--gold` | `#e3b341` | epic |
| `--gray` | `#7d8896` | chore, p4 |

## Typography
- **Sans (titles/body):** `system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`
- **Mono (ids/metadata/code):** `ui-monospace, "SF Mono", Menlo, Consolas, monospace`
- Sizes: 11px micro (chips/meta), 12px mono ids, 13px body, 14px row titles,
  16px section/card headers, 20px detail title, 13px/700 banner.
- Weights: 400 body, 500 labels, 600 titles, 700 ids/banner.

## Spacing & radius
- Spacing scale (px): `4, 8, 12, 16, 20, 24, 32` (`--space-1..7`).
- Radius: `--radius-sm 4px`, `--radius-md 6px`, `--radius-lg 10px`, pill `999px`.
- Content width: `1440px`, centered. Hairline (1px) borders throughout.

## Design rationale
Midnight IDE leans all the way into the engineer-at-their-editor feeling: the base
is GitHub-dark `#0d1117`, panels step up in luminance rather than using shadows, so
depth reads as it does in a code editor — flat, layered, calm. The accent palette is
borrowed directly from syntax highlighting (blue/green/red/amber/purple), which lets
status, type, and priority be color-coded without ever feeling like a candy SaaS app.
Ids, counts, dates and code are set in monospace so they line up in dense tables the
way a developer expects, while titles use a clean sans for scan-ability. Borders are
single-pixel hairlines and corners are tight (4–6px) to maximize information density,
echoing clove's terminal roots while giving the web the precision of Linear.
