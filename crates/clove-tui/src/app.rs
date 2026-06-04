//! TUI application state and the read-only data layer.
//!
//! All data comes from the file store (the source of truth): a fresh
//! `scan_frontmatter` + `GraphStore::build` on launch and on every manual
//! refresh. This keeps the TUI always-correct and decoupled from the optional
//! SQLite index and daemon — it never mutates anything.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use clove_core::{
    BlockedItem, ChildrenSummary, CloveId, Comment, DepTreeNode, GraphStore, Item, ItemFrontmatter,
    ItemStore,
};
use ratatui::widgets::ListState;

/// The top-level view filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    All,
    Ready,
    Blocked,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::All, Tab::Ready, Tab::Blocked];

    pub fn title(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Ready => "Ready",
            Tab::Blocked => "Blocked",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Tab::All => 0,
            Tab::Ready => 1,
            Tab::Blocked => 2,
        }
    }
}

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

/// Input mode: normal browsing vs. typing a search query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Search,
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
    store: ItemStore,

    // Loaded data (refreshed wholesale).
    all: Vec<ItemFrontmatter>,
    ready: HashSet<CloveId>,
    blocked: HashMap<CloveId, BlockedItem>,
    graph: GraphStore,
    /// Non-fatal load problems (e.g. files that failed to parse).
    pub load_warnings: Vec<String>,

    // View state.
    pub tab: Tab,
    /// Indices into `all` that pass the current tab + search filter.
    view: Vec<usize>,
    pub list_state: ListState,
    pub mode: Mode,
    pub search: String,
    pub detail_tab: DetailTab,
    pub detail: Option<Detail>,
    pub detail_scroll: u16,
    pub focus: Focus,
    pub show_help: bool,
    pub status: String,
    pub should_quit: bool,
}

impl App {
    /// Build the app from a file store, performing the initial scan.
    pub fn new(store: ItemStore) -> Self {
        let mut app = App {
            store,
            all: Vec::new(),
            ready: HashSet::new(),
            blocked: HashMap::new(),
            graph: GraphStore::build(&[]).0,
            load_warnings: Vec::new(),
            tab: Tab::All,
            view: Vec::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            search: String::new(),
            detail_tab: DetailTab::Overview,
            detail: None,
            detail_scroll: 0,
            focus: Focus::List,
            show_help: false,
            status: String::new(),
            should_quit: false,
        };
        app.refresh();
        app
    }

    /// Re-scan the store and rebuild all derived state, preserving the selected
    /// item where possible.
    pub fn refresh(&mut self) {
        let selected_id = self.selected_id();

        let (mut frontmatters, errors) = match self.store.scan_frontmatter() {
            Ok(pair) => pair,
            Err(e) => {
                self.status = format!("scan failed: {e}");
                return;
            }
        };

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

        self.recompute_view();
        // Restore selection to the same id when still present.
        if let Some(id) = selected_id {
            if let Some(pos) = self.view.iter().position(|&i| self.all[i].id == id) {
                self.list_state.select(Some(pos));
            }
        }
        self.clamp_selection();
        self.load_detail();
        self.status = format!(
            "{} item(s) loaded{}",
            self.all.len(),
            if self.load_warnings.is_empty() {
                String::new()
            } else {
                format!(" · {} warning(s)", self.load_warnings.len())
            }
        );
    }

    /// Recompute the filtered view indices from the current tab + search.
    fn recompute_view(&mut self) {
        let needle = self.search.to_lowercase();
        self.view = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, fm)| match self.tab {
                Tab::All => true,
                Tab::Ready => self.ready.contains(&fm.id),
                Tab::Blocked => self.blocked.contains_key(&fm.id),
            })
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
        self.clamp_selection();
    }

    /// Keep the list selection within the current view bounds.
    fn clamp_selection(&mut self) {
        if self.view.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.view.len() - 1);
            self.list_state.select(Some(sel));
        }
    }

    /// The frontmatter rows in the current (filtered, ordered) view.
    pub fn visible(&self) -> impl Iterator<Item = &ItemFrontmatter> {
        self.view.iter().map(move |&i| &self.all[i])
    }

    pub fn visible_count(&self) -> usize {
        self.view.len()
    }

    pub fn total_count(&self) -> usize {
        self.all.len()
    }

    /// Count of items belonging to `tab` (ignoring the active search), for the
    /// tab-bar badges.
    pub fn visible_for(&self, tab: Tab) -> usize {
        match tab {
            Tab::All => self.all.len(),
            Tab::Ready => self
                .all
                .iter()
                .filter(|fm| self.ready.contains(&fm.id))
                .count(),
            Tab::Blocked => self
                .all
                .iter()
                .filter(|fm| self.blocked.contains_key(&fm.id))
                .count(),
        }
    }

    /// Whether an item is ready / blocked (for badges in the list).
    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.ready.contains(id)
    }

    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.blocked.contains_key(id)
    }

    /// The currently-selected item's frontmatter, if any.
    pub fn selected_frontmatter(&self) -> Option<&ItemFrontmatter> {
        let pos = self.list_state.selected()?;
        let &idx = self.view.get(pos)?;
        self.all.get(idx)
    }

    fn selected_id(&self) -> Option<CloveId> {
        self.selected_frontmatter().map(|fm| fm.id.clone())
    }

    // --- Navigation -------------------------------------------------------

    pub fn select_next(&mut self) {
        if self.view.is_empty() {
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) if i + 1 < self.view.len() => i + 1,
            Some(i) => i,
            None => 0,
        };
        self.list_state.select(Some(next));
        self.on_selection_changed();
    }

    pub fn select_prev(&mut self) {
        if self.view.is_empty() {
            return;
        }
        let prev = self.list_state.selected().unwrap_or(0).saturating_sub(1);
        self.list_state.select(Some(prev));
        self.on_selection_changed();
    }

    pub fn select_first(&mut self) {
        if !self.view.is_empty() {
            self.list_state.select(Some(0));
            self.on_selection_changed();
        }
    }

    pub fn select_last(&mut self) {
        if !self.view.is_empty() {
            self.list_state.select(Some(self.view.len() - 1));
            self.on_selection_changed();
        }
    }

    fn on_selection_changed(&mut self) {
        self.detail_scroll = 0;
        self.load_detail();
    }

    // --- Tabs / detail views ---------------------------------------------

    pub fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.recompute_view();
            self.on_selection_changed();
        }
    }

    pub fn next_tab(&mut self) {
        let next = (self.tab.index() + 1) % Tab::ALL.len();
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
        self.search.push(c);
        self.recompute_view();
        self.on_selection_changed();
    }

    pub fn pop_search(&mut self) {
        self.search.pop();
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
        if !self.search.is_empty() {
            self.search.clear();
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

        let item = match self.store.get(&id) {
            Ok(item) => item,
            Err(e) => {
                self.detail = None;
                self.status = format!("failed to load {id}: {e}");
                return;
            }
        };

        let comments = clove_core::list_comments(self.store.issues_dir(), &id).unwrap_or_default();

        let (blocking_deps, dangling_deps) = self
            .blocked
            .get(&id)
            .map(|b| (b.blocking_deps.clone(), b.dangling_deps.clone()))
            .unwrap_or_default();

        let children = self.graph.epic_children_summary(&id);

        let tree = self.graph.dep_tree(&id, 25);

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

/// Format a UTC timestamp for display (date + minute precision, local-agnostic).
pub fn fmt_ts(ts: chrono::DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
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
        assert_eq!(app.list_state.selected(), Some(0));
        app.select_last();
        assert_eq!(app.list_state.selected(), Some(2));
        // Moving past the end stays at the end.
        app.select_next();
        assert_eq!(app.list_state.selected(), Some(2));
    }
}
