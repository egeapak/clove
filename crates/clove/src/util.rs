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

/// Parse a status word into [`ItemStatus`] (delegates to the shared core parser).
pub fn parse_status(raw: &str) -> Result<ItemStatus, CloveError> {
    ItemStatus::parse(raw)
}

/// Parse a type word into [`ItemType`] (delegates to the shared core parser).
pub fn parse_type(raw: &str) -> Result<ItemType, CloveError> {
    ItemType::parse(raw)
}

/// Parse and validate a priority 0–4.
pub fn parse_priority(raw: u8) -> Result<Priority, CloveError> {
    Priority::new(raw)
}
