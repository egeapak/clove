//! TUI application state and the read-only data layer.
//!
//! All data comes from the file store (the source of truth): a fresh
//! `scan_frontmatter` + `GraphStore::build` on launch and on every manual
//! refresh. This keeps the TUI always-correct and decoupled from the optional
//! SQLite index and daemon — it never mutates anything.

mod data;
pub use data::Data;

mod listing;
pub use listing::{Listing, SortDir, SortField, Tab, ViewFilter};

use chrono::Utc;
use clove_core::{
    ChildrenSummary, CloveId, Comment, DepTreeNode, Item, ItemFrontmatter, ItemStatus, ItemStore,
    ItemType,
};

/// Which sub-view of the detail pane is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Tree,
    Comments,
}

impl DetailTab {
    pub fn title(self) -> &'static str {
        match self {
            DetailTab::Overview => "Overview",
            DetailTab::Tree => "Dep tree",
            DetailTab::Comments => "Comments",
        }
    }
}

/// Input mode: browsing, typing a search query, or the facet filter menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Search,
    Filter,
}

/// One facet shown in the filter menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Status,
    Type,
    Priority,
    Label,
    Assignee,
}

impl Facet {
    pub fn label(self) -> &'static str {
        match self {
            Facet::Status => "Status",
            Facet::Type => "Type",
            Facet::Priority => "Priority",
            Facet::Label => "Label",
            Facet::Assignee => "Assignee",
        }
    }

    /// Single-valued facets behave as radios (selecting one clears the rest);
    /// multi-valued ones as checkboxes.
    pub fn is_single(self) -> bool {
        matches!(self, Facet::Status | Facet::Assignee)
    }
}

/// One selectable value row in the filter menu.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub facet: Facet,
    pub value: MenuValue,
    /// The display label for the value.
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum MenuValue {
    Status(ItemStatus),
    Type(ItemType),
    Priority(u8),
    Label(String),
    Assignee(String),
}

/// Which pane holds focus. Only visible in the single-pane (narrow) layout,
/// where it decides which pane is shown; in side-by-side / stacked layouts both
/// panes render and focus just marks the active border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

/// Everything loaded for the currently-selected item, computed lazily when the
/// selection changes (the body and comments are not part of the list scan).
pub struct Detail {
    pub item: Item,
    pub comments: Vec<Comment>,
    /// Open hard-dependency targets blocking this item.
    pub blocking_deps: Vec<CloveId>,
    /// Hard-dependency targets with no backing item.
    pub dangling_deps: Vec<CloveId>,
    /// Direct-children roll-up when the item is an epic.
    pub children: Option<ChildrenSummary>,
    /// The dependency tree rooted at this item (ids, titles, status, cycles).
    pub tree: Option<DepTreeNode>,
}

/// The TUI application state.
pub struct App {
    pub data: Data,
    pub list: Listing,

    // View state.
    pub mode: Mode,
    pub detail_tab: DetailTab,
    pub detail: Option<Detail>,
    pub detail_scroll: u16,
    pub focus: Focus,
    pub show_help: bool,
    pub status: String,
    pub should_quit: bool,

    // Filter menu state.
    /// The filter menu's selectable rows (built from values present in the repo).
    pub filter_menu: Vec<MenuItem>,
    /// Cursor into `filter_menu` while `Mode::Filter` is active.
    pub filter_cursor: usize,
}

impl App {
    /// Build the app from a file store, performing the initial scan.
    pub fn new(store: ItemStore) -> Self {
        let mut app = App {
            data: Data::new(store),
            list: Listing::default(),
            mode: Mode::Browse,
            detail_tab: DetailTab::Overview,
            detail: None,
            detail_scroll: 0,
            focus: Focus::List,
            show_help: false,
            status: String::new(),
            should_quit: false,
            filter_menu: Vec::new(),
            filter_cursor: 0,
        };
        app.refresh();
        app
    }

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
        self.filter_menu = menu;
        if self.filter_cursor >= self.filter_menu.len() {
            self.filter_cursor = 0;
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
        self.detail_scroll = 0;
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
        if self.detail_tab != tab {
            self.detail_tab = tab;
            self.detail_scroll = 0;
        }
    }

    pub fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(3);
    }

    pub fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(3);
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
        if self.filter_cursor >= self.filter_menu.len() {
            self.filter_cursor = 0;
        }
        self.mode = Mode::Filter;
    }

    /// Close the filter menu, returning to browse mode.
    pub fn exit_filter(&mut self) {
        self.mode = Mode::Browse;
    }

    /// Move the filter-menu cursor by `delta` (clamped).
    pub fn filter_move(&mut self, delta: i32) {
        if self.filter_menu.is_empty() {
            return;
        }
        let last = self.filter_menu.len() as i32 - 1;
        let next = (self.filter_cursor as i32 + delta).clamp(0, last);
        self.filter_cursor = next as usize;
    }

    /// Whether the menu item at `idx` is currently selected in the filter.
    pub fn is_menu_selected(&self, idx: usize) -> bool {
        let Some(item) = self.filter_menu.get(idx) else {
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
        let Some(item) = self.filter_menu.get(self.filter_cursor).cloned() else {
            return;
        };
        let on = self.is_menu_selected(self.filter_cursor);
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

    // --- Detail loading ---------------------------------------------------

    /// Load the body, comments, dep tree, and block reasons for the selection.
    fn load_detail(&mut self) {
        let Some(fm) = self.selected_frontmatter() else {
            self.detail = None;
            return;
        };
        let id = fm.id.clone();

        let item = match self.data.store.get(&id) {
            Ok(item) => item,
            Err(e) => {
                self.detail = None;
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

        self.detail = Some(Detail {
            item,
            comments,
            blocking_deps,
            dangling_deps,
            children,
            tree,
        });
    }
}

/// Add or remove `value` from `vec` (used for multi-valued facets). `present`
/// says whether it is currently in the vec.
fn toggle_vec<T: PartialEq>(vec: &mut Vec<T>, value: T, present: bool) {
    if present {
        vec.retain(|v| v != &value);
    } else {
        vec.push(value);
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
    use clove_core::{ItemType, NewItem, Priority};

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
        let detail = app.detail.as_ref().expect("blocked item has detail");
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
}
