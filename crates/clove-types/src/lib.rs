//! clove-types: the pure, shared data types for the whole workspace.
//!
//! The item model, id, errors, field validation, and the create/edit request
//! types live here — with no store, graph, SQLite, async, or IPC. `clove-core`
//! layers the file store, dependency graph, and high-level operations on top;
//! every other crate marshals these types without pulling in that machinery.

pub mod error;
pub mod fields;
pub mod id;
pub mod limits;
pub mod model;
pub mod request;
pub mod validate;

pub use error::{error_code, CloveError};
pub use id::{generate_id, new_id, CloveId};
pub use model::{
    normalize_label, Item, ItemFrontmatter, ItemStatus, ItemType, Priority, CURRENT_SCHEMA_VERSION,
};
pub use request::{apply_assignments, normalize_body, set_status, EditRequest, LabelEdit, NewSpec};
pub use validate::{validate_item, ValidationError};
