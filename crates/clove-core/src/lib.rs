//! clove-core: item model, file store, dependency-graph engine, ID generation.
//!
//! This crate is pure: no SQLite, no async, no IPC, no clap. Correctness for the
//! whole tool lives here — the index and daemon are accelerators layered on top.

pub mod comments;
pub mod config;
pub mod doctor;
pub mod error;
pub mod fixtures;
pub mod graph;
pub mod id;
pub mod limits;
pub mod model;
pub mod parse;
pub mod repo;
pub mod store;
pub mod validate;
pub mod write;

pub use comments::{add_comment, list_comments, Comment};
pub use config::{load_config, CloveConfig, DaemonConfig, IndexConfig, OutputFormat};
pub use doctor::{diagnose, fix as doctor_fix, DoctorIssue, DoctorReport, Severity};
pub use error::CloveError;
pub use graph::{
    is_hard_dep, BlockedItem, ChildrenSummary, DanglingRef, DepTreeNode, EdgeKind, GraphStore,
    ItemMeta,
};
pub use id::CloveId;
pub use model::{normalize_label, Item, ItemFrontmatter, ItemStatus, ItemType, Priority};
pub use parse::{
    contains_yaml_anchor_or_alias, parse_frontmatter_file, parse_item_bytes, parse_item_file,
    parse_item_lenient,
};
pub use store::{ItemStore, NewItem, ScanError};
pub use validate::{validate_item, ValidationError};
