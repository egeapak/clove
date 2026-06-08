//! The detail pane: which sub-view is active, the loaded per-selection data, and
//! the scroll offset. `DetailPane.detail` is the loaded [`Detail`] (so the field
//! path is `app.detail.detail`).

use clove_core::{ChildrenSummary, Comment, DepTreeNode};
use clove_types::{CloveId, Item};

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
