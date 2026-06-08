//! The add/edit form state: a modal over the browser that builds the unified
//! [`NewSpec`] / [`EditRequest`] and applies them through `clove_core` ops, so
//! the TUI shares one write path with the CLI, web, and MCP surfaces.

use clove_types::{CloveId, EditRequest, Item, ItemStatus, ItemType, LabelEdit, NewSpec};

/// Whether the form creates a new item or edits an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    New,
    Edit,
}

/// One editable row in the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Status,
    Type,
    Priority,
    Assignee,
    Labels,
    Parent,
    Deps,
    Body,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Status => "Status",
            Field::Type => "Type",
            Field::Priority => "Priority",
            Field::Assignee => "Assignee",
            Field::Labels => "Labels",
            Field::Parent => "Parent",
            Field::Deps => "Deps",
            Field::Body => "Body",
        }
    }

    /// Enum fields are cycled with ←/→; the rest take typed text.
    pub fn is_enum(self) -> bool {
        matches!(self, Field::Status | Field::Type | Field::Priority)
    }
}

const TYPES: [ItemType; 5] = [
    ItemType::Bug,
    ItemType::Feature,
    ItemType::Chore,
    ItemType::Docs,
    ItemType::Epic,
];
const STATUSES: [ItemStatus; 3] = [ItemStatus::Open, ItemStatus::InProgress, ItemStatus::Closed];

/// The full form state (one instance, reused across opens).
pub struct FormState {
    pub mode: FormMode,
    pub edit_id: Option<CloveId>,
    /// The original item being edited (to diff parent/deps on submit).
    pub original: Option<Item>,
    pub fields: Vec<Field>,
    pub focus: usize,

    pub title: String,
    pub status: ItemStatus,
    pub item_type: ItemType,
    pub priority: u8,
    pub assignee: String,
    pub labels: String,
    pub parent: String,
    pub deps: String,
    pub body: String,

    /// A validation/op error from the last submit attempt (keeps the form open).
    pub error: Option<String>,
}

impl Default for FormState {
    fn default() -> Self {
        FormState {
            mode: FormMode::New,
            edit_id: None,
            original: None,
            fields: Vec::new(),
            focus: 0,
            title: String::new(),
            status: ItemStatus::Open,
            item_type: ItemType::Feature,
            priority: 2,
            assignee: String::new(),
            labels: String::new(),
            parent: String::new(),
            deps: String::new(),
            body: String::new(),
            error: None,
        }
    }
}

impl FormState {
    /// Reset to a blank "new item" form with the repo's default type.
    pub fn new_item(default_type: ItemType) -> FormState {
        FormState {
            mode: FormMode::New,
            item_type: default_type,
            fields: vec![
                Field::Title,
                Field::Type,
                Field::Priority,
                Field::Assignee,
                Field::Labels,
                Field::Parent,
                Field::Deps,
                Field::Body,
            ],
            ..FormState::default()
        }
    }

    /// Prefill an "edit" form from an existing item.
    pub fn edit_item(item: &Item) -> FormState {
        let fm = &item.frontmatter;
        FormState {
            mode: FormMode::Edit,
            edit_id: Some(fm.id.clone()),
            original: Some(item.clone()),
            fields: vec![
                Field::Title,
                Field::Status,
                Field::Type,
                Field::Priority,
                Field::Assignee,
                Field::Labels,
                Field::Parent,
                Field::Deps,
                Field::Body,
            ],
            focus: 0,
            title: fm.title.clone(),
            status: fm.status,
            item_type: fm.item_type,
            priority: fm.priority.get(),
            assignee: fm.assignee.clone().unwrap_or_default(),
            labels: fm.labels.join(", "),
            parent: fm
                .parent
                .as_ref()
                .map(|p| p.to_string())
                .unwrap_or_default(),
            deps: fm
                .deps
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            body: item.body.clone(),
            error: None,
        }
    }

    pub fn focused(&self) -> Field {
        self.fields[self.focus]
    }

    pub fn next_field(&mut self) {
        self.focus = (self.focus + 1) % self.fields.len();
    }

    pub fn prev_field(&mut self) {
        self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
    }

    /// The mutable text buffer for the focused text field (None for enum fields).
    fn buffer_mut(&mut self) -> Option<&mut String> {
        match self.focused() {
            Field::Title => Some(&mut self.title),
            Field::Assignee => Some(&mut self.assignee),
            Field::Labels => Some(&mut self.labels),
            Field::Parent => Some(&mut self.parent),
            Field::Deps => Some(&mut self.deps),
            Field::Body => Some(&mut self.body),
            Field::Status | Field::Type | Field::Priority => None,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if let Some(buf) = self.buffer_mut() {
            buf.push(c);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(buf) = self.buffer_mut() {
            buf.pop();
        }
    }

    /// Insert a newline (Body field only).
    pub fn newline(&mut self) {
        if self.focused() == Field::Body {
            self.body.push('\n');
        }
    }

    /// Cycle an enum field's value by `delta` (wrapping). No-op on text fields.
    pub fn cycle(&mut self, delta: i32) {
        match self.focused() {
            Field::Type => self.item_type = cycle_arr(&TYPES, self.item_type, delta),
            Field::Status => self.status = cycle_arr(&STATUSES, self.status, delta),
            Field::Priority => {
                self.priority = (self.priority as i32 + delta).rem_euclid(5) as u8;
            }
            _ => {}
        }
    }

    /// The display value of an enum field.
    pub fn enum_value(&self, field: Field) -> String {
        match field {
            Field::Type => self.item_type.as_str().to_owned(),
            Field::Status => self.status.as_str().to_owned(),
            Field::Priority => format!("p{}", self.priority),
            _ => String::new(),
        }
    }

    // ---- payload builders ----------------------------------------------------

    /// Build a [`NewSpec`] for the create path.
    pub fn to_new_spec(&self) -> NewSpec {
        NewSpec {
            title: self.title.trim().to_owned(),
            item_type: Some(self.item_type.as_str().to_owned()),
            priority: Some(self.priority),
            labels: split_csv(&self.labels),
            deps: split_csv(&self.deps),
            parent: opt(&self.parent),
            assignee: opt(&self.assignee),
            body: opt(&self.body),
        }
    }

    /// Build an [`EditRequest`] for the scalar/label/body surface (parent and
    /// deps are applied separately via the graph-validated ops).
    pub fn to_edit_request(&self) -> EditRequest {
        EditRequest {
            title: Some(self.title.trim().to_owned()),
            body: Some(self.body.clone()),
            status: Some(self.status),
            priority: clove_types::Priority::new(self.priority).ok(),
            item_type: Some(self.item_type),
            assignee: Some(opt(&self.assignee)),
            labels: Some(LabelEdit::Set(split_csv(&self.labels))),
        }
    }

    /// The parsed parent id (None when blank).
    pub fn parent_id(&self) -> Result<Option<CloveId>, clove_types::CloveError> {
        match opt(&self.parent) {
            Some(p) => Ok(Some(CloveId::new(&p)?)),
            None => Ok(None),
        }
    }

    /// The parsed dependency ids.
    pub fn dep_ids(&self) -> Result<Vec<CloveId>, clove_types::CloveError> {
        split_csv(&self.deps)
            .iter()
            .map(|s| CloveId::new(s))
            .collect()
    }
}

/// Cycle through a fixed array of `Copy + PartialEq` values by `delta`.
fn cycle_arr<T: Copy + PartialEq>(arr: &[T], current: T, delta: i32) -> T {
    let i = arr.iter().position(|&v| v == current).unwrap_or(0) as i32;
    let n = arr.len() as i32;
    arr[(i + delta).rem_euclid(n) as usize]
}

/// Split a comma-separated field into trimmed, non-empty parts.
fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A trimmed string, or None when blank.
fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}
