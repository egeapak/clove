# clove-tui — modular split + tick-driven refresh loop

**Date:** 2026-06-05
**Status:** Approved (design); implementation pending
**Scope crate:** `clove-tui`

## Problem

Two `clove-tui` files have grown into god-files that block future M4
expansion (write actions, live refresh, concurrent model):

- `ui.rs` — 1180 LOC, ~40 functions for every screen and component.
- `app.rs` — 994 LOC, one monolithic `App` struct holding all state plus all
  data/view/detail/filter behavior.

Separately, the event loop blocks indefinitely on `event::read()`. We want a
tick-driven loop with a defined cadence so the frame stays live and a future
background scan can animate progress.

## Goals

1. Split `ui.rs` into per-component and per-page modules under `ui/`.
2. Split the `App` struct into cohesive sub-structs (`Data`, `Listing`,
   `DetailPane`, `FilterMenu`) so state regroups by concern and each sub-struct
   can later be wrapped in its own lock without an API change.
3. Replace the blocking event loop with a tick-driven loop:
   **1fps when idle, redraw after every event, 10fps when busy.**

## Non-goals (explicitly deferred)

- **No concurrency, no threads, no locks now.** The struct split only *enables*
  fine-grained locking later. This is a structural refactor.
- **No background re-scan / spinner yet.** The cadence logic is provisioned via
  `is_busy()` (returns `false` today) and `on_tick()` (no-op today). The actual
  background scan that flips `is_busy()` to `true` is deferred to M4 extras,
  after the concurrent TUI model lands.
- No behavior change to filtering, sort, detail loading, or rendering output.

## Design

### App struct split (Approach A: state sub-structs, commands stay on `App`)

Fields regroup into sub-structs; the high-level command API that `lib.rs` calls
(`select_next`, `set_tab`, `refresh`, `cycle_sort_field`, `filter_toggle`, …)
stays on `App`. `App` is the coordinator that orchestrates cross-cutting flows
(`refresh`, `load_detail`, `recompute_view`) by reading across sub-structs — the
exact seams a future `RwLock`-per-sub-struct would split on, acquired in a fixed
order.

Rejected alternatives:
- **(B) Full OO decomposition** (behavior moved onto sub-structs,
  `app.list.select_next()`): too much call-site churn; cross-struct ops need
  references threaded around. Premature.
- **(C) Split files via `impl App` blocks only**: keeps one flat struct, does not
  satisfy "split the struct" or enable fine-grained locking.

Module tree (from `app.rs` 994 LOC):

```
app/mod.rs          App (coordinator) + command methods + new()/refresh()
                    + Mode, Focus, misc UI flags, is_busy()/tick_interval()/on_tick(),
                      fmt_ts/fmt_day, toggle_vec
app/data.rs         Data { store, all, ready, blocked, graph, load_warnings }
                    + scan/build + queries (is_ready, is_blocked, visible_for, total_count)
app/listing.rs      Listing { tab, view, list_state, sort, filter, search }
                    + recompute_view / apply_sort / restore_selection / clamp_selection
                    + Tab, SortField, SortDir, Sort, ViewFilter
app/detail.rs       DetailPane { detail_tab, detail, detail_scroll }
                    + Detail, DetailTab
app/filter_menu.rs  FilterMenu { menu, cursor }
                    + Facet, MenuItem, MenuValue, rebuild_facets, toggle logic
```

`App` holds `data: Data`, `list: Listing`, `detail: DetailPane`,
`filter_menu: FilterMenu`, plus the few UI flags (`mode`, `focus`, `show_help`,
`status`, `should_quit`).

Notes:
- The active `filter: ViewFilter` lives in `Listing` (it is part of the view
  projection); the menu's candidate values + cursor live in `FilterMenu`.
- `Detail` (per-selection loaded data) lives in `app/detail.rs`; `load_detail`
  stays an `App` method since it reads `Data` (store + graph + blocked) and the
  selection from `Listing`.
- Test call sites that touch fields directly (e.g. `app.detail`) update to the
  sub-struct path (e.g. `app.detail.detail`) — our own code, updated in place.

### ui.rs split (per-component + per-page)

Module tree (from `ui.rs` 1180 LOC):

```
ui/mod.rs              render() [only pub entry], render_body(), pick_layout/BodyLayout,
                       render_too_small
ui/util.rs             render_rule, right_align, border_style, kv, field_line, truncate,
                       centered_fixed, short_id, short_ref, join_ids, push_id_field
ui/style.rs            status_glyph, status_style, type_style, type_icon,
                       priority_style, priority_glyph  (+ colour-semantic unit tests)
ui/tabs.rs             render_tabs, short_tab
ui/list.rs             render_list, list_row
ui/detail/mod.rs       render_detail, detail_title, head_spans
ui/detail/overview.rs  overview_header/body/lines, title_span, assignee_deps_spans,
                       blocker_lines, relation_lines, status_spans, footer_line,
                       fit_labels, time_field
ui/detail/tree.rs      tree_lines, push_tree_node
ui/detail/comments.rs  comment_lines
ui/status.rs           render_status, filter_summary
ui/help.rs             render_help
ui/filter_menu.rs      render_filter_menu
```

- Inter-module functions become `pub(crate)`; `ui::render` stays the only `pub`
  entry.
- The colour-semantic unit tests (`priority_style`, `status_style`,
  `type_style`) move from `ui.rs`'s `tests` module into `ui/style.rs`.
- **CLAUDE.md** must be updated: it currently points reviewers at "the `tests`
  module in `ui.rs`" for colour semantics — update to `ui/style.rs`.

### Tick-driven refresh loop (lib.rs)

Replace blocking `event::read()` with a poll loop:

```
draw once up front
while !should_quit:
    timeout = app.tick_interval()        // 1000ms idle, 100ms when app.is_busy()
    if event::poll(timeout)?:            // input arrived
        match event::read()?:
            Key press        -> handle_key(...); draw
            Resize           -> draw
            _                -> {}
    else:                                // timeout elapsed → tick
        app.on_tick(); draw
```

- `tick_interval() -> Duration`: `100ms` when `is_busy()`, else `1s`. Directly
  unit-tested.
- `is_busy() -> bool`: returns `false` now (hook for the deferred background
  scan).
- `on_tick()`: no-op now (future: advance spinner frame).
- "Always redraw after handling an event" is satisfied by the `draw` after
  `handle_key`. Resize now also redraws (a latent fix that falls out for free).

## Testing & verification

- **Pure-refactor proof:** existing insta render snapshots stay byte-identical;
  no `INSTA_UPDATE` regeneration expected. If any snapshot changes, the refactor
  altered behavior and must be corrected (not accepted).
- New unit test for `tick_interval()` (busy → 100ms, idle → 1s).
- Existing `app.rs` data-layer + `TestBackend` smoke tests keep passing
  (updated for new field/module paths only).
- Quality gate (all clean): `cargo fmt --check`,
  `cargo clippy --all-targets -p clove-tui -- -D warnings`,
  `cargo test --workspace`.
- No new dependencies.

## Docs to update

- `CLAUDE.md` — colour-test location (`ui.rs` → `ui/style.rs`).
- `HANDOFF.md` / `docs/IMPLEMENTATION_PLAN.md` — note the new `clove-tui` module
  layout and the tick-loop cadence; flag the deferred background-scan/concurrent
  model as the next M4 step.
