//! Beads exporter: the inverse of [`crate::beads`], producing a beads-native
//! `issues.jsonl` (NDJSON) from clove items so `clove export beads` yields a file
//! isomorphic with `.beads/issues.jsonl` (readable by `bd` *and* by clove's own
//! `import beads`). Pure and always compiled — no network, no filesystem here (the
//! caller supplies the items and the writer).
//!
//! ## Field mapping (clove → beads, the inverse of the importer's table)
//!
//! | clove | beads |
//! |---|---|
//! | `id` | `id` |
//! | `title` | `title` |
//! | body | `description` |
//! | `status` (`open`/`in_progress`/`closed`) | `status`; a `deferred` label on an open item becomes `status:"deferred"` (inverse of the importer) |
//! | `priority` | `priority` (number) |
//! | `type` (`chore` → `task`) | `issue_type` |
//! | `assignee` | `assignee` |
//! | `labels` (minus an extracted `deferred`) | `labels` |
//! | `deps` | `dependencies[{type:"blocks"}]` |
//! | `parent` | `dependencies[{type:"parent-child"}]` |
//! | `relates` | `dependencies[{type:"related"}]` |
//! | `duplicates` / `supersedes` | flat `duplicates` / `supersedes` arrays |
//! | `external_ref` | `external_ref` (preserved verbatim → idempotent re-import) |
//! | `source_system` | `source_system` (emitted for `bd` compatibility; **not** read back — `import beads` re-stamps `"beads"`) |
//! | `created` / `updated` / `closed` | same (informational; the importer re-stamps) |
//!
//! ## Round-trip
//!
//! `clove export beads > issues.jsonl` then `clove import beads issues.jsonl`
//! reproduces every mapped field *except the provenance stamps* (`source_system`
//! and the timestamps, which `import beads` re-derives). `deps`/`parent`/`relates`
//! ride beads' structured
//! `dependencies[]` (so `bd` understands them); `duplicates`/`supersedes` — which
//! have no beads-native edge kind — ride the top-level flat arrays the importer
//! always reads, so they are not lost. `external_ref` is preserved verbatim, so a
//! re-import is idempotent (skips the already-imported item) rather than
//! duplicating it.

use std::io::{self, Write};

use clove_types::{Item, ItemStatus, ItemType};
use serde_json::{json, Map, Value};

/// The label the importer injects for a beads `deferred` status. Extracted back
/// out on export so the round-trip is symmetric.
const DEFERRED_LABEL: &str = "deferred";

/// The beads-native `issue_type` string for a clove [`ItemType`]. The inverse of
/// [`crate::map::beads_type`] — notably `chore → task`, since beads' native word
/// is `task` (the importer maps it back to `chore`).
fn beads_issue_type(item_type: ItemType) -> &'static str {
    match item_type {
        ItemType::Chore => "task",
        ItemType::Bug => "bug",
        ItemType::Feature => "feature",
        ItemType::Docs => "docs",
        ItemType::Epic => "epic",
    }
}

/// A structured beads dependency edge `{ "id": <id>, "type": <kind> }`.
fn dep_edge(id: &str, kind: &str) -> Value {
    json!({ "id": id, "type": kind })
}

/// Shape one clove [`Item`] into a beads-native issue object.
pub fn build_beads_object(item: &Item) -> Map<String, Value> {
    let fm = &item.frontmatter;

    // Deferred inverse: an open item carrying the `deferred` label was originally
    // a beads `deferred` status. Re-emit that status and drop the synthetic label
    // so the round-trip is symmetric.
    let has_deferred =
        fm.status == ItemStatus::Open && fm.labels.iter().any(|l| l == DEFERRED_LABEL);
    let status = if has_deferred {
        "deferred".to_owned()
    } else {
        fm.status.as_str().to_owned()
    };
    let labels: Vec<&String> = fm
        .labels
        .iter()
        .filter(|l| !(has_deferred && l.as_str() == DEFERRED_LABEL))
        .collect();

    // Structured beads dependency edges for the kinds beads models natively.
    let mut dependencies: Vec<Value> = Vec::new();
    for dep in &fm.deps {
        dependencies.push(dep_edge(dep.as_str(), "blocks"));
    }
    if let Some(parent) = &fm.parent {
        dependencies.push(dep_edge(parent.as_str(), "parent-child"));
    }
    for rel in &fm.relates {
        dependencies.push(dep_edge(rel.as_str(), "related"));
    }

    let mut obj = Map::new();
    obj.insert("id".to_owned(), json!(fm.id.as_str()));
    obj.insert("title".to_owned(), json!(fm.title));
    obj.insert("description".to_owned(), json!(item.body));
    obj.insert("status".to_owned(), json!(status));
    obj.insert("priority".to_owned(), json!(fm.priority.get()));
    obj.insert(
        "issue_type".to_owned(),
        json!(beads_issue_type(fm.item_type)),
    );
    if let Some(assignee) = &fm.assignee {
        obj.insert("assignee".to_owned(), json!(assignee));
    }
    obj.insert("labels".to_owned(), json!(labels));
    obj.insert("dependencies".to_owned(), json!(dependencies));
    // `duplicates`/`supersedes` have no beads-native edge kind, so they ride the
    // flat arrays the importer always reads (keeping the round-trip lossless).
    if !fm.duplicates.is_empty() {
        let ids: Vec<&str> = fm.duplicates.iter().map(|d| d.as_str()).collect();
        obj.insert("duplicates".to_owned(), json!(ids));
    }
    if !fm.supersedes.is_empty() {
        let ids: Vec<&str> = fm.supersedes.iter().map(|d| d.as_str()).collect();
        obj.insert("supersedes".to_owned(), json!(ids));
    }
    if let Some(external_ref) = &fm.external_ref {
        obj.insert("external_ref".to_owned(), json!(external_ref));
    }
    if let Some(source_system) = &fm.source_system {
        obj.insert("source_system".to_owned(), json!(source_system));
    }
    obj.insert("created".to_owned(), json!(fm.created));
    obj.insert("updated".to_owned(), json!(fm.updated));
    if let Some(closed) = &fm.closed {
        obj.insert("closed".to_owned(), json!(closed));
    }
    // beads carries a comment count but not bodies; clove comments are not exported
    // here, so it is always 0 (present for beads-tool compatibility).
    obj.insert("comment_count".to_owned(), json!(0));
    obj
}

/// Write `items` as a beads-native NDJSON stream (one issue object per line, each
/// terminated by `\n`). An empty slice writes nothing.
pub fn export_beads<W: Write>(writer: &mut W, items: &[Item]) -> io::Result<()> {
    for item in items {
        let obj = build_beads_object(item);
        serde_json::to_writer(&mut *writer, &Value::Object(obj))?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use clove_types::model::CURRENT_SCHEMA_VERSION;
    use clove_types::{CloveId, ItemFrontmatter, Priority};

    fn item(build: impl FnOnce(&mut ItemFrontmatter)) -> Item {
        let now = Utc::now();
        let mut fm = ItemFrontmatter {
            schema: CURRENT_SCHEMA_VERSION,
            id: CloveId::new("proj-AAAA1111").unwrap(),
            title: "Title".to_owned(),
            status: ItemStatus::Open,
            item_type: ItemType::Feature,
            priority: Priority(2),
            created: now,
            updated: now,
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
        };
        build(&mut fm);
        Item {
            frontmatter: fm,
            body: "Body text.".to_owned(),
        }
    }

    #[test]
    fn maps_core_fields_to_beads_native() {
        let it = item(|fm| {
            fm.item_type = ItemType::Chore;
            fm.status = ItemStatus::InProgress;
            fm.priority = Priority(1);
            fm.assignee = Some("ege".to_owned());
            fm.labels = vec!["area:core".to_owned()];
        });
        let obj = build_beads_object(&it);
        assert_eq!(obj["id"], "proj-AAAA1111");
        assert_eq!(obj["description"], "Body text.");
        assert_eq!(obj["issue_type"], "task"); // chore → task (beads-native)
        assert_eq!(obj["status"], "in_progress");
        assert_eq!(obj["priority"], 1);
        assert_eq!(obj["assignee"], "ege");
        assert_eq!(obj["labels"], json!(["area:core"]));
        assert_eq!(obj["comment_count"], 0);
    }

    #[test]
    fn deferred_label_becomes_deferred_status() {
        let it = item(|fm| {
            fm.status = ItemStatus::Open;
            fm.labels = vec!["deferred".to_owned(), "area:core".to_owned()];
        });
        let obj = build_beads_object(&it);
        assert_eq!(obj["status"], "deferred");
        // The synthetic `deferred` label is dropped from the label list.
        assert_eq!(obj["labels"], json!(["area:core"]));
    }

    #[test]
    fn relations_map_to_structured_edges_and_flat_arrays() {
        let it = item(|fm| {
            fm.deps = vec![CloveId::new("proj-BBBB2222").unwrap()];
            fm.parent = Some(CloveId::new("proj-CCCC3333").unwrap());
            fm.relates = vec![CloveId::new("proj-DDDD4444").unwrap()];
            fm.duplicates = vec![CloveId::new("proj-EEEE5555").unwrap()];
            fm.supersedes = vec![CloveId::new("proj-FFFF6666").unwrap()];
        });
        let obj = build_beads_object(&it);
        let deps = obj["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 3);
        assert!(deps.contains(&dep_edge("proj-BBBB2222", "blocks")));
        assert!(deps.contains(&dep_edge("proj-CCCC3333", "parent-child")));
        assert!(deps.contains(&dep_edge("proj-DDDD4444", "related")));
        // duplicates/supersedes ride the flat arrays.
        assert_eq!(obj["duplicates"], json!(["proj-EEEE5555"]));
        assert_eq!(obj["supersedes"], json!(["proj-FFFF6666"]));
    }

    #[test]
    fn external_ref_is_preserved() {
        let it = item(|fm| fm.external_ref = Some("gh-42".to_owned()));
        let obj = build_beads_object(&it);
        assert_eq!(obj["external_ref"], "gh-42");
    }

    #[test]
    fn export_writes_ndjson_one_line_per_item() {
        let items = vec![
            item(|_| {}),
            item(|fm| fm.id = CloveId::new("proj-BBBB2222").unwrap()),
        ];
        let mut buf = Vec::new();
        export_beads(&mut buf, &items).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line is a standalone JSON object.
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            assert!(v.is_object());
        }
    }
}
