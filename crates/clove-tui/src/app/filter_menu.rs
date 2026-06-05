//! The facet filter menu: the selectable rows (built from values present in the
//! repo) and the cursor into them. The *active* filter lives on `Listing`.

use clove_core::{ItemStatus, ItemType};

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

#[derive(Default)]
pub struct FilterMenu {
    pub menu: Vec<MenuItem>,
    pub cursor: usize,
}

/// Add or remove `value` from `vec` (used for multi-valued facets). `present`
/// says whether it is currently in the vec.
pub(crate) fn toggle_vec<T: PartialEq>(vec: &mut Vec<T>, value: T, present: bool) {
    if present {
        vec.retain(|v| v != &value);
    } else {
        vec.push(value);
    }
}
