# clove-tui Modular Split + Tick-Driven Refresh Loop — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Break the `clove-tui` god-files (`app.rs` 994 LOC, `ui.rs` 1180 LOC) into cohesive modules, regroup `App` state into sub-structs, and replace the blocking event loop with a tick-driven loop (1fps idle / redraw-after-event / 10fps when busy).

**Architecture:** State regroups into four sub-structs (`Data`, `Listing`, `DetailPane`, `FilterMenu`) held by `App`; command methods stay on `App` as the coordinator (Approach A from the spec). Type definitions move into the new `app/` sub-modules and are re-exported from `app/mod.rs` so external type paths (`app::Tab`, `app::DetailTab`, …) stay stable. `ui.rs` is carved into per-component + per-page modules under `ui/`. The loop change adds `is_busy()`/`tick_interval()`/`on_tick()` hooks; the background scan that flips `is_busy()` is deferred to M4.

**Tech Stack:** Rust, `ratatui` (re-exports `crossterm`), `insta` snapshot tests, `pulldown-cmark`.

**Invariant for the whole refactor (Tasks 1–6):** This is a *pure* refactor. Existing `insta` render snapshots MUST stay byte-identical — `cargo test -p clove-tui` passing with **no** snapshot changes is the proof nothing visible changed. If a snapshot diff appears, the refactor altered behavior: fix the code, do NOT run `INSTA_UPDATE`.

**Spec:** `docs/superpowers/specs/2026-06-05-tui-modular-split-and-tick-loop-design.md`

---

## Field → sub-struct mapping (reference for Tasks 1–4)

| Old field on `App`     | New location                 |
|------------------------|------------------------------|
| `store`                | `data.store`                 |
| `all`                  | `data.all`                   |
| `ready`                | `data.ready`                 |
| `blocked`              | `data.blocked`               |
| `graph`                | `data.graph`                 |
| `load_warnings`        | `data.load_warnings`         |
| `tab`                  | `list.tab`                   |
| `view`                 | `list.view`                  |
| `list_state`           | `list.list_state`            |
| `sort`                 | `list.sort`                  |
| `filter`               | `list.filter`                |
| `search`               | `list.search`                |
| `detail_tab`           | `detail.detail_tab`          |
| `detail` (the `Option<Detail>`) | `detail.detail`     |
| `detail_scroll`        | `detail.detail_scroll`       |
| `filter_menu`          | `filter_menu.menu`           |
| `filter_cursor`        | `filter_menu.cursor`         |
| `mode` `focus` `show_help` `status` `should_quit` | stay on `App` |

Command/query **methods** (`refresh`, `select_*`, `set_tab`, `recompute_view`, `load_detail`, `visible*`, `is_ready`, `is_blocked`, `is_menu_selected`, `selected_frontmatter`, `cycle_sort_field`, `filter_*`, `clear_filters`, …) **stay on `App`**; their bodies get `self.<field>` → `self.<sub>.<field>` rewrites per the table.

Type definitions move with their sub-struct and are re-exported from `app/mod.rs`:
- `Tab`, `SortField`, `SortDir`, `Sort`, `ViewFilter` → `app/listing.rs`
- `Detail`, `DetailTab` → `app/detail.rs`
- `Facet`, `MenuItem`, `MenuValue` → `app/filter_menu.rs`
- `Mode`, `Focus`, `fmt_ts`, `fmt_day`, `toggle_vec` stay in `app/mod.rs`

---

## Task 0: Scaffold the `app/` module directory

**Files:**
- Move: `crates/clove-tui/src/app.rs` → `crates/clove-tui/src/app/mod.rs`

- [ ] **Step 1: Move the file into a directory (no code change)**

```bash
cd crates/clove-tui/src
mkdir app
git mv app.rs app/mod.rs
```

- [ ] **Step 2: Verify it still builds and tests pass unchanged**

Run: `cargo test -p clove-tui`
Expected: PASS, identical to before (Rust treats `app/mod.rs` exactly like `app.rs`).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor(tui): move app.rs into app/ module directory"
```

---

## Task 1: Extract the `Data` sub-struct

**Files:**
- Create: `crates/clove-tui/src/app/data.rs`
- Modify: `crates/clove-tui/src/app/mod.rs`
- Modify: `crates/clove-tui/src/ui.rs` (one site: `app.load_warnings` → `app.data.load_warnings`)

- [ ] **Step 1: Create `app/data.rs` with the `Data` struct**

```rust
//! The store-derived data layer: the file-store scan result and the graph.
//!
//! Refreshed wholesale by [`Data::scan`]. This is the cohesive state a future
//! concurrent model would put behind its own lock.

use std::collections::{HashMap, HashSet};

use clove_core::{BlockedItem, CloveId, GraphStore, ItemFrontmatter, ItemStore};

pub struct Data {
    pub store: ItemStore,
    pub all: Vec<ItemFrontmatter>,
    pub ready: HashSet<CloveId>,
    pub blocked: HashMap<CloveId, BlockedItem>,
    pub graph: GraphStore,
    /// Non-fatal load problems (e.g. files that failed to parse).
    pub load_warnings: Vec<String>,
}

impl Data {
    pub fn new(store: ItemStore) -> Self {
        Data {
            store,
            all: Vec::new(),
            ready: HashSet::new(),
            blocked: HashMap::new(),
            graph: GraphStore::build(&[]).0,
            load_warnings: Vec::new(),
        }
    }

    /// Re-scan the store and rebuild the derived graph state. Returns `Err` with
    /// a human message if the scan itself failed (caller surfaces it as status).
    pub fn scan(&mut self) -> Result<(), String> {
        let (mut frontmatters, errors) = self
            .store
            .scan_frontmatter()
            .map_err(|e| format!("scan failed: {e}"))?;

        let (graph, _dangling) = GraphStore::build(&frontmatters);
        let ranks = graph.topological_ranks();
        frontmatters.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| {
                    let ra = ranks.get(&a.id).copied().unwrap_or(usize::MAX);
                    let rb = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                    ra.cmp(&rb)
                })
                .then_with(|| a.id.cmp(&b.id))
        });

        self.ready = graph.ready_items().into_iter().collect();
        self.blocked = graph
            .blocked_items()
            .into_iter()
            .map(|b| (b.id.clone(), b))
            .collect();
        self.all = frontmatters;
        self.graph = graph;
        self.load_warnings = errors.iter().map(|e| e.to_string()).collect();
        Ok(())
    }

    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.ready.contains(id)
    }

    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.blocked.contains_key(id)
    }

    pub fn total(&self) -> usize {
        self.all.len()
    }
}
```

- [ ] **Step 2: Wire `Data` into `app/mod.rs`**

At the top of `app/mod.rs` add the module + import:

```rust
mod data;
pub use data::Data;
```

Remove the now-moved `use` items that are no longer needed directly in `mod.rs` only if unused (the compiler will tell you). Replace the six fields in `struct App` with a single field:

```rust
    pub data: Data,
```

In `App::new`, replace the six field initializers (`store`, `all`, `ready`, `blocked`, `graph`, `load_warnings`) with:

```rust
            data: Data::new(store),
```

- [ ] **Step 3: Rewrite `App::refresh` to delegate the scan to `Data`**

Replace the scan/sort/ready/blocked block at the top of `refresh` (everything that produced `frontmatters`, `graph`, `ready`, `blocked`, `all`, `load_warnings`) with:

```rust
        if let Err(msg) = self.data.scan() {
            self.status = msg;
            return;
        }
```

Then update the remaining body of `refresh` to read `self.data.all` / `self.data.load_warnings` and keep the existing calls (`rebuild_facets`, `recompute_view`, `load_detail`, status line):

```rust
        self.rebuild_facets();
        self.recompute_view();
        self.load_detail();
        self.status = format!(
            "{} item(s) loaded{}",
            self.data.all.len(),
            if self.data.load_warnings.is_empty() {
                String::new()
            } else {
                format!(" · {} warning(s)", self.data.load_warnings.len())
            }
        );
```

- [ ] **Step 4: Rewrite every other `Data`-field reference in `app/mod.rs`**

Apply the mapping (`self.store`→`self.data.store`, `self.all`→`self.data.all`, `self.ready`→`self.data.ready`, `self.blocked`→`self.data.blocked`, `self.graph`→`self.data.graph`, `self.load_warnings`→`self.data.load_warnings`) across the remaining methods: `recompute_view`, `apply_sort`, `restore_selection`, `rebuild_facets`, `visible`, `visible_count`, `total_count`, `visible_for`, `is_ready`, `is_blocked`, `selected_frontmatter`, `load_detail`. Where a method body becomes a one-line delegate, simplify it, e.g.:

```rust
    pub fn total_count(&self) -> usize {
        self.data.total()
    }

    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.data.is_ready(id)
    }

    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.data.is_blocked(id)
    }
```

- [ ] **Step 5: Fix the one `ui.rs` site**

In `crates/clove-tui/src/ui.rs`, change `app.load_warnings` → `app.data.load_warnings` (the `render_status` function).

- [ ] **Step 6: Build, test (snapshots must be unchanged), commit**

Run: `cargo test -p clove-tui`
Expected: PASS, zero snapshot changes.

```bash
cargo fmt
git add -A
git commit -m "refactor(tui): extract Data sub-struct (store + scan + graph state)"
```

---

## Task 2: Extract the `Listing` sub-struct

**Files:**
- Create: `crates/clove-tui/src/app/listing.rs`
- Modify: `crates/clove-tui/src/app/mod.rs`
- Modify: `crates/clove-tui/src/ui.rs` (sites: `app.tab`, `app.search`, `app.filter`, `app.sort`)
- Modify: `crates/clove-tui/src/snapshot.rs` (sites: `app.sort`)

- [ ] **Step 1: Create `app/listing.rs`**

Move the `Tab`, `SortField`, `SortDir`, `Sort`, `ViewFilter` type definitions verbatim from `app/mod.rs` into this file (keep their `impl` blocks). Add the `Listing` struct and the file header:

```rust
//! The filtered/sorted projection over [`super::Data`]: which items are shown,
//! in what order, and the list cursor. Cohesive state for a future lock.

use clove_core::{ItemFrontmatter, ItemStatus, ItemType};
use ratatui::widgets::ListState;
```

(Move the existing `Tab`/`SortField`/`SortDir`/`Sort`/`ViewFilter` definitions here — they already carry their doc comments and `impl`s. `ViewFilter::matches`/`is_active` move with it unchanged.)

Then add:

```rust
/// The list view: tab partition + active sort/filter/search + the cursor.
pub struct Listing {
    pub tab: Tab,
    /// Indices into `Data::all` that pass the current tab + facet + search filter.
    pub view: Vec<usize>,
    pub list_state: ListState,
    pub sort: Sort,
    pub filter: ViewFilter,
    pub search: String,
}

impl Default for Listing {
    fn default() -> Self {
        Listing {
            tab: Tab::All,
            view: Vec::new(),
            list_state: ListState::default(),
            sort: Sort::default(),
            filter: ViewFilter::default(),
            search: String::new(),
        }
    }
}
```

- [ ] **Step 2: Wire `Listing` into `app/mod.rs`**

Add near the other module decls:

```rust
mod listing;
pub use listing::{Listing, Sort, SortDir, SortField, Tab, ViewFilter};
```

Delete the `Tab`/`SortField`/`SortDir`/`Sort`/`ViewFilter` definitions that you moved out of `mod.rs`. Replace the six fields (`tab`, `view`, `list_state`, `sort`, `filter`, `search`) in `struct App` with:

```rust
    pub list: Listing,
```

In `App::new`, replace those six initializers with:

```rust
            list: Listing::default(),
```

- [ ] **Step 3: Rewrite `Listing`-field references in `app/mod.rs`**

Apply the mapping (`self.tab`→`self.list.tab`, `self.view`→`self.list.view`, `self.list_state`→`self.list.list_state`, `self.sort`→`self.list.sort`, `self.filter`→`self.list.filter`, `self.search`→`self.list.search`) across: `recompute_view`, `apply_sort`, `restore_selection`, `clamp_selection`, `visible`, `visible_count`, `selected_frontmatter`, `selected_id`, `set_tab`, `next_tab`, all `select_*`, all search methods, `cycle_sort_field`, `toggle_sort_dir`, `filter_toggle`, `clear_filters`, `is_menu_selected`.

- [ ] **Step 4: Fix `ui.rs` and `snapshot.rs` sites**

In `ui.rs`: `app.tab` → `app.list.tab`; `app.search` → `app.list.search`; `app.filter` → `app.list.filter`; `app.sort` → `app.list.sort`.
In `snapshot.rs`: `app.sort` → `app.list.sort`.

- [ ] **Step 5: Build, test, commit**

Run: `cargo test -p clove-tui`
Expected: PASS, zero snapshot changes.

```bash
cargo fmt
git add -A
git commit -m "refactor(tui): extract Listing sub-struct (tab/view/sort/filter/search)"
```

---

## Task 3: Extract the `DetailPane` sub-struct

**Files:**
- Create: `crates/clove-tui/src/app/detail.rs`
- Modify: `crates/clove-tui/src/app/mod.rs`
- Modify: `crates/clove-tui/src/ui.rs` (sites: `app.detail`, `app.detail_tab`, `app.detail_scroll`)
- Modify: `crates/clove-tui/src/snapshot.rs` (sites: `app.detail_scroll`)

- [ ] **Step 1: Create `app/detail.rs`**

Move the `Detail` struct and the `DetailTab` enum (with its `impl`) verbatim from `app/mod.rs` into this file. Add header + the `DetailPane` struct:

```rust
//! The detail pane: which sub-view is active, the loaded per-selection data, and
//! the scroll offset. `DetailPane.detail` is the loaded [`Detail`] (so the field
//! path is `app.detail.detail`).

use clove_core::{ChildrenSummary, CloveId, Comment, DepTreeNode, Item};

// (Move the existing `Detail` struct and `DetailTab` enum here, unchanged.)

pub struct DetailPane {
    pub detail_tab: DetailTab,
    pub detail: Option<Detail>,
    pub detail_scroll: u16,
}

impl Default for DetailPane {
    fn default() -> Self {
        DetailPane {
            detail_tab: DetailTab::Overview,
            detail: None,
            detail_scroll: 0,
        }
    }
}
```

- [ ] **Step 2: Wire into `app/mod.rs`**

```rust
mod detail;
pub use detail::{Detail, DetailPane, DetailTab};
```

Delete the moved `Detail`/`DetailTab` definitions from `mod.rs`. Replace the three fields (`detail_tab`, `detail`, `detail_scroll`) in `struct App` with:

```rust
    pub detail: DetailPane,
```

In `App::new`, replace those three initializers with:

```rust
            detail: DetailPane::default(),
```

- [ ] **Step 3: Rewrite `DetailPane`-field references in `app/mod.rs`**

Apply (`self.detail_tab`→`self.detail.detail_tab`, the `Option<Detail>` `self.detail`→`self.detail.detail`, `self.detail_scroll`→`self.detail.detail_scroll`) across: `on_selection_changed`, `set_detail_tab`, `scroll_detail_down`, `scroll_detail_up`, `load_detail`. In `load_detail`, the assignment becomes `self.detail.detail = Some(Detail { … })` and the early-return becomes `self.detail.detail = None;`.

- [ ] **Step 4: Fix `ui.rs` and `snapshot.rs` sites**

In `ui.rs`: `app.detail` (the `Option<Detail>`) → `app.detail.detail`; `app.detail_tab` → `app.detail.detail_tab`; `app.detail_scroll` → `app.detail.detail_scroll`.
In `snapshot.rs`: `app.detail_scroll` → `app.detail.detail_scroll`.

- [ ] **Step 5: Fix the `app/mod.rs` test that touches the field**

In the `tests` module, the line `let detail = app.detail.as_ref().expect("blocked item has detail");` becomes:

```rust
        let detail = app.detail.detail.as_ref().expect("blocked item has detail");
```

- [ ] **Step 6: Build, test, commit**

Run: `cargo test -p clove-tui`
Expected: PASS, zero snapshot changes.

```bash
cargo fmt
git add -A
git commit -m "refactor(tui): extract DetailPane sub-struct (detail tab/data/scroll)"
```

---

## Task 4: Extract the `FilterMenu` sub-struct

**Files:**
- Create: `crates/clove-tui/src/app/filter_menu.rs`
- Modify: `crates/clove-tui/src/app/mod.rs`
- Modify: `crates/clove-tui/src/ui.rs` (sites: `app.filter_menu`, `app.filter_cursor`)
- Modify: `crates/clove-tui/src/snapshot.rs` (sites: `app.filter_menu`, `app.filter_cursor`)

- [ ] **Step 1: Create `app/filter_menu.rs`**

Move the `Facet` enum (with `impl`), `MenuItem` struct, `MenuValue` enum, and the `toggle_vec` free function verbatim from `app/mod.rs` into this file. Add header + the `FilterMenu` struct:

```rust
//! The facet filter menu: the selectable rows (built from values present in the
//! repo) and the cursor into them. The *active* filter lives on `Listing`.

use clove_core::{ItemStatus, ItemType};

// (Move `Facet`, `MenuItem`, `MenuValue`, and `toggle_vec` here, unchanged.)

#[derive(Default)]
pub struct FilterMenu {
    pub menu: Vec<MenuItem>,
    pub cursor: usize,
}
```

- [ ] **Step 2: Wire into `app/mod.rs`**

```rust
mod filter_menu;
pub use filter_menu::{Facet, FilterMenu, MenuItem, MenuValue};
use filter_menu::toggle_vec;
```

Delete the moved `Facet`/`MenuItem`/`MenuValue`/`toggle_vec` definitions from `mod.rs`. Replace the two fields (`filter_menu`, `filter_cursor`) in `struct App` with:

```rust
    pub filter_menu: FilterMenu,
```

In `App::new`, replace those two initializers with:

```rust
            filter_menu: FilterMenu::default(),
```

- [ ] **Step 3: Rewrite `FilterMenu`-field references in `app/mod.rs`**

Apply (`self.filter_menu`→`self.filter_menu.menu`, `self.filter_cursor`→`self.filter_menu.cursor`) across: `rebuild_facets`, `start_filter`, `filter_move`, `is_menu_selected`, `filter_toggle`. Note `toggle_vec` is now imported (Step 2) so its call sites in `filter_toggle` are unchanged.

- [ ] **Step 4: Fix `ui.rs` and `snapshot.rs` sites**

In `ui.rs`: `app.filter_menu` → `app.filter_menu.menu`; `app.filter_cursor` → `app.filter_menu.cursor`.
In `snapshot.rs`: `app.filter_menu` → `app.filter_menu.menu`; `app.filter_cursor` → `app.filter_menu.cursor`.

- [ ] **Step 5: Build, test, commit**

Run: `cargo test -p clove-tui`
Expected: PASS, zero snapshot changes.

```bash
cargo fmt
git add -A
git commit -m "refactor(tui): extract FilterMenu sub-struct (menu rows + cursor)"
```

---

## Task 5: Split `ui.rs` into the `ui/` module tree

**Files:**
- Create: `crates/clove-tui/src/ui/mod.rs` and the modules below
- Delete: `crates/clove-tui/src/ui.rs`

Module destinations (move each function verbatim; change top-level `fn` to `pub(crate) fn` so siblings can call it; `ui::render` stays `pub`):

| Module                    | Functions |
|---------------------------|-----------|
| `ui/mod.rs`               | `render` (pub), `render_body`, `pick_layout`, `BodyLayout`, `render_too_small`; `mod` decls + `use` re-exports |
| `ui/util.rs`              | `render_rule`, `right_align`, `border_style`, `kv`, `field_line`, `truncate`, `centered_fixed`, `short_id`, `short_ref`, `join_ids`, `push_id_field` |
| `ui/style.rs`             | `status_glyph`, `status_style`, `type_style`, `type_icon`, `priority_style`, `priority_glyph` + the colour-semantic `tests` module (moved from `ui.rs`) |
| `ui/tabs.rs`              | `render_tabs`, `short_tab` |
| `ui/list.rs`              | `render_list`, `list_row` |
| `ui/detail/mod.rs`        | `render_detail`, `detail_title`, `head_spans`; `mod overview/tree/comments` decls |
| `ui/detail/overview.rs`   | `overview_header`, `overview_body`, `overview_lines`, `title_span`, `assignee_deps_spans`, `blocker_lines`, `relation_lines`, `status_spans`, `footer_line`, `fit_labels`, `time_field` |
| `ui/detail/tree.rs`       | `tree_lines`, `push_tree_node` |
| `ui/detail/comments.rs`   | `comment_lines` |
| `ui/status.rs`            | `render_status`, `filter_summary` |
| `ui/help.rs`              | `render_help` |
| `ui/filter_menu.rs`       | `render_filter_menu` |

- [ ] **Step 1: Create the directory and `ui/mod.rs`**

```bash
cd crates/clove-tui/src
mkdir -p ui/detail
git rm ui.rs   # after you have copied its contents out; or move-then-edit
```

`ui/mod.rs` declares the modules and holds the orchestrators:

```rust
//! Rendering. `render` is the only public entry; everything else is `pub(crate)`
//! and split per-component / per-page.

mod detail;
mod filter_menu;
mod help;
mod list;
mod status;
mod style;
mod tabs;
mod util;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::App;

// (Move `render`, `render_body`, `pick_layout`, `BodyLayout`, `render_too_small`
//  here. `render` stays `pub`; the rest become `pub(crate)` if called across
//  modules, otherwise private.)
```

- [ ] **Step 2: Create each leaf module**

Move the functions listed in the table into their module files. At the top of each module add the imports it needs (the compiler errors will list missing symbols — typical needs: `ratatui::text::{Line, Span}`, `ratatui::style::{Style, Color, Modifier}`, `ratatui::widgets::*`, `ratatui::layout::*`, `ratatui::Frame`, `crate::app::{App, …}`, `crate::markdown`, and `use super::{style, util}` / `use crate::ui::{style, util}` for cross-module helper calls). Cross-module calls become `style::priority_style(…)`, `util::truncate(…)`, etc.

- [ ] **Step 3: Move the colour-semantic tests into `ui/style.rs`**

Move the `#[cfg(test)] mod tests` block that asserts `priority_style`/`status_style`/`type_style` from the old `ui.rs` into `ui/style.rs` (it sits next to the functions it tests).

- [ ] **Step 4: Build, fix visibility, test**

Run: `cargo build -p clove-tui`
Expected: a list of privacy/import errors on first pass — resolve by adding `pub(crate)` to cross-called fns and the missing `use` lines, until it builds clean.

Run: `cargo test -p clove-tui`
Expected: PASS, **zero snapshot changes** (the render output is identical).

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -A
git commit -m "refactor(tui): split ui.rs into per-component/per-page ui/ modules"
```

---

## Task 6: Add the tick-loop hooks to `App` (TDD)

**Files:**
- Modify: `crates/clove-tui/src/app/mod.rs` (add `busy` field, `is_busy`, `tick_interval`, `on_tick`, and a test)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `app/mod.rs`:

```rust
#[test]
fn tick_interval_reflects_busy_state() {
    use std::time::Duration;
    let (_dir, store) = fixture();
    let mut app = App::new(store);
    // Idle: 1 fps.
    assert_eq!(app.tick_interval(), Duration::from_secs(1));
    // Busy: 10 fps (the hook the deferred background scan will flip).
    app.busy = true;
    assert_eq!(app.tick_interval(), Duration::from_millis(100));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p clove-tui tick_interval_reflects_busy_state`
Expected: FAIL — `no field 'busy'` / `no method 'tick_interval'`.

- [ ] **Step 3: Implement the hooks**

Add a field to `struct App`:

```rust
    /// Whether a background operation is in progress. Always `false` today; the
    /// deferred M4 background scan flips this to drive the 10fps cadence.
    busy: bool,
```

Initialize it in `App::new` (`busy: false,`). Add the methods in the `impl App` block:

```rust
    /// Whether a background operation is in progress (hook for the deferred
    /// background scan; always `false` today).
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// The event-loop poll timeout: 10fps while busy, 1fps when idle.
    pub fn tick_interval(&self) -> std::time::Duration {
        if self.is_busy() {
            std::time::Duration::from_millis(100)
        } else {
            std::time::Duration::from_secs(1)
        }
    }

    /// Advance one idle/progress tick. A no-op today (future: spinner frame).
    pub fn on_tick(&mut self) {}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p clove-tui tick_interval_reflects_busy_state`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -A
git commit -m "feat(tui): add is_busy/tick_interval/on_tick cadence hooks"
```

---

## Task 7: Convert the event loop to tick-driven polling

**Files:**
- Modify: `crates/clove-tui/src/lib.rs` (`event_loop`)

- [ ] **Step 1: Update imports in `lib.rs`**

Ensure the event imports include `poll` and `Resize`:

```rust
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
```

(`event::poll` is reached through `event::`; `Event::Resize` is a variant of the already-imported `Event` — no new import needed.)

- [ ] **Step 2: Replace `event_loop` with the polling version**

```rust
fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    // Initial frame.
    terminal.draw(|f| ui::render(f, app))?;

    while !app.should_quit {
        // Cadence: 1fps idle, 10fps while busy. An input arriving before the
        // timeout wakes us immediately.
        if event::poll(app.tick_interval())? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(app, key.code, key.modifiers);
                    // Always redraw after handling an event.
                    terminal.draw(|f| ui::render(f, app))?;
                }
                Event::Resize(_, _) => {
                    terminal.draw(|f| ui::render(f, app))?;
                }
                _ => {}
            }
        } else {
            // Timeout elapsed: advance a tick and redraw (keeps the frame live;
            // animates progress at 10fps once a background op sets `busy`).
            app.on_tick();
            terminal.draw(|f| ui::render(f, app))?;
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p clove-tui`
Expected: PASS (the loop itself isn't unit-tested; `tick_interval` is covered by Task 6 and the smoke/snapshot tests still pass).

- [ ] **Step 4: Manual smoke check (optional but recommended)**

Run the TUI against this repo's own store and confirm keys respond and `q` quits:

Run: `cargo run -p clove -- tui`
Expected: browser opens, navigation works, `q` exits cleanly. (Idle redraws are invisible since content is static today.)

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -A
git commit -m "feat(tui): tick-driven event loop (1fps idle / redraw-on-event / 10fps busy)"
```

---

## Task 8: Update docs

**Files:**
- Modify: `CLAUDE.md` (colour-test location)
- Modify: `HANDOFF.md`
- Modify: `docs/IMPLEMENTATION_PLAN.md`

- [ ] **Step 1: Update `CLAUDE.md` colour-test pointer**

Find the sentence in the "Validating colour" section that says the colour semantics are locked by unit tests "in `ui.rs` (`tests` module: `priority_style`, `status_style`, `type_style`)" and change the location to `ui/style.rs` (`tests` module). Also update the nearby line "When you change a colour constant, update those tests" if it names `ui.rs`.

- [ ] **Step 2: Add a module-layout note to `HANDOFF.md`**

Under the M4 / `clove tui` section, add a short paragraph: `clove-tui` is now split into `app/{mod,data,listing,detail,filter_menu}.rs` (state regrouped into `Data`/`Listing`/`DetailPane`/`FilterMenu` sub-structs, command methods stay on `App`) and `ui/{mod,util,style,tabs,list,status,help,filter_menu}.rs` + `ui/detail/{mod,overview,tree,comments}.rs`. The event loop is tick-driven (1fps idle / redraw-after-event / 10fps when `App::is_busy()`); the background scan that flips `is_busy()` is the deferred next M4 step (concurrent model + fine-grained locks per sub-struct).

- [ ] **Step 3: Mirror the deferred-work note in `docs/IMPLEMENTATION_PLAN.md`**

In the M4 backlog section, add a bullet under T-U01: the read-only TUI has been modularized (per-component/per-page modules + `Data`/`Listing`/`DetailPane`/`FilterMenu` sub-structs) and given a tick-driven loop; the next step is the concurrent TUI model — move the wholesale re-scan onto a background worker behind per-sub-struct locks and drive the 10fps `is_busy()` cadence with a spinner.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md HANDOFF.md docs/IMPLEMENTATION_PLAN.md
git commit -m "docs(tui): record modular layout + tick loop; note deferred concurrent model"
```

---

## Task 9: Final quality gate

- [ ] **Step 1: Run the full gate**

Run:
```bash
cargo fmt --check
cargo clippy --all-targets -p clove-tui -- -D warnings
cargo test --workspace
```
Expected: all clean; **no snapshot changes** anywhere across Tasks 1–5.

- [ ] **Step 2: If clippy flags anything introduced by the split**

Typical post-split clippy nits: `module_inception` (a `mod detail` containing `detail`-named items is fine; only the module/struct-name clash triggers it — rename or `#[allow]` with a comment only if it actually fires), or `needless `pub(crate)``. Fix minimally and re-run the gate. Do not silence warnings wholesale.

- [ ] **Step 3: Final confirmation**

The branch now contains: `app/` split into five files with four state sub-structs, `ui/` split into the per-component/per-page tree, a tick-driven loop with provisioned `is_busy`/`tick_interval`/`on_tick`, and updated docs — with every insta snapshot byte-identical to `master`.

---

## Self-review notes

- **Spec coverage:** App sub-structs (Tasks 1–4), `ui/` per-component+per-page split with style tests relocated (Task 5), tick loop with `is_busy`/`tick_interval`/`on_tick` and 1fps/event/10fps cadence (Tasks 6–7), CLAUDE.md + HANDOFF + plan docs (Task 8), pure-refactor proof via unchanged snapshots + final gate (Task 9). All spec sections mapped.
- **Deferred items** (background scan, threads, locks, spinner) are intentionally NOT implemented — provisioned via `busy`/`is_busy`/`on_tick` only, per the spec non-goals.
- **Type consistency:** field paths (`data.*`, `list.*`, `detail.detail`, `filter_menu.menu`/`.cursor`) are used identically in the mapping table, the app rewrites, and the ui/snapshot fix steps. Re-exports from `app/mod.rs` keep `app::Tab`/`app::DetailTab`/`app::Mode` paths stable for `lib.rs`.
