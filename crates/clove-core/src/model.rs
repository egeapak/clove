//! The item data model: [`Item`], [`ItemFrontmatter`], and the field types
//! [`ItemStatus`], [`ItemType`], [`Priority`]. Also [`normalize_label`], the
//! single canonicalization point for labels (DESIGN.md §2.2).
//!
//! ## Design note: status / closed coupling
//!
//! DESIGN.md §2.3 sketches `ItemStatus::Closed { at }` with the timestamp
//! embedded in the enum. We instead model status as a plain tag
//! ([`ItemStatus`]) plus a separate [`ItemFrontmatter::closed`] timestamp field.
//! The wire format is identical either way (`status: closed` + `closed: <ts>` in
//! YAML; separate `status`/`closed` keys in JSON — §2.2/§7.4). The reason for
//! the tag-plus-field representation: it lets a *broken* coupling (e.g.
//! `status: closed` with no `closed:` timestamp, from a bad hand-edit or merge)
//! be **represented, then reported** by `validate_item` (T-C04) and `clove
//! doctor` (§7.7 check #4) as a recoverable validation finding — rather than
//! being unrepresentable and so only surfacing as an opaque parse failure.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CloveError;
use crate::id::CloveId;

/// The current item schema version. Files without a `schema` key are treated as
/// version 1 (DESIGN.md §2.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// A complete item: validated frontmatter plus the Markdown body below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub frontmatter: ItemFrontmatter,
    pub body: String,
}

/// The lifecycle status of an item.
///
/// This is the discriminant only; the close timestamp lives in
/// [`ItemFrontmatter::closed`] (see the module-level design note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Open,
    InProgress,
    Closed,
}

impl ItemStatus {
    /// True for `open` / `in_progress` — the statuses eligible for `ready`.
    pub fn is_active(self) -> bool {
        matches!(self, ItemStatus::Open | ItemStatus::InProgress)
    }

    /// The canonical wire string (`open`, `in_progress`, `closed`).
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Open => "open",
            ItemStatus::InProgress => "in_progress",
            ItemStatus::Closed => "closed",
        }
    }

    /// Parse a status word, accepting the common aliases used by the CLI/MCP
    /// surfaces (`in-progress`/`started` → in_progress; `done` → closed).
    pub fn parse(raw: &str) -> Result<ItemStatus, CloveError> {
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
}

/// The kind of work an item represents. Serialized as the `type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Bug,
    #[default]
    Feature,
    Chore,
    Docs,
    Epic,
}

impl ItemType {
    /// The canonical wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Bug => "bug",
            ItemType::Feature => "feature",
            ItemType::Chore => "chore",
            ItemType::Docs => "docs",
            ItemType::Epic => "epic",
        }
    }

    /// Parse a type word into [`ItemType`].
    pub fn parse(raw: &str) -> Result<ItemType, CloveError> {
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
}

/// Item priority: 0 (highest) through 4, default 2.
///
/// A thin `u8` wrapper that does **not** validate on construction — the range
/// is checked by `validate_item` (T-C04), so an out-of-range value parsed from
/// disk is representable and reportable rather than a hard parse error. Use
/// [`Priority::new`] when you want validation at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Priority(pub u8);

impl Priority {
    /// Highest priority.
    pub const HIGHEST: Priority = Priority(0);
    /// The default priority for new items.
    pub const DEFAULT: Priority = Priority(2);
    /// Lowest valid priority.
    pub const LOWEST: Priority = Priority(4);

    /// The maximum valid priority value.
    pub const MAX: u8 = 4;

    /// Construct a priority, validating it is within 0–4.
    pub fn new(value: u8) -> Result<Priority, CloveError> {
        if value <= Priority::MAX {
            Ok(Priority(value))
        } else {
            Err(CloveError::InvalidField {
                field: "priority".to_owned(),
                reason: format!("must be 0–{}, got {value}", Priority::MAX),
            })
        }
    }

    /// The raw numeric value.
    pub fn get(self) -> u8 {
        self.0
    }

    /// Whether this priority is within the valid 0–4 range.
    pub fn is_valid(self) -> bool {
        self.0 <= Priority::MAX
    }
}

impl Default for Priority {
    fn default() -> Self {
        Priority::DEFAULT
    }
}

/// The parsed, structured frontmatter of an item.
///
/// Field declaration order here mirrors the canonical on-disk order
/// (DESIGN.md §2.2). On-disk YAML serialization is performed by the hand-rolled
/// `FrontmatterWriter` (T-C02), not by this type's `Serialize` impl; the derived
/// serde impls drive deserialization from YAML and JSON (de)serialization for
/// tests and export. `#[serde(default)]` on the list fields (with no
/// `skip_serializing_if`) makes JSON serialization emit `[]` for empty arrays,
/// matching the item JSON schema (§7.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFrontmatter {
    /// Schema version. Missing on disk → treated as [`CURRENT_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema: u32,
    pub id: CloveId,
    pub title: String,
    pub status: ItemStatus,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    pub priority: Priority,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,

    // Optional fields (after the required block, in canonical order).
    #[serde(default)]
    pub closed: Option<DateTime<Utc>>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub parent: Option<CloveId>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub deps: Vec<CloveId>,
    #[serde(default)]
    pub relates: Vec<CloveId>,
    #[serde(default)]
    pub duplicates: Vec<CloveId>,
    #[serde(default)]
    pub supersedes: Vec<CloveId>,
    #[serde(default)]
    pub source_system: Option<String>,
    #[serde(default)]
    pub external_ref: Option<String>,
}

/// Canonicalize a label (DESIGN.md §2.2): Unicode-lowercase, trim, collapse
/// internal whitespace runs to a single space, reject empty.
///
/// This is the single canonicalization point used by item construction, label
/// edits, `--label` filters, and importers, so the stored frontmatter only ever
/// contains canonical labels (`area:iOS`, `  AREA:IOS  `, `area:ios` all map to
/// `area:ios`).
pub fn normalize_label(raw: &str) -> Result<String, CloveError> {
    // `split_whitespace` trims leading/trailing whitespace and collapses any
    // internal whitespace run to a single separator; rejoining with a single
    // space yields the canonical form. Lowercasing first keeps Unicode casing
    // rules (`to_lowercase`) intact.
    let lowered = raw.to_lowercase();
    let canonical = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    if canonical.is_empty() {
        return Err(CloveError::EmptyLabel {
            raw: raw.to_owned(),
        });
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frontmatter() -> ItemFrontmatter {
        ItemFrontmatter {
            schema: 1,
            id: CloveId::new("proj-7AF3K2MN").unwrap(),
            title: "Sample".to_owned(),
            status: ItemStatus::Open,
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
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
    fn item_type_serializes_as_type_field() {
        let fm = sample_frontmatter();
        let value = serde_json::to_value(&fm).unwrap();
        assert_eq!(value["type"], "feature");
        assert!(value.get("item_type").is_none(), "must serialize as `type`");
    }

    #[test]
    fn empty_lists_serialize_as_empty_arrays() {
        let fm = sample_frontmatter();
        let value = serde_json::to_value(&fm).unwrap();
        for field in ["deps", "relates", "duplicates", "supersedes", "labels"] {
            assert_eq!(value[field], serde_json::json!([]), "{field} must be []");
        }
    }

    #[test]
    fn status_and_closed_roundtrip_through_json() {
        // The status tag + separate `closed` timestamp survive a serialize →
        // deserialize cycle (the coupling-aware equivalent of §2.3's
        // `Closed { at }` round-trip).
        let mut fm = sample_frontmatter();
        fm.status = ItemStatus::Closed;
        fm.closed = Some("2026-06-02T14:23:00Z".parse().unwrap());

        let json = serde_json::to_string(&fm).unwrap();
        let back: ItemFrontmatter = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, ItemStatus::Closed);
        assert_eq!(back.closed, fm.closed);
        assert_eq!(back, fm);
    }

    #[test]
    fn deserialize_defaults_missing_schema_to_v1() {
        let json = serde_json::json!({
            "id": "proj-7AF3K2MN",
            "title": "No schema key",
            "status": "open",
            "type": "bug",
            "priority": 1,
            "created": "2026-06-02T10:00:00Z",
            "updated": "2026-06-02T10:00:00Z"
        });
        let fm: ItemFrontmatter = serde_json::from_value(json).unwrap();
        assert_eq!(fm.schema, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn deserialize_rejects_unknown_fields() {
        let json = serde_json::json!({
            "schema": 1,
            "id": "proj-7AF3K2MN",
            "title": "x",
            "status": "open",
            "type": "bug",
            "priority": 1,
            "created": "2026-06-02T10:00:00Z",
            "updated": "2026-06-02T10:00:00Z",
            "blocks": ["proj-00000000"]
        });
        // `blocks` is never stored — deny_unknown_fields must reject it.
        assert!(serde_json::from_value::<ItemFrontmatter>(json).is_err());
    }

    #[test]
    fn priority_out_of_range_still_deserializes() {
        // Representable (so validate_item / doctor can report it), not a hard
        // parse error.
        let json = serde_json::json!({
            "schema": 1,
            "id": "proj-7AF3K2MN",
            "title": "x",
            "status": "open",
            "type": "bug",
            "priority": 9,
            "created": "2026-06-02T10:00:00Z",
            "updated": "2026-06-02T10:00:00Z"
        });
        let fm: ItemFrontmatter = serde_json::from_value(json).unwrap();
        assert_eq!(fm.priority, Priority(9));
        assert!(!fm.priority.is_valid());
    }

    #[test]
    fn normalize_label_canonicalizes_case_and_whitespace() {
        for input in ["Area:iOS", "  AREA:IOS  ", "area:ios", "area:IOS"] {
            assert_eq!(normalize_label(input).unwrap(), "area:ios");
        }
        assert_eq!(
            normalize_label("multi   word  tag").unwrap(),
            "multi word tag"
        );
    }

    #[test]
    fn normalize_label_rejects_empty() {
        assert!(normalize_label("   ").is_err());
        assert!(normalize_label("").is_err());
        assert!(normalize_label("\t\n").is_err());
    }

    #[test]
    fn priority_new_validates_range() {
        assert!(Priority::new(0).is_ok());
        assert!(Priority::new(4).is_ok());
        assert!(Priority::new(5).is_err());
        assert!(Priority::new(255).is_err());
    }
}
