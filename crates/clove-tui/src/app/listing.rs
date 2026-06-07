//! The filtered/sorted projection over [`super::Data`]: which items are shown,
//! in what order, and the list cursor. Cohesive state for a future lock.

use clove_types::{ItemFrontmatter, ItemStatus, ItemType};
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

/// The field the list is sorted by. `Default` is the canonical
/// `(priority, topo-rank, id)` order shared with `clove ls`; the others are flat
/// single-key sorts (topo ordering does not apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Default,
    Priority,
    Created,
    Updated,
    Id,
}

impl SortField {
    const CYCLE: [SortField; 5] = [
        SortField::Default,
        SortField::Priority,
        SortField::Created,
        SortField::Updated,
        SortField::Id,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortField::Default => "rank",
            SortField::Priority => "prio",
            SortField::Created => "created",
            SortField::Updated => "updated",
            SortField::Id => "id",
        }
    }

    pub(super) fn next(self) -> SortField {
        let i = Self::CYCLE.iter().position(|&f| f == self).unwrap_or(0);
        Self::CYCLE[(i + 1) % Self::CYCLE.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn glyph(self) -> &'static str {
        match self {
            SortDir::Asc => "↑",
            SortDir::Desc => "↓",
        }
    }
}

/// The active sort: a field plus a direction.
#[derive(Debug, Clone, Copy)]
pub struct Sort {
    pub field: SortField,
    pub dir: SortDir,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            field: SortField::Default,
            dir: SortDir::Asc,
        }
    }
}

/// Interactive facet filters. Empty/`None` means unconstrained. Semantics:
/// AND across facets; OR within `types`/`priorities` (any-of); AND within
/// `labels` (all-of); `status`/`assignee` are single-valued.
#[derive(Debug, Default, Clone)]
pub struct ViewFilter {
    pub status: Option<ItemStatus>,
    pub assignee: Option<String>,
    pub types: Vec<ItemType>,
    pub priorities: Vec<u8>,
    pub labels: Vec<String>,
}

impl ViewFilter {
    pub fn is_active(&self) -> bool {
        self.status.is_some()
            || self.assignee.is_some()
            || !self.types.is_empty()
            || !self.priorities.is_empty()
            || !self.labels.is_empty()
    }

    pub(super) fn matches(&self, fm: &ItemFrontmatter) -> bool {
        if let Some(s) = self.status {
            if fm.status != s {
                return false;
            }
        }
        if let Some(a) = &self.assignee {
            if fm.assignee.as_deref() != Some(a.as_str()) {
                return false;
            }
        }
        if !self.types.is_empty() && !self.types.contains(&fm.item_type) {
            return false;
        }
        if !self.priorities.is_empty() && !self.priorities.contains(&fm.priority.get()) {
            return false;
        }
        // Labels are all-of (AND): every selected label must be present.
        self.labels.iter().all(|l| fm.labels.contains(l))
    }
}

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
