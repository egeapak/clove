//! The clove-native json/jsonl **restore** engine — a faithful, id-preserving
//! inverse of `export json` / `export jsonl`.
//!
//! Where the beads/tk/github importers *map* a foreign shape onto clove items,
//! restore re-reads clove's *own* export verbatim: every stored frontmatter
//! field (id, status, the `closed` timestamp, `created`/`updated`, labels, deps,
//! …) is preserved exactly, so an export → restore round-trip is a byte-stable
//! backup/restore rather than a lossy re-import.
//!
//! This module is **pure** — parsing + planning only, no I/O — mirroring the
//! `sync` (pure) / `sync_net` (apply) split. The two parsers strip the
//! exporter's computed/augmented fields (`comment_count`, `ready`, `blocked_by`,
//! …) so the remaining object deserializes cleanly against
//! [`ItemFrontmatter`]'s `deny_unknown_fields`. Format/schema versioning is
//! checked here so a future clove can add a migration without changing callers.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use clove_core::{ItemStore, RestoreOutcome};
use clove_types::{Item, ItemFrontmatter, CURRENT_SCHEMA_VERSION};
use serde::Serialize;
use serde_json::Value;

use crate::error::ImportError;
use crate::plan::{PlanItem, SkipItem};

/// The version of the export **container** format this build emits and can read.
///
/// Distinct from [`clove_types::CURRENT_SCHEMA_VERSION`] (the per-item
/// frontmatter schema): this versions the envelope/provenance that
/// [`crate::export::export_json`] stamps into `_meta.clove_export`, so a future
/// change to the export shape can be detected and refused (or migrated) here.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// Keys the exporter *adds* to the stored frontmatter (DESIGN §7.4 / the
/// `clove` crate's `item_json::export_object`). They are computed at export
/// time, never stored, and must be removed before deserializing back into an
/// [`ItemFrontmatter`] (whose `deny_unknown_fields` would otherwise reject them).
const COMPUTED_KEYS: &[&str] = &[
    "comment_count",
    "ready",
    "blocked_by",
    "dangling_deps",
    "children_summary",
    "warnings",
];

/// Parse a `clove export json` document: the standard `{ v, ok, data, _meta }`
/// envelope whose `data` is the array of export item objects.
///
/// The container `_meta.clove_export.format` (when present) is checked first: an
/// export produced by a newer clove is refused outright with
/// [`ImportError::Incompatible`]. Each `data` entry is then decoded; an entry
/// that fails to decode (malformed shape, a per-item `schema` newer than this
/// build) is collected as a warning and skipped so one bad item never aborts the
/// batch. Returns `(items, warnings)`.
pub fn parse_export_json(bytes: &str) -> Result<(Vec<Item>, Vec<String>), ImportError> {
    let envelope: Value = serde_json::from_str(bytes).map_err(|source| ImportError::Record {
        message: format!("invalid export JSON: {source}"),
    })?;

    // Container-level format gate (only the json envelope carries it).
    if let Some(format) = envelope
        .get("_meta")
        .and_then(|m| m.get("clove_export"))
        .and_then(|c| c.get("format"))
        .and_then(Value::as_u64)
    {
        if format > u64::from(EXPORT_FORMAT_VERSION) {
            return Err(ImportError::Incompatible {
                message: format!(
                    "export produced by a newer clove (format {format}); upgrade clove"
                ),
            });
        }
    }

    let data = envelope
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ImportError::Record {
            message: "export JSON has no `data` array".to_owned(),
        })?;

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for (index, entry) in data.iter().enumerate() {
        match item_from_object(entry.clone()) {
            Ok(item) => items.push(item),
            Err(err) => warnings.push(format!("data[{index}]: {err}")),
        }
    }
    Ok((items, warnings))
}

/// Parse a `clove export jsonl` document: one bare export item object per
/// non-empty line, with no envelope wrapper (so no container `format` is
/// available — per-item `schema` is the only version signal).
///
/// A line that is not valid JSON, or whose object fails to decode, is collected
/// as a warning and skipped; the rest still parse. Returns `(items, warnings)`.
pub fn parse_export_jsonl(bytes: &str) -> Result<(Vec<Item>, Vec<String>), ImportError> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for (index, line) in bytes.lines().enumerate() {
        let lineno = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => match item_from_object(value) {
                Ok(item) => items.push(item),
                Err(err) => warnings.push(format!("line {lineno}: {err}")),
            },
            Err(source) => warnings.push(format!("line {lineno}: invalid JSON: {source}")),
        }
    }
    Ok((items, warnings))
}

/// Decode one export item object into an [`Item`]: pull out `body`, strip the
/// exporter's computed keys, run the schema-migration seam, then deserialize the
/// remaining object as an [`ItemFrontmatter`].
fn item_from_object(mut value: Value) -> Result<Item, ImportError> {
    let Some(map) = value.as_object_mut() else {
        return Err(ImportError::Record {
            message: "export item is not a JSON object".to_owned(),
        });
    };

    let body = match map.remove("body") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text,
        Some(other) => {
            return Err(ImportError::Record {
                message: format!("`body` must be a string, got {other}"),
            })
        }
    };

    for key in COMPUTED_KEYS {
        map.remove(*key);
    }

    // The `map` borrow ends here (NLL); `value` is free to move below.
    migrate_frontmatter_value(&mut value)?;

    let frontmatter: ItemFrontmatter =
        serde_json::from_value(value).map_err(|source| ImportError::Record {
            message: source.to_string(),
        })?;
    Ok(Item { frontmatter, body })
}

/// The per-item **schema migration seam**.
///
/// Reads the object's `schema` (missing → [`CURRENT_SCHEMA_VERSION`]):
/// - newer than this build → [`ImportError::Incompatible`];
/// - equal to current → identity (no rewrite);
/// - older than current → a future migration would rewrite `obj` in place here.
///   Unreachable today (`CURRENT_SCHEMA_VERSION == 1`, the lowest version), this
///   is the single point a `v1 → v2` migration hooks in when the schema bumps.
fn migrate_frontmatter_value(obj: &mut Value) -> Result<(), ImportError> {
    let schema = obj
        .get("schema")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(CURRENT_SCHEMA_VERSION);

    match schema.cmp(&CURRENT_SCHEMA_VERSION) {
        Ordering::Greater => Err(ImportError::Incompatible {
            message: format!(
                "item schema {schema} newer than this clove (supports {CURRENT_SCHEMA_VERSION})"
            ),
        }),
        Ordering::Equal => Ok(()),
        Ordering::Less => {
            // Future migrations rewrite `obj` from its older schema to current
            // here, then fall through. No older schema exists yet.
            Ok(())
        }
    }
}

/// A summary of what an [`apply_restore`] run actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    /// Items written under an id that did not previously exist.
    pub created: usize,
    /// Items skipped because their id already existed and `overwrite` was false.
    pub skipped: usize,
    /// Items that replaced an existing id (`overwrite` was true).
    pub overwritten: usize,
}

/// The write-free plan a restore would carry out — the `--dry-run` payload.
///
/// Mirrors [`crate::ImportPlan`]'s `{ would_create, would_skip }` shape, adding a
/// `would_overwrite` bucket (populated only when `overwrite` is set; otherwise
/// existing ids land in `would_skip`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RestorePlan {
    /// Items whose id does not yet exist and would be written.
    pub would_create: Vec<PlanItem>,
    /// Items whose id already exists and would be left untouched (no overwrite).
    pub would_skip: Vec<SkipItem>,
    /// Items whose id already exists and would be replaced (`overwrite`).
    pub would_overwrite: Vec<PlanItem>,
}

/// Compute the write-free [`RestorePlan`] for restoring `items` into `store`.
pub fn plan_restore(
    items: &[Item],
    store: &ItemStore,
    overwrite: bool,
) -> Result<RestorePlan, ImportError> {
    let mut plan = RestorePlan::default();
    for item in items {
        let id = item.frontmatter.id.to_string();
        let title = item.frontmatter.title.clone();
        if store.exists(&item.frontmatter.id) {
            if overwrite {
                plan.would_overwrite.push(PlanItem { id, title });
            } else {
                plan.would_skip.push(SkipItem {
                    id,
                    reason: "id_exists".to_owned(),
                });
            }
        } else {
            plan.would_create.push(PlanItem { id, title });
        }
    }
    Ok(plan)
}

/// Restore `items` into `store` via the unified verbatim write path
/// ([`ItemStore::restore_item`]), tallying the outcome of each.
pub fn apply_restore(
    items: &[Item],
    store: &ItemStore,
    overwrite: bool,
    now: DateTime<Utc>,
) -> Result<RestoreReport, ImportError> {
    let mut report = RestoreReport::default();
    for item in items {
        match store.restore_item(item, overwrite, now)? {
            RestoreOutcome::Created => report.created += 1,
            RestoreOutcome::Skipped => report.skipped += 1,
            RestoreOutcome::Overwritten => report.overwritten += 1,
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_core::view::item_object;
    use clove_types::{CloveId, ItemStatus, ItemType, Priority};
    use serde_json::{json, Map, Value};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// A small, varied fixture set exercising deps/parent/labels/status/closed.
    fn sample_items() -> Vec<Item> {
        let a = Item {
            frontmatter: ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: CloveId::new("proj-AAAAAAAA").unwrap(),
                title: "First".to_owned(),
                status: ItemStatus::Open,
                item_type: ItemType::Feature,
                priority: Priority(1),
                created: ts("2026-01-01T00:00:00Z"),
                updated: ts("2026-01-02T00:00:00Z"),
                closed: None,
                assignee: Some("ege".to_owned()),
                parent: None,
                labels: vec!["area:core".to_owned()],
                deps: Vec::new(),
                relates: Vec::new(),
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: None,
                external_ref: None,
            },
            body: "First body.\n".to_owned(),
        };
        let b = Item {
            frontmatter: ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: CloveId::new("proj-BBBBBBBB").unwrap(),
                title: "Second".to_owned(),
                status: ItemStatus::Closed,
                item_type: ItemType::Bug,
                priority: Priority(0),
                created: ts("2026-01-01T00:00:00Z"),
                updated: ts("2026-03-03T00:00:00Z"),
                closed: Some(ts("2026-03-03T00:00:00Z")),
                assignee: None,
                parent: Some(CloveId::new("proj-AAAAAAAA").unwrap()),
                labels: vec!["area:core".to_owned(), "perf".to_owned()],
                deps: vec![CloveId::new("proj-AAAAAAAA").unwrap()],
                relates: Vec::new(),
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: Some("github".to_owned()),
                external_ref: Some("gh-7".to_owned()),
            },
            body: "Second body.\n".to_owned(),
        };
        let c = Item {
            frontmatter: ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: CloveId::new("proj-CCCCCCCC").unwrap(),
                title: "Third".to_owned(),
                status: ItemStatus::InProgress,
                item_type: ItemType::Chore,
                priority: Priority(3),
                created: ts("2026-02-01T00:00:00Z"),
                updated: ts("2026-02-01T00:00:00Z"),
                closed: None,
                assignee: None,
                parent: None,
                labels: Vec::new(),
                deps: vec![CloveId::new("proj-BBBBBBBB").unwrap()],
                relates: vec![CloveId::new("proj-AAAAAAAA").unwrap()],
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: None,
                external_ref: None,
            },
            body: String::new(),
        };
        vec![a, b, c]
    }

    /// Shape one item the way the exporter does: serialized frontmatter + `body`
    /// + a fake computed field that restore must strip.
    fn export_object(item: &Item) -> Map<String, Value> {
        let mut obj = item_object(item);
        obj.insert("body".to_owned(), json!(item.body));
        // Computed/augmented fields the exporter adds (must be stripped on read).
        obj.insert("comment_count".to_owned(), json!(2));
        obj.insert("ready".to_owned(), json!(true));
        obj.insert("blocked_by".to_owned(), json!(["proj-ZZZZZZZZ"]));
        obj.insert("dangling_deps".to_owned(), json!([]));
        obj
    }

    #[test]
    fn json_roundtrip_preserves_every_item_verbatim() {
        let originals = sample_items();
        let objects: Vec<Value> = originals
            .iter()
            .map(|i| Value::Object(export_object(i)))
            .collect();
        let envelope = json!({
            "v": 1,
            "ok": true,
            "data": objects,
            "_meta": { "clove_export": { "format": 1, "item_schema": 1 } },
        });
        let text = serde_json::to_string(&envelope).unwrap();

        let (parsed, warnings) = parse_export_json(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(parsed, originals, "computed fields ignored, items verbatim");
    }

    #[test]
    fn jsonl_roundtrip_preserves_every_item_verbatim() {
        let originals = sample_items();
        let mut text = String::new();
        for item in &originals {
            text.push_str(&serde_json::to_string(&export_object(item)).unwrap());
            text.push('\n');
        }

        let (parsed, warnings) = parse_export_jsonl(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(parsed, originals);
    }

    #[test]
    fn newer_container_format_is_rejected() {
        let envelope = json!({
            "v": 1,
            "ok": true,
            "data": [],
            "_meta": { "clove_export": { "format": EXPORT_FORMAT_VERSION + 1 } },
        });
        let err = parse_export_json(&serde_json::to_string(&envelope).unwrap()).unwrap_err();
        assert!(matches!(err, ImportError::Incompatible { .. }));
        assert!(err.to_string().contains("newer clove"));
    }

    #[test]
    fn newer_item_schema_becomes_a_warning_not_a_batch_failure() {
        let good = &sample_items()[0];
        let mut newer = export_object(good);
        newer.insert("schema".to_owned(), json!(CURRENT_SCHEMA_VERSION + 1));
        let mut text = serde_json::to_string(&Value::Object(newer)).unwrap();
        text.push('\n');
        text.push_str(&serde_json::to_string(&export_object(good)).unwrap());
        text.push('\n');

        let (parsed, warnings) = parse_export_jsonl(&text).unwrap();
        assert_eq!(parsed.len(), 1, "the readable item still parses");
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("newer than this clove"),
            "{warnings:?}"
        );
    }

    #[test]
    fn malformed_line_warns_and_others_parse() {
        let items = sample_items();
        let mut text = String::new();
        text.push_str(&serde_json::to_string(&export_object(&items[0])).unwrap());
        text.push('\n');
        text.push_str("{ this is not json\n");
        text.push('\n'); // a blank line is skipped silently
        text.push_str(&serde_json::to_string(&export_object(&items[1])).unwrap());
        text.push('\n');

        let (parsed, warnings) = parse_export_jsonl(&text).unwrap();
        assert_eq!(parsed.len(), 2, "the two good lines still parse");
        assert_eq!(warnings.len(), 1, "only the malformed line warns");
        assert!(warnings[0].contains("invalid JSON"));
    }

    #[test]
    fn plan_and_apply_restore_into_a_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap().to_owned();
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
        let store = ItemStore::new(root);

        let items = sample_items();
        let now = ts("2026-07-01T00:00:00Z");

        // Dry-run: all three would be created.
        let plan = plan_restore(&items, &store, false).unwrap();
        assert_eq!(plan.would_create.len(), 3);
        assert!(plan.would_skip.is_empty());
        assert!(plan.would_overwrite.is_empty());

        // Apply: all created.
        let report = apply_restore(&items, &store, false, now).unwrap();
        assert_eq!(report.created, 3);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.overwritten, 0);

        // A re-read of one item matches the original byte-for-byte in memory.
        let reread = store.get(&items[1].frontmatter.id).unwrap();
        assert_eq!(reread, items[1]);

        // Second apply without overwrite skips all; with overwrite, replaces all.
        let report = apply_restore(&items, &store, false, now).unwrap();
        assert_eq!(report.skipped, 3);
        let report = apply_restore(&items, &store, true, now).unwrap();
        assert_eq!(report.overwritten, 3);

        // Plan with overwrite routes existing ids to would_overwrite.
        let plan = plan_restore(&items, &store, true).unwrap();
        assert_eq!(plan.would_overwrite.len(), 3);
        assert!(plan.would_skip.is_empty());
    }
}
