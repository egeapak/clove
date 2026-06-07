//! clove-core: file store, dependency-graph engine, and high-level operations.
//!
//! The pure data types (model, id, error, validation, request types) live in the
//! `clove-types` crate; this crate layers correctness on top — the file store,
//! the dependency graph, and the create/edit/transition operations. The index
//! and daemon are accelerators layered further on top.

pub mod comments;
pub mod config;
pub mod doctor;
pub mod edit;
pub mod fixtures;
pub mod graph;
pub mod ops;
pub mod parse;
pub mod repo;
pub mod stats;
pub mod store;
pub mod view;
pub mod write;

// The pure type layer now lives in `clove-types`. Re-export its modules at the
// same paths so this crate's internals keep using `crate::error`, `crate::model`,
// `crate::id`, `crate::validate`, `crate::fields`, and `crate::limits` unchanged.
pub use clove_types::{error, fields, id, limits, model, request, validate};

pub use comments::{add_comment, list_comments, Comment};
pub use config::{load_config, CloveConfig, DaemonConfig, IndexConfig, OutputFormat, WebConfig};
pub use doctor::{diagnose, fix as doctor_fix, DoctorIssue, DoctorReport, Severity};
pub use edit::apply_edit;
pub use graph::{
    is_hard_dep, BlockedItem, ChildrenSummary, DanglingRef, DepTreeNode, EdgeKind, GraphStore,
    ItemMeta,
};
pub use parse::{
    contains_yaml_anchor_or_alias, parse_frontmatter_file, parse_item_bytes, parse_item_file,
    parse_item_lenient,
};
pub use stats::{
    compute as compute_stats, EpicRollup, KeyCount, StatsOptions, StatsReport, StatusCounts,
    Throughput, TypeCounts,
};
pub use store::{ItemStore, NewItem, ScanError};
pub use view::{frontmatter_object, item_object, project, rank_of, sort_by_rank, Filters};

// Re-export the `clove-types` data types at this crate's root, mirroring the
// pre-split public surface so existing `clove_core::…` references keep resolving.
pub use clove_types::{
    error_code, normalize_body, normalize_label, validate_item, CloveError, CloveId, EditRequest,
    Item, ItemFrontmatter, ItemStatus, ItemType, LabelEdit, NewSpec, Priority, ValidationError,
};
