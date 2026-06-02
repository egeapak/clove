//! Field-level validation of parsed frontmatter (DESIGN.md §7.7 check #4).
//!
//! `validate_item` reports the invariants that are *representable* on an
//! [`ItemFrontmatter`] yet may be violated by a hand-edit, bad merge, or buggy
//! importer:
//!
//! - priority within 0–4,
//! - the status ↔ `closed`-timestamp coupling,
//! - a supported `schema` version,
//! - dependency/relation array lengths within [`MAX_DEP_ARRAY_LEN`].
//!
//! Two invariants from the spec are enforced *earlier*, by the type system and
//! the parser, so they cannot reach `validate_item` as a bad value:
//! - **ID format** (`deps`/`parent`/relations) — each is a [`crate::CloveId`],
//!   validated on construction; a malformed id fails YAML deserialization.
//! - **RFC3339 timestamps** — each is a `chrono::DateTime<Utc>`; a malformed
//!   timestamp fails YAML deserialization.

use thiserror::Error;

use crate::limits::MAX_DEP_ARRAY_LEN;
use crate::model::{ItemFrontmatter, ItemStatus, CURRENT_SCHEMA_VERSION};

/// A single field-level validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("priority must be 0–4, got {0}")]
    PriorityOutOfRange(u8),

    #[error("status is `closed` but no `closed` timestamp is set")]
    ClosedWithoutTimestamp,

    #[error("`closed` timestamp is set but status is `{0}`")]
    ClosedTimestampOnNonClosed(&'static str),

    #[error("unsupported schema version {found} (this build supports {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },

    #[error("`{field}` has {len} entries, exceeding the maximum of {max}")]
    ListTooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
}

/// Validate `frontmatter`, returning every failure found (empty = valid).
pub fn validate_item(frontmatter: &ItemFrontmatter) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if !frontmatter.priority.is_valid() {
        errors.push(ValidationError::PriorityOutOfRange(
            frontmatter.priority.get(),
        ));
    }

    match (frontmatter.status, frontmatter.closed) {
        (ItemStatus::Closed, None) => errors.push(ValidationError::ClosedWithoutTimestamp),
        (ItemStatus::Open | ItemStatus::InProgress, Some(_)) => {
            errors.push(ValidationError::ClosedTimestampOnNonClosed(
                frontmatter.status.as_str(),
            ));
        }
        _ => {}
    }

    if frontmatter.schema != CURRENT_SCHEMA_VERSION {
        errors.push(ValidationError::UnsupportedSchema {
            found: frontmatter.schema,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }

    for (field, len) in [
        ("deps", frontmatter.deps.len()),
        ("relates", frontmatter.relates.len()),
        ("duplicates", frontmatter.duplicates.len()),
        ("supersedes", frontmatter.supersedes.len()),
    ] {
        if len > MAX_DEP_ARRAY_LEN {
            errors.push(ValidationError::ListTooLong {
                field,
                len,
                max: MAX_DEP_ARRAY_LEN,
            });
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::CloveId;
    use crate::model::{ItemFrontmatter, ItemType, Priority};

    fn valid_frontmatter() -> ItemFrontmatter {
        ItemFrontmatter {
            schema: CURRENT_SCHEMA_VERSION,
            id: CloveId::new("proj-7AF3K2MN").unwrap(),
            title: "x".to_owned(),
            status: ItemStatus::Open,
            item_type: ItemType::Bug,
            priority: Priority(2),
            created: "2026-06-02T10:00:00Z".parse().unwrap(),
            updated: "2026-06-02T10:00:00Z".parse().unwrap(),
            closed: None,
            assignee: None,
            parent: None,
            labels: Vec::new(),
            deps: Vec::new(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
    }

    #[test]
    fn valid_item_has_no_errors() {
        assert!(validate_item(&valid_frontmatter()).is_empty());
    }

    #[test]
    fn rejects_priority_out_of_range() {
        let mut fm = valid_frontmatter();
        fm.priority = Priority(9);
        let errors = validate_item(&fm);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::PriorityOutOfRange(9))));
        assert!(errors[0].to_string().contains("priority"));
    }

    #[test]
    fn rejects_closed_status_without_timestamp() {
        let mut fm = valid_frontmatter();
        fm.status = ItemStatus::Closed;
        fm.closed = None;
        let errors = validate_item(&fm);
        assert!(errors.contains(&ValidationError::ClosedWithoutTimestamp));
        assert!(errors[0].to_string().contains("closed"));
    }

    #[test]
    fn rejects_closed_timestamp_on_open_item() {
        let mut fm = valid_frontmatter();
        fm.status = ItemStatus::Open;
        fm.closed = Some("2026-06-02T10:00:00Z".parse().unwrap());
        let errors = validate_item(&fm);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::ClosedTimestampOnNonClosed("open"))));
    }

    #[test]
    fn accepts_closed_status_with_timestamp() {
        let mut fm = valid_frontmatter();
        fm.status = ItemStatus::Closed;
        fm.closed = Some("2026-06-02T10:00:00Z".parse().unwrap());
        assert!(validate_item(&fm).is_empty());
    }

    #[test]
    fn rejects_unsupported_schema() {
        let mut fm = valid_frontmatter();
        fm.schema = 99;
        let errors = validate_item(&fm);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::UnsupportedSchema { found: 99, .. })));
        assert!(errors[0].to_string().contains("schema"));
    }

    #[test]
    fn rejects_oversized_dependency_array() {
        let mut fm = valid_frontmatter();
        fm.deps = (0..MAX_DEP_ARRAY_LEN + 1)
            .map(|_| CloveId::new("proj-AAAAAAAA").unwrap())
            .collect();
        let errors = validate_item(&fm);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::ListTooLong { field: "deps", .. })));
    }
}
