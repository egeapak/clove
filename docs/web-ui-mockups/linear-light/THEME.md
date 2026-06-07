# Theme — Linear Light (`linear-light`)

A bright, minimal modern-SaaS aesthetic inspired by Linear / Height / Vercel.
Near-white surfaces, hairline borders, soft shadows, generous whitespace, and a
single confident indigo accent. Color is reserved for semantic meaning only
(status / priority / type).

## Palette

### Surfaces & borders
| Token | Hex | Use |
|---|---|---|
| `--bg-page` | `#fbfcfd` | App page background (with faint indigo/violet corner glows) |
| `--surface` | `#ffffff` | Cards, panels, top bar |
| `--surface-2` | `#f4f5f8` | Inputs, chips, subtle fills |
| `--surface-3` | `#eef0f4` | Deeper fill |
| `--hover` | `#f5f6f9` | Row / card hover |
| `--border` | `#e7e9ee` | Default hairline border |
| `--border-strong` | `#d8dbe2` | Emphasis border |
| `--hairline` | `#edeef2` | Internal dividers / table rules |

### Text
| Token | Hex |
|---|---|
| `--text` | `#1c1e26` |
| `--text-2` | `#5b606e` |
| `--text-3` | `#8b90a0` |
| `--text-4` | `#abb0bd` |

### Accent (indigo / violet)
| Token | Hex |
|---|---|
| `--accent` | `#5b5bd6` |
| `--accent-hover` | `#4f4fc9` |
| `--accent-text` | `#4a4abf` |
| `--accent-soft` | `#eeeefb` |
| `--accent-soft-2` | `#e3e3f8` |

### Semantic (status / priority / type)
| Meaning | Token | Hex |
|---|---|---|
| Bug / P0 | `--red` | `#e5484d` |
| P1 / in-progress glyph | `--orange` | `#e8731b` |
| P2 | `--amber` | `#d9a40b` |
| Feature / closed | `--green` | `#2f9e44` |
| P3 (icy) | `--icy` | `#5f9fd6` |
| Docs | `--purple` | `#8b5cf6` |
| Epic | `--gold` | `#c99700` |
| Chore / P4 / unassigned | `--gray` | `#9aa0ad` |

Each semantic color also has a `*-soft` tinted background for chips/badges
(e.g. `--red-soft #fdecec`, `--green-soft #e7f5ea`, `--gold-soft #faf2d6`).

**Glyph semantics:** status `○` open / `◐` in_progress / `●` closed;
priority `!` p0 (red) `↑` p1 (orange) `•` p2 (amber) `•` p3 (icy) `↓` p4 (gray);
type letters B=bug (red) F=feature (green) C=chore (gray) D=docs (purple) E=epic (gold).

## Typography
- **UI:** `system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif`.
- **Mono (ids, code, weeks):** `ui-monospace, "SF Mono", "JetBrains Mono", "Roboto Mono", Menlo, Consolas, monospace`.
- Base 14px / 1.5. Title 22–24px at `-0.02em` tracking; section/card titles
  15–16px at `-0.01em`; eyebrow labels 11px uppercase, `+0.05em`, `--text-3`.
- Tight negative tracking on display text, weights 480–680 (no ultra-bold).

## Spacing scale (4px base)
`4 · 8 · 12 · 16 · 20 · 24 · 32 · 40 · 48` (`--s1`…`--s12`).

## Radius scale
`4 (xs) · 6 (sm) · 8 (md) · 12 (lg) · 16 (xl) · 999 (full)`.

## Shadow scale
`xs` `0 1px 2px /.04` → `sm` → `md` `0 4px 12px /.07` → `lg` `0 12px 32px /.10`.
Soft, low-spread, cool-gray — never harsh.

## Rationale
Linear Light commits hard to a bright, premium, keyboard-first feel: lots of
whitespace, hairline borders, and soft cool shadows give the UI a calm,
documentation-like clarity where the work items — not chrome — are the focus.
A single restrained indigo accent carries all interaction (primary buttons,
active tabs, selection, the "today" marker), so the eye reads it instantly as
"the app's voice." All other color is strictly semantic: status, priority, and
type each own a hue with a matching soft tint, making a board scannable at a
glance without a legend. Type uses a system sans with tight negative tracking
for a modern-SaaS polish and a mono stack for ids and code so the developer
origin stays legible. The result feels fast and file-native — fitting for a tool
where Markdown is the source of truth.
