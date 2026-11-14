//! The detail pane: which sub-view is active, the loaded per-selection data, and
//! the scroll offset. `DetailPane.detail` is the loaded [`Detail`] (so the field
//! path is `app.detail.detail`).

use std::cell::{Cell, RefCell};

use clove_core::{ChildrenSummary, Comment, DepTreeNode};
use clove_types::{CloveId, Item};
use ratatui::text::Line;

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
    /// The body rendered from Markdown, with the width it was rendered for.
    /// A `Detail` is rebuilt whenever the selection or the store changes, so
    /// the width is the only thing that can invalidate it.
    body_lines: RefCell<Option<(u16, Vec<Line<'static>>)>>,
    /// How many times the body's Markdown has been rendered (for tests).
    body_renders: Cell<u32>,
}

impl Detail {
    pub fn new(
        item: Item,
        comments: Vec<Comment>,
        blocking_deps: Vec<CloveId>,
        dangling_deps: Vec<CloveId>,
        children: Option<ChildrenSummary>,
        tree: Option<DepTreeNode>,
    ) -> Detail {
        Detail {
            item,
            comments,
            blocking_deps,
            dangling_deps,
            children,
            tree,
            body_lines: RefCell::new(None),
            body_renders: Cell::new(0),
        }
    }

    /// The item body rendered from Markdown for a pane `width` columns wide.
    ///
    /// The frame is redrawn on every key press and every busy tick, and the
    /// body used to be parsed and rendered from Markdown each time. The lines
    /// are now rendered once per width and reused until the detail is reloaded.
    pub fn body_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut cache = self.body_lines.borrow_mut();
        match &*cache {
            Some((cached_width, lines)) if *cached_width == width => lines.clone(),
            _ => {
                self.body_renders.set(self.body_renders.get() + 1);
                let lines = crate::markdown::render(self.item.body.trim(), width);
                *cache = Some((width, lines.clone()));
                lines
            }
        }
    }

    /// How many times [`Detail::body_lines`] had to render the Markdown.
    #[cfg(test)]
    pub(crate) fn body_renders(&self) -> u32 {
        self.body_renders.get()
    }
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
