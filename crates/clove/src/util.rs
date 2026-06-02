//! Small parsing helpers shared across commands.

use chrono::{DateTime, Timelike, Utc};
use clove_core::{CloveError, CloveId, ItemStatus, ItemType, Priority};

/// The current time truncated to whole seconds (the canonical on-disk timestamp
/// precision; matches `ItemStore`'s internal truncation).
pub fn now_seconds() -> DateTime<Utc> {
    Utc::now().with_nanosecond(0).unwrap_or_else(Utc::now)
}

/// Parse and validate an item id argument.
pub fn parse_id(raw: &str) -> Result<CloveId, CloveError> {
    CloveId::new(raw)
}

/// Parse a status word into [`ItemStatus`].
pub fn parse_status(raw: &str) -> Result<ItemStatus, CloveError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "open" => Ok(ItemStatus::Open),
        "in_progress" | "in-progress" | "started" => Ok(ItemStatus::InProgress),
        "closed" | "done" => Ok(ItemStatus::Closed),
        other => Err(CloveError::InvalidField {
            field: "status".to_owned(),
            reason: format!("expected open|in_progress|closed, got `{other}`"),
        }),
    }
}

/// Parse a type word into [`ItemType`].
pub fn parse_type(raw: &str) -> Result<ItemType, CloveError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "bug" => Ok(ItemType::Bug),
        "feature" => Ok(ItemType::Feature),
        "chore" => Ok(ItemType::Chore),
        "docs" => Ok(ItemType::Docs),
        "epic" => Ok(ItemType::Epic),
        other => Err(CloveError::InvalidField {
            field: "type".to_owned(),
            reason: format!("expected bug|feature|chore|docs|epic, got `{other}`"),
        }),
    }
}

/// Parse and validate a priority 0–4.
pub fn parse_priority(raw: u8) -> Result<Priority, CloveError> {
    Priority::new(raw)
}
