//! TUI application state and the read-only data layer.
//!
//! All data comes from the file store (the source of truth): a fresh
//! `scan_frontmatter` + `GraphStore::build` on launch and on every manual
//! refresh. This keeps the TUI always-correct and decoupled from the optional
//! SQLite index and daemon — it never mutates anything.

mod data;
pub use data::Data;

mod detail;
pub use detail::{Detail, DetailPane, DetailTab};

mod filter_menu;
use filter_menu::toggle_vec;
pub use filter_menu::{Facet, FilterMenu, MenuItem, MenuValue};

mod form;
pub use form::{Field, FormMode, FormState};

mod listing;
pub use listing::{Listing, SortDir, SortField, Tab, ViewFilter};

use chrono::Utc;
use clove_core::ItemStore;
use clove_types::{CloveId, ItemFrontmatter, ItemStatus, ItemType};

/// Input mode: browsing, typing a search query, the facet filter menu, or the
/// add/edit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Search,
    Filter,
    Form,
}

/// Which pane holds focus. Only visible in the single-pane (narrow) layout,
/// where it decides which pane is shown; in side-by-side / stacked layouts both
/// panes render and focus just marks the active border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

/// The TUI application state.
pub struct App {
    pub data: Data,
    pub list: Listing,

    // View state.
    pub mode: Mode,
    pub detail: DetailPane,
    pub focus: Focus,
    pub show_help: bool,
    pub status: String,
    pub should_quit: bool,
    /// Whether a background operation is in progress. Always `false` today; the
    /// deferred M4 background scan flips this to drive the 10fps cadence.
    busy: bool,

    // Filter menu state.
    pub filter_menu: FilterMenu,

    // Add/edit form state + the config needed to create items.
    pub form: FormState,
    id_prefix: String,
    default_type: ItemType,
}

impl App {
    /// Build the app from a file store, performing the initial scan.
    pub fn new(store: ItemStore) -> Self {
        let mut app = App {
            data: Data::new(store),
            list: Listing::default(),
            mode: Mode::Browse,
            detail: DetailPane::default(),
            focus: Focus::List,
            show_help: false,
            status: String::new(),
            should_quit: false,
            busy: false,
            filter_menu: FilterMenu::default(),
            form: FormState::default(),
            id_prefix: "proj".to_owned(),
            default_type: ItemType::Feature,
        };
        app.refresh();
        app
    }

    /// Set the id prefix + default type used when creating items (from config).
    pub fn with_config(mut self, id_prefix: String, default_type: ItemType) -> Self {
        self.id_prefix = id_prefix;
        self.default_type = default_type;
        self
    }

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

    /// Re-scan the store and rebuild all derived state, preserving the selected
    /// item where possible.
    pub fn refresh(&mut self) {
        if let Err(msg) = self.data.scan() {
            self.status = msg;
            return;
        }

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
    }

    /// Recompute the view indices from the current tab + facet filters + search,
    /// apply the sort, and restore the selection to the same item where possible.
    ///
    /// The pipeline mirrors `clove`'s semantics: tab partition (graph-derived) AND
    /// facet filters AND substring search, then sort. `self.all` stays in the
    /// canonical `(priority, topo-rank, id)` order; only `self.view` is re-sorted.
    fn recompute_view(&mut self) {
        let keep = self.selected_id();
        let needle = self.list.search.to_lowercase();

        self.list.view = self
            .data
            .all
            .iter()
            .enumerate()
            .filter(|(_, fm)| match self.list.tab {
                Tab::All => true,
                Tab::Ready => self.data.ready.contains(&fm.id),
                Tab::Blocked => self.data.blocked.contains_key(&fm.id),
            })
            .filter(|(_, fm)| self.list.filter.matches(fm))
            .filter(|(_, fm)| {
                if needle.is_empty() {
                    return true;
                }
                fm.id.as_str().to_lowercase().contains(&needle)
                    || fm.title.to_lowercase().contains(&needle)
                    || fm.labels.iter().any(|l| l.to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();

        self.apply_sort();
        self.restore_selection(keep);
    }

    /// Re-order `self.view` by the active sort. `Default` keeps the canonical
    /// order `self.all` is already in; other fields sort flatly with an `id`
    /// tiebreak for determinism.
    fn apply_sort(&mut self) {
        if self.list.sort.field == SortField::Default {
            return;
        }
        let all = &self.data.all;
        let field = self.list.sort.field;
        let dir = self.list.sort.dir;
        self.list.view.sort_by(|&a, &b| {
            let (x, y) = (&all[a], &all[b]);
            let ord = match field {
                SortField::Priority => x.priority.cmp(&y.priority),
                SortField::Created => x.created.cmp(&y.created),
                SortField::Updated => x.updated.cmp(&y.updated),
                SortField::Id => x.id.cmp(&y.id),
                SortField::Default => std::cmp::Ordering::Equal,
            }
            .then_with(|| x.id.cmp(&y.id));
            match dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });
    }

    /// Restore the list selection to item `keep` if it is still in the view;
    /// otherwise clamp to a valid position.
    fn restore_selection(&mut self, keep: Option<CloveId>) {
        if let Some(id) = keep {
            if let Some(pos) = self
                .list
                .view
                .iter()
                .position(|&i| self.data.all[i].id == id)
            {
                self.list.list_state.select(Some(pos));
            }
        }
        self.clamp_selection();
    }

    /// Rebuild the filter-menu candidate values from the values actually present
    /// in the repo, in a deterministic (sorted) order.
    fn rebuild_facets(&mut self) {
        use std::collections::BTreeSet;

        let mut statuses = Vec::new();
        for s in [ItemStatus::Open, ItemStatus::InProgress, ItemStatus::Closed] {
            if self.data.all.iter().any(|fm| fm.status == s) {
                statuses.push(MenuValue::Status(s));
            }
        }
        let mut types = Vec::new();
        for t in [
            ItemType::Bug,
            ItemType::Feature,
            ItemType::Chore,
            ItemType::Docs,
            ItemType::Epic,
        ] {
            if self.data.all.iter().any(|fm| fm.item_type == t) {
                types.push(MenuValue::Type(t));
            }
        }
        let mut priorities: Vec<u8> = self.data.all.iter().map(|fm| fm.priority.get()).collect();
        priorities.sort_unstable();
        priorities.dedup();
        let labels: BTreeSet<String> = self
            .data
            .all
            .iter()
            .flat_map(|fm| fm.labels.iter().cloned())
            .collect();
        let assignees: BTreeSet<String> = self
            .data
            .all
            .iter()
            .filter_map(|fm| fm.assignee.clone())
            .collect();

        let mut menu = Vec::new();
        for v in statuses {
            if let MenuValue::Status(s) = &v {
                menu.push(MenuItem {
                    facet: Facet::Status,
                    text: s.as_str().to_owned(),
                    value: v.clone(),
                });
            }
        }
        for v in types {
            if let MenuValue::Type(t) = &v {
                menu.push(MenuItem {
                    facet: Facet::Type,
                    text: t.as_str().to_owned(),
                    value: v.clone(),
                });
            }
        }
        for p in priorities {
            menu.push(MenuItem {
                facet: Facet::Priority,
                text: format!("p{p}"),
                value: MenuValue::Priority(p),
            });
        }
        for l in labels {
            menu.push(MenuItem {
                facet: Facet::Label,
                text: l.clone(),
                value: MenuValue::Label(l),
            });
        }
        for a in assignees {
            menu.push(MenuItem {
                facet: Facet::Assignee,
                text: a.clone(),
                value: MenuValue::Assignee(a),
            });
        }
        self.filter_menu.menu = menu;
        if self.filter_menu.cursor >= self.filter_menu.menu.len() {
            self.filter_menu.cursor = 0;
        }
    }

    /// Keep the list selection within the current view bounds.
    fn clamp_selection(&mut self) {
        if self.list.view.is_empty() {
            self.list.list_state.select(None);
        } else {
            let sel = self
                .list
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.list.view.len() - 1);
            self.list.list_state.select(Some(sel));
        }
    }

    /// The frontmatter rows in the current (filtered, ordered) view.
    pub fn visible(&self) -> impl Iterator<Item = &ItemFrontmatter> {
        self.list.view.iter().map(move |&i| &self.data.all[i])
    }

    pub fn visible_count(&self) -> usize {
        self.list.view.len()
    }

    pub fn total_count(&self) -> usize {
        self.data.total()
    }

    /// Count of items belonging to `tab` (ignoring the active search), for the
    /// tab-bar badges.
    pub fn visible_for(&self, tab: Tab) -> usize {
        match tab {
            Tab::All => self.data.all.len(),
            Tab::Ready => self
                .data
                .all
                .iter()
                .filter(|fm| self.data.ready.contains(&fm.id))
                .count(),
            Tab::Blocked => self
                .data
                .all
                .iter()
                .filter(|fm| self.data.blocked.contains_key(&fm.id))
                .count(),
        }
    }

    /// Whether an item is ready / blocked (for badges in the list).
    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.data.is_ready(id)
    }

    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.data.is_blocked(id)
    }

    /// The currently-selected item's frontmatter, if any.
    pub fn selected_frontmatter(&self) -> Option<&ItemFrontmatter> {
        let pos = self.list.list_state.selected()?;
        let &idx = self.list.view.get(pos)?;
        self.data.all.get(idx)
    }

    fn selected_id(&self) -> Option<CloveId> {
        self.selected_frontmatter().map(|fm| fm.id.clone())
    }

    // --- Navigation -------------------------------------------------------

    pub fn select_next(&mut self) {
        if self.list.view.is_empty() {
            return;
        }
        let next = match self.list.list_state.selected() {
            Some(i) if i + 1 < self.list.view.len() => i + 1,
            Some(i) => i,
            None => 0,
        };
        self.list.list_state.select(Some(next));
        self.on_selection_changed();
    }

    pub fn select_prev(&mut self) {
        if self.list.view.is_empty() {
            return;
        }
        let prev = self
            .list
            .list_state
            .selected()
            .unwrap_or(0)
            .saturating_sub(1);
        self.list.list_state.select(Some(prev));
        self.on_selection_changed();
    }

    pub fn select_first(&mut self) {
        if !self.list.view.is_empty() {
            self.list.list_state.select(Some(0));
            self.on_selection_changed();
        }
    }

    pub fn select_last(&mut self) {
        if !self.list.view.is_empty() {
            self.list.list_state.select(Some(self.list.view.len() - 1));
            self.on_selection_changed();
        }
    }

    fn on_selection_changed(&mut self) {
        self.detail.detail_scroll = 0;
        self.load_detail();
    }

    // --- Tabs / detail views ---------------------------------------------

    pub fn set_tab(&mut self, tab: Tab) {
        if self.list.tab != tab {
            self.list.tab = tab;
            self.recompute_view();
            self.on_selection_changed();
        }
    }

    pub fn next_tab(&mut self) {
        let next = (self.list.tab.index() + 1) % Tab::ALL.len();
        self.set_tab(Tab::ALL[next]);
    }

    pub fn set_detail_tab(&mut self, tab: DetailTab) {
        if self.detail.detail_tab != tab {
            self.detail.detail_tab = tab;
            self.detail.detail_scroll = 0;
        }
    }

    pub fn scroll_detail_down(&mut self) {
        self.detail.detail_scroll = self.detail.detail_scroll.saturating_add(3);
    }

    pub fn scroll_detail_up(&mut self) {
        self.detail.detail_scroll = self.detail.detail_scroll.saturating_sub(3);
    }

    /// Focus the detail pane (shows it in the single-pane narrow layout).
    pub fn focus_detail(&mut self) {
        self.focus = Focus::Detail;
    }

    /// Focus the list pane.
    pub fn focus_list(&mut self) {
        self.focus = Focus::List;
    }

    // --- Search -----------------------------------------------------------

    pub fn start_search(&mut self) {
        self.mode = Mode::Search;
    }

    pub fn push_search(&mut self, c: char) {
        self.list.search.push(c);
        self.recompute_view();
        self.on_selection_changed();
    }

    pub fn pop_search(&mut self) {
        self.list.search.pop();
        self.recompute_view();
        self.on_selection_changed();
    }

    /// Commit the current search and return to browse mode.
    pub fn commit_search(&mut self) {
        self.mode = Mode::Browse;
    }

    /// Cancel search: clear the query and return to browse mode.
    pub fn cancel_search(&mut self) {
        self.mode = Mode::Browse;
        if !self.list.search.is_empty() {
            self.list.search.clear();
            self.recompute_view();
            self.on_selection_changed();
        }
    }

    // --- Sort -------------------------------------------------------------

    /// Advance the sort field through its cycle (rank → priority → … → id).
    pub fn cycle_sort_field(&mut self) {
        self.list.sort.field = self.list.sort.field.next();
        // Sensible default direction per field: recency descends, others ascend.
        self.list.sort.dir = match self.list.sort.field {
            SortField::Created | SortField::Updated => SortDir::Desc,
            _ => SortDir::Asc,
        };
        self.recompute_view();
        self.on_selection_changed();
    }

    /// Toggle the sort direction.
    pub fn toggle_sort_dir(&mut self) {
        self.list.sort.dir = match self.list.sort.dir {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        };
        self.recompute_view();
        self.on_selection_changed();
    }

    // --- Filter menu ------------------------------------------------------

    /// Open the facet filter menu.
    pub fn start_filter(&mut self) {
        if self.filter_menu.cursor >= self.filter_menu.menu.len() {
            self.filter_menu.cursor = 0;
        }
        self.mode = Mode::Filter;
    }

    /// Close the filter menu, returning to browse mode.
    pub fn exit_filter(&mut self) {
        self.mode = Mode::Browse;
    }

    /// Move the filter-menu cursor by `delta` (clamped).
    pub fn filter_move(&mut self, delta: i32) {
        if self.filter_menu.menu.is_empty() {
            return;
        }
        let last = self.filter_menu.menu.len() as i32 - 1;
        let next = (self.filter_menu.cursor as i32 + delta).clamp(0, last);
        self.filter_menu.cursor = next as usize;
    }

    /// Whether the menu item at `idx` is currently selected in the filter.
    pub fn is_menu_selected(&self, idx: usize) -> bool {
        let Some(item) = self.filter_menu.menu.get(idx) else {
            return false;
        };
        match &item.value {
            MenuValue::Status(s) => self.list.filter.status == Some(*s),
            MenuValue::Assignee(a) => self.list.filter.assignee.as_deref() == Some(a.as_str()),
            MenuValue::Type(t) => self.list.filter.types.contains(t),
            MenuValue::Priority(p) => self.list.filter.priorities.contains(p),
            MenuValue::Label(l) => self.list.filter.labels.contains(l),
        }
    }

    /// Toggle the value under the cursor in/out of the active filter.
    pub fn filter_toggle(&mut self) {
        let Some(item) = self.filter_menu.menu.get(self.filter_menu.cursor).cloned() else {
            return;
        };
        let on = self.is_menu_selected(self.filter_menu.cursor);
        match item.value {
            // Single-valued: toggling sets or clears the one value (radio).
            MenuValue::Status(s) => self.list.filter.status = if on { None } else { Some(s) },
            MenuValue::Assignee(a) => self.list.filter.assignee = if on { None } else { Some(a) },
            // Multi-valued: toggle membership.
            MenuValue::Type(t) => toggle_vec(&mut self.list.filter.types, t, on),
            MenuValue::Priority(p) => toggle_vec(&mut self.list.filter.priorities, p, on),
            MenuValue::Label(l) => toggle_vec(&mut self.list.filter.labels, l, on),
        }
        self.recompute_view();
        self.on_selection_changed();
    }

    /// Clear all active facet filters (leaves tab, search, and sort intact).
    pub fn clear_filters(&mut self) {
        if self.list.filter.is_active() {
            self.list.filter = ViewFilter::default();
            self.recompute_view();
            self.on_selection_changed();
        }
    }

    // --- Add / edit form --------------------------------------------------

    /// Open a blank "new item" form.
    pub fn start_new(&mut self) {
        self.form = FormState::new_item(self.default_type);
        self.mode = Mode::Form;
    }

    /// Open an "edit" form prefilled from the selected item (no-op if no
    /// selection or the item can't be loaded).
    pub fn start_edit(&mut self) {
        let Some(fm) = self.selected_frontmatter() else {
            return;
        };
        let id = fm.id.clone();
        match self.data.store.get(&id) {
            Ok(item) => {
                self.form = FormState::edit_item(&item);
                self.mode = Mode::Form;
            }
            Err(e) => self.status = format!("failed to load {id}: {e}"),
        }
    }

    /// Close the form without saving.
    pub fn cancel_form(&mut self) {
        self.mode = Mode::Browse;
    }

    pub fn form_next_field(&mut self) {
        self.form.next_field();
    }

    pub fn form_prev_field(&mut self) {
        self.form.prev_field();
    }

    /// Route a key to the focused field: cycle enum values with ←/→, otherwise
    /// edit text (Enter inserts a newline in the body, else advances a field).
    pub fn form_key(&mut self, code: ratatui::crossterm::event::KeyCode) {
        use ratatui::crossterm::event::KeyCode;
        if self.form.focused().is_enum() {
            match code {
                KeyCode::Left => self.form.cycle(-1),
                KeyCode::Right | KeyCode::Char(' ') => self.form.cycle(1),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Char(c) => self.form.insert_char(c),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Enter if self.form.focused() == Field::Body => self.form.newline(),
            KeyCode::Enter => self.form.next_field(),
            _ => {}
        }
    }

    /// Validate + apply the form through the shared `clove_core` write path, then
    /// refresh. On error the form stays open with the message shown.
    pub fn form_submit(&mut self) {
        if self.form.title.trim().is_empty() {
            self.form.error = Some("title cannot be empty".to_owned());
            return;
        }
        let now = Utc::now();
        let result = match self.form.mode {
            FormMode::New => self.submit_new(now),
            FormMode::Edit => self.submit_edit(now),
        };
        match result {
            Ok(status) => {
                self.mode = Mode::Browse;
                self.refresh();
                self.status = status;
            }
            Err(e) => self.form.error = Some(e.to_string()),
        }
    }

    fn submit_new(
        &mut self,
        now: chrono::DateTime<Utc>,
    ) -> Result<String, clove_types::CloveError> {
        let spec = self.form.to_new_spec();
        let out = clove_core::ops::create(
            &self.data.store,
            &self.id_prefix,
            self.default_type,
            spec,
            now,
        )?;
        let id = out["id"].as_str().unwrap_or("item").to_owned();
        Ok(format!("created {id}"))
    }

    fn submit_edit(
        &mut self,
        now: chrono::DateTime<Utc>,
    ) -> Result<String, clove_types::CloveError> {
        let id =
            self.form
                .edit_id
                .clone()
                .ok_or_else(|| clove_types::CloveError::InvalidField {
                    field: "id".to_owned(),
                    reason: "no item to edit".to_owned(),
                })?;
        let store = &self.data.store;

        // Not transactional: the scalar/label/body edit is applied first, then the
        // graph edges. If a later parent/dep op fails (e.g. a typed id that would
        // cycle), the earlier change is already on disk and the form stays open
        // showing the error; resubmitting re-applies idempotently.
        // Scalars / labels / body via the unified structured edit.
        clove_core::apply_edit(store, &id, &self.form.to_edit_request(), now)?;

        // Parent (graph-validated) — only when it changed.
        let new_parent = self.form.parent_id()?;
        let old_parent = self
            .form
            .original
            .as_ref()
            .and_then(|i| i.frontmatter.parent.clone());
        if new_parent != old_parent {
            clove_core::ops::set_parent(store, &id, new_parent.as_ref(), now)?;
        }

        // Deps (graph-validated) — diff add/remove.
        let new_deps = self.form.dep_ids()?;
        let old_deps = self
            .form
            .original
            .as_ref()
            .map(|i| i.frontmatter.deps.clone())
            .unwrap_or_default();
        for dep in &new_deps {
            if !old_deps.contains(dep) {
                clove_core::ops::dep_add(store, &id, dep, now)?;
            }
        }
        for dep in &old_deps {
            if !new_deps.contains(dep) {
                clove_core::ops::dep_remove(store, &id, dep, now)?;
            }
        }
        Ok(format!("saved {id}"))
    }

    // --- Detail loading ---------------------------------------------------

    /// Load the body, comments, dep tree, and block reasons for the selection.
    fn load_detail(&mut self) {
        let Some(fm) = self.selected_frontmatter() else {
            self.detail.detail = None;
            return;
        };
        let id = fm.id.clone();

        let item = match self.data.store.get(&id) {
            Ok(item) => item,
            Err(e) => {
                self.detail.detail = None;
                self.status = format!("failed to load {id}: {e}");
                return;
            }
        };

        let comments =
            clove_core::list_comments(self.data.store.issues_dir(), &id).unwrap_or_default();

        let (blocking_deps, dangling_deps) = self
            .data
            .blocked
            .get(&id)
            .map(|b| (b.blocking_deps.clone(), b.dangling_deps.clone()))
            .unwrap_or_default();

        let children = self.data.graph.epic_children_summary(&id);

        let tree = self.data.graph.dep_tree(&id, 25);

        self.detail.detail = Some(Detail {
            item,
            comments,
            blocking_deps,
            dangling_deps,
            children,
            tree,
        });
    }
}

/// Format a UTC timestamp for display (date + minute precision, local-agnostic).
pub fn fmt_ts(ts: chrono::DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Day-resolution timestamp for the detail pane: month + day, no year/time
/// (e.g. `Jan 20`). `%e` space-pads single digits, so collapse the double space.
pub fn fmt_day(ts: chrono::DateTime<Utc>) -> String {
    ts.format("%b %e").to_string().replace("  ", " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use clove_core::NewItem;
    use clove_types::{ItemType, Priority};

    /// Build a store in a temp dir with a small dependency graph:
    /// `b` (open) ← `a` depends on `b`, plus an independent `c`.
    /// → ready: {b, c}; blocked: {a}.
    fn fixture() -> (tempfile::TempDir, ItemStore) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
        let store = ItemStore::new(root);
        let now = Utc::now();

        let new = |title: &str, deps: Vec<CloveId>| NewItem {
            title: title.to_owned(),
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
            labels: vec!["area:core".to_owned()],
            deps,
            parent: None,
            assignee: None,
            body: format!("body of {title}"),
        };

        let b = store.create("proj", new("Base task", vec![]), now).unwrap();
        store
            .create(
                "proj",
                new("Depends on base", vec![b.frontmatter.id.clone()]),
                now,
            )
            .unwrap();
        store
            .create("proj", new("Independent", vec![]), now)
            .unwrap();
        (dir, store)
    }

    #[test]
    fn loads_and_partitions_ready_blocked() {
        let (_dir, store) = fixture();
        let app = App::new(store);

        assert_eq!(app.total_count(), 3);
        // Two items with no open deps are ready; the dependent one is blocked.
        assert_eq!(app.visible_for(Tab::Ready), 2);
        assert_eq!(app.visible_for(Tab::Blocked), 1);
    }

    #[test]
    fn tab_filters_the_view() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.set_tab(Tab::Blocked);
        assert_eq!(app.visible_count(), 1);
        let blocked = app.selected_frontmatter().unwrap();
        assert_eq!(blocked.title, "Depends on base");

        app.set_tab(Tab::All);
        assert_eq!(app.visible_count(), 3);
    }

    #[test]
    fn search_filters_by_title_and_clears() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.start_search();
        for c in "independent".chars() {
            app.push_search(c);
        }
        assert_eq!(app.visible_count(), 1);
        assert_eq!(app.selected_frontmatter().unwrap().title, "Independent");

        app.cancel_search();
        assert_eq!(app.visible_count(), 3);
    }

    #[test]
    fn detail_loads_body_and_block_reasons() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.set_tab(Tab::Blocked);
        let detail = app.detail.detail.as_ref().expect("blocked item has detail");
        assert!(detail.item.body.contains("Depends on base"));
        // It is blocked by exactly one open dependency.
        assert_eq!(detail.blocking_deps.len(), 1);
        assert!(detail.dangling_deps.is_empty());
        // The dep tree roots at this item with the base task as its one child.
        let tree = detail.tree.as_ref().expect("dep tree present");
        assert_eq!(tree.children.len(), 1);
    }

    #[test]
    fn renders_every_view_without_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (_dir, store) = fixture();
        let mut app = App::new(store);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        for detail in [DetailTab::Overview, DetailTab::Tree, DetailTab::Comments] {
            app.set_detail_tab(detail);
            for tab in Tab::ALL {
                app.set_tab(tab);
                terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
            }
        }

        // Help overlay and search mode also render.
        app.show_help = true;
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        app.show_help = false;
        app.start_search();
        app.push_search('a');
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        app.cancel_search();

        // The add and edit forms also render.
        app.start_new();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        app.cancel_form();
        app.select_first();
        app.start_edit();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    }

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

    #[test]
    fn navigation_clamps_and_moves() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.select_first();
        assert_eq!(app.list.list_state.selected(), Some(0));
        app.select_last();
        assert_eq!(app.list.list_state.selected(), Some(2));
        // Moving past the end stays at the end.
        app.select_next();
        assert_eq!(app.list.list_state.selected(), Some(2));
    }

    // --- Add / edit form --------------------------------------------------

    #[test]
    fn new_form_creates_item_through_store() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.start_new();
        assert_eq!(app.mode, Mode::Form);
        app.form.title = "Brand new".to_owned();
        app.form.item_type = ItemType::Bug;
        app.form.priority = 0;
        app.form.assignee = "carol".to_owned();
        app.form.labels = "area:web, Urgent".to_owned();
        app.form.body = "hello".to_owned();
        app.form_submit();

        assert_eq!(app.mode, Mode::Browse, "form closes on success");
        let created = app
            .data
            .all
            .iter()
            .find(|fm| fm.title == "Brand new")
            .expect("new item scanned in");
        assert_eq!(created.item_type, ItemType::Bug);
        assert_eq!(created.priority.get(), 0);
        assert_eq!(created.assignee.as_deref(), Some("carol"));
        // Labels canonicalized + sorted by the shared path.
        assert_eq!(
            created.labels,
            vec!["area:web".to_owned(), "urgent".to_owned()]
        );
    }

    #[test]
    fn edit_form_applies_scalars_labels_and_body() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);
        app.select_first();
        let id = app.selected_frontmatter().unwrap().id.clone();

        app.start_edit();
        assert_eq!(app.mode, Mode::Form);
        app.form.title = "Renamed".to_owned();
        app.form.status = ItemStatus::Closed;
        app.form.priority = 1;
        app.form.assignee = "dave".to_owned();
        app.form.labels = "x, y".to_owned();
        app.form.body = "edited body".to_owned();
        app.form_submit();

        assert_eq!(app.mode, Mode::Browse);
        let item = app.data.store.get(&id).unwrap();
        assert_eq!(item.frontmatter.title, "Renamed");
        assert_eq!(item.frontmatter.status, ItemStatus::Closed);
        assert!(item.frontmatter.closed.is_some(), "closed timestamp set");
        assert_eq!(item.frontmatter.priority.get(), 1);
        assert_eq!(item.frontmatter.assignee.as_deref(), Some("dave"));
        assert_eq!(
            item.frontmatter.labels,
            vec!["x".to_owned(), "y".to_owned()]
        );
        assert_eq!(item.body, "edited body\n", "body normalized with newline");
    }

    #[test]
    fn edit_form_diffs_parent_and_deps() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        let independent = app
            .data
            .all
            .iter()
            .find(|fm| fm.title == "Independent")
            .unwrap()
            .id
            .clone();
        let base = app
            .data
            .all
            .iter()
            .find(|fm| fm.title == "Base task")
            .unwrap()
            .id
            .clone();

        // Open the edit form directly for the independent item.
        let item = app.data.store.get(&independent).unwrap();
        app.form = FormState::edit_item(&item);
        app.mode = Mode::Form;
        app.form.parent = base.to_string();
        app.form.deps = base.to_string();
        app.form_submit();

        assert_eq!(app.mode, Mode::Browse);
        let updated = app.data.store.get(&independent).unwrap();
        assert_eq!(updated.frontmatter.parent.as_ref(), Some(&base));
        assert_eq!(updated.frontmatter.deps, vec![base.clone()]);
    }

    #[test]
    fn form_navigation_and_enum_cycle_wrap() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);

        app.start_new();
        assert_eq!(app.form.focused(), Field::Title);
        app.form_prev_field(); // wraps to the last field (Body)
        assert_eq!(app.form.focused(), Field::Body);
        app.form_next_field(); // wraps back to Title
        assert_eq!(app.form.focused(), Field::Title);

        // Priority is an enum field at index 2; cycling past 4 wraps to 0.
        app.form.focus = 2;
        app.form.priority = 4;
        app.form.cycle(1);
        assert_eq!(app.form.priority, 0);
    }

    #[test]
    fn empty_title_keeps_form_open_with_error() {
        let (_dir, store) = fixture();
        let mut app = App::new(store);
        app.start_new();
        app.form.title = "   ".to_owned();
        app.form_submit();
        assert_eq!(app.mode, Mode::Form, "stays open on validation error");
        assert!(app.form.error.is_some());
    }
}
