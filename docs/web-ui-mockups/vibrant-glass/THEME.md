# Theme: Vibrant Glass (`vibrant-glass`)

A bold, premium glassmorphism take on clove: frosted translucent panels floating
on a deep indigo→plum gradient with ambient color blobs, vivid violet→fuchsia→cyan
accents, and glossy gradient buttons.

## Palette

### Background
| Token    | Hex       | Use |
|----------|-----------|-----|
| `--bg-0` | `#0b0720` | deepest indigo (gradient start) |
| `--bg-1` | `#160d33` | plum (gradient mid) |
| `--bg-2` | `#1d1140` | violet shade (gradient end) |

App background = layered radial color blobs (violet `#7c3aed`, fuchsia `#db2bb3`,
cyan `#1899b3`, pink `#ff4d8d`) over a `160deg` linear `--bg-0 → --bg-1 → --bg-2`.

### Glass surfaces (translucent white over the gradient)
| Token                 | Value                     |
|-----------------------|---------------------------|
| `--glass-1`           | `rgba(255,255,255,0.06)`  |
| `--glass-2`           | `rgba(255,255,255,0.085)` |
| `--glass-3`           | `rgba(255,255,255,0.12)`  |
| `--glass-stroke`      | `rgba(255,255,255,0.16)`  |
| `--glass-stroke-soft` | `rgba(255,255,255,0.09)`  |

Panels use `backdrop-filter: blur(22–26px) saturate(150–160%)`, a 1px light
border, and an inset top highlight (`inset 0 1px 0 rgba(255,255,255,0.05)`).

### Text
| Token     | Hex       | Use |
|-----------|-----------|-----|
| `--txt-1` | `#f4f1ff` | primary |
| `--txt-2` | `#c9c2e8` | secondary |
| `--txt-3` | `#968dbe` | muted / labels |
| `--txt-4` | `#6f6796` | faint / placeholders |

### Vivid accents
| Token       | Hex       | Meaning |
|-------------|-----------|---------|
| `--violet`  | `#8b5cf6` | brand / feature |
| `--fuchsia` | `#e445c7` | brand / epic |
| `--cyan`    | `#34d6ee` | live / docs / closed-accent |
| `--pink`    | `#ff4d8d` | p0 hot / bug / blocked |
| `--orange`  | `#ff9a3c` | p1 |
| `--amber`   | `#ffd24a` | p2 / chore |
| `--slate`   | `#8c9bc0` | p4 |
| `--green`   | `#46e0a8` | status closed ● / live dot |

Priority: p0 `!` pink · p1 `↑` orange · p2 `•` amber · p3 `•` cyan · p4 `↓` slate.
Type: B bug (pink) · F feature (violet) · C chore (amber) · D docs (cyan) · E epic (fuchsia).

### Gradients
- **Brand text** `--grad-brand`: `120deg #8b5cf6 → #e445c7 → #34d6ee`
- **Buttons / active** `--grad-btn`: `120deg #9a6bff → #e445c7`
- **Timeline bars**: feature `#8b5cf6→#34d6ee`, bug `#ff4d8d→#ff9a3c`,
  chore `#ffd24a→#ff9a3c`, epic `#e445c7→#8b5cf6`, docs `#34d6ee→#46e0a8`

## Typography
System font stack only: `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, …`.
Monospace (ids, code): `ui-monospace, "SF Mono", Menlo, Consolas, monospace`.
Base 14px / 1.45. Title 28px/800, detail title 24px/800, section labels 13px uppercase
with 0.14em tracking. Brand wordmark and headline use the brand gradient as clipped text.

## Spacing & radius scale
- Spacing (4px base): 4 / 8 / 12 / 16 / 24 / 32 / 48 px (`--s-1`…`--s-7`).
- Radius: `--r-xs 6` · `--r-sm 9` · `--r-md 14` · `--r-lg 20` · `--r-xl 28` · `--r-pill 999`.
- Elevation: glass `0 10px 40px /.40` + inset highlight; card `0 6px 22px /.32`;
  glow `0 8px 30px rgba(228,69,199,.35)` on primary buttons / active pills.

## Rationale
Vibrant Glass commits hard to a premium, energetic identity that reads instantly as
"modern product," distinct from a flat IDE-dark or a bright minimal-SaaS look. Real
glassmorphism — translucent surfaces, backdrop blur, thin light borders, and ambient
gradient blobs — gives depth and layering so the kanban columns, table, and timeline
panels feel like physical frosted glass over a glowing field. A disciplined
violet→fuchsia→cyan accent system carries all semantic color (status, priority, type,
dependencies) so the palette stays cohesive rather than noisy. Text stays high-contrast
(`#f4f1ff` on dark glass) to remain legible on translucent surfaces, and a single
gradient button/active treatment keeps the call-to-action language consistent across
every screen. The result is colorful and glossy but still calm enough to track real work.
