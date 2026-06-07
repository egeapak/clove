//! Shared read-side presentation: filters, list ordering, and item→JSON shaping.
//!
//! This is the single definition consumed by every read surface — the `clove`
//! CLI, the `clove-mcp` server, the daemon, and (later) the web UI — so they all
//! filter, sort, and serialize items identically. It is pure (no I/O); the JSON
//! it produces is the DESIGN §7.4 item shape, minus the response envelope.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    normalize_label, CloveError, CloveId, Item, ItemFrontmatter, ItemStatus, ItemType, Priority,
};

/// Parsed list filters. A `None` field does not constrain.
#[derive(Debug, Default, Clone)]
pub struct Filters {
    pub status: Option<ItemStatus>,
    pub item_type: Option<ItemType>,
    pub label: Option<String>,
    pub assignee: Option<String>,
    pub priority: Option<Priority>,
}

impl Filters {
    /// Build filters from raw strings, validating and canonicalizing each
    /// (status/type words, the label via [`normalize_label`], priority 0–4).
    pub fn parse(
        status: Option<&str>,
        item_type: Option<&str>,
        label: Option<&str>,
        assignee: Option<&str>,
        priority: Option<u8>,
    ) -> Result<Filters, CloveError> {
        Ok(Filters {
            status: status.map(ItemStatus::parse).transpose()?,
            item_type: item_type.map(ItemType::parse).transpose()?,
            label: label.map(normalize_label).transpose()?,
            assignee: assignee.map(str::to_owned),
            priority: priority.map(Priority::new).transpose()?,
        })
    }

    /// Whether `fm` satisfies every set constraint.
    pub fn matches(&self, fm: &ItemFrontmatter) -> bool {
        if let Some(s) = self.status {
            if fm.status != s {
                return false;
            }
        }
        if let Some(t) = self.item_type {
            if fm.item_type != t {
                return false;
            }
        }
        if let Some(p) = self.priority {
            if fm.priority != p {
                return false;
            }
        }
        if let Some(a) = &self.assignee {
            if fm.assignee.as_deref() != Some(a.as_str()) {
                return false;
            }
        }
        if let Some(l) = &self.label {
            if !fm.labels.iter().any(|x| x == l) {
                return false;
            }
        }
        true
    }
}

/// Sort frontmatter in place by `(priority, topological_rank, id)` — the
/// canonical list order shared by the file and index paths.
pub fn sort_by_rank(items: &mut [ItemFrontmatter], ranks: &HashMap<CloveId, usize>) {
    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| rank_of(ranks, &a.id).cmp(&rank_of(ranks, &b.id)))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// A topological rank lookup that sorts unknown ids last.
pub fn rank_of(ranks: &HashMap<CloveId, usize>, id: &CloveId) -> usize {
    ranks.get(id).copied().unwrap_or(usize::MAX)
}

/// The JSON object for an item's frontmatter alone (the list fast path, which
/// never reads bodies): `id`, `title`, `status`, `type`, `priority`, timestamps,
/// `labels`, `deps`, ….
pub fn frontmatter_object(fm: &ItemFrontmatter) -> Map<String, Value> {
    match serde_json::to_value(fm) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// The base JSON object for an item: exactly its serialized frontmatter.
pub fn item_object(item: &Item) -> Map<String, Value> {
    frontmatter_object(&item.frontmatter)
}

/// Restrict `obj` to the keys named in `fields`. Unknown field names are
/// ignored. (Key order in the result follows `serde_json::Map`, which is a
/// sorted `BTreeMap` unless the `preserve_order` feature is enabled.)
pub fn project(obj: Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    for field in fields {
        if let Some(value) = obj.get(field) {
            out.insert(field.clone(), value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn fm(title: &str, status: ItemStatus, t: ItemType, p: u8, labels: &[&str]) -> ItemFrontmatter {
        let now = Utc::now();
        ItemFrontmatter {
            schema: 1,
            id: CloveId::new("proj-0000000A").unwrap(),
            title: title.to_owned(),
            status,
            item_type: t,
            priority: Priority(p),
            created: now,
            updated: now,
            closed: None,
            assignee: None,
            parent: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            deps: Vec::new(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
    }

    #[test]
    fn filters_match_each_dimension() {
        let f = fm("a", ItemStatus::Open, ItemType::Bug, 1, &["area:core"]);
        assert!(Filters::parse(Some("open"), None, None, None, None)
            .unwrap()
            .matches(&f));
        assert!(!Filters::parse(Some("closed"), None, None, None, None)
            .unwrap()
            .matches(&f));
        assert!(Filters::parse(None, Some("bug"), None, None, Some(1))
            .unwrap()
            .matches(&f));
        // Label filter is canonicalized before matching.
        assert!(Filters::parse(None, None, Some("Area:Core"), None, None)
            .unwrap()
            .matches(&f));
    }

    #[test]
    fn status_aliases_parse() {
        assert_eq!(
            ItemStatus::parse("started").unwrap(),
            ItemStatus::InProgress
        );
        assert_eq!(ItemStatus::parse("done").unwrap(), ItemStatus::Closed);
        assert!(ItemStatus::parse("nope").is_err());
    }

    #[test]
    fn frontmatter_object_has_core_fields() {
        let f = fm("hello", ItemStatus::Open, ItemType::Feature, 2, &[]);
        let obj = frontmatter_object(&f);
        assert_eq!(obj["title"], "hello");
        assert_eq!(obj["status"], "open");
        assert_eq!(obj["type"], "feature");
        assert_eq!(obj["priority"], 2);
        // Empty list fields serialize as `[]` (not absent) per §7.4.
        assert_eq!(obj["labels"], serde_json::json!([]));
        assert_eq!(obj["deps"], serde_json::json!([]));
    }

    #[test]
    fn empty_filters_match_everything() {
        let f = fm("a", ItemStatus::Closed, ItemType::Epic, 4, &[]);
        assert!(Filters::default().matches(&f));
        assert!(Filters::parse(None, None, None, None, None)
            .unwrap()
            .matches(&f));
    }

    #[test]
    fn parse_rejects_invalid_values() {
        // Negative: out-of-range priority and unknown status/type words.
        assert!(Filters::parse(None, None, None, None, Some(5)).is_err());
        assert!(Filters::parse(Some("paused"), None, None, None, None).is_err());
        assert!(Filters::parse(None, Some("saga"), None, None, None).is_err());
        // Negative: an all-whitespace label canonicalizes to empty → rejected.
        assert!(Filters::parse(None, None, Some("   "), None, None).is_err());
    }

    #[test]
    fn assignee_filter_is_exact() {
        let mut f = fm("a", ItemStatus::Open, ItemType::Bug, 1, &[]);
        f.assignee = Some("alice".to_owned());
        assert!(Filters::parse(None, None, None, Some("alice"), None)
            .unwrap()
            .matches(&f));
        assert!(!Filters::parse(None, None, None, Some("bob"), None)
            .unwrap()
            .matches(&f));
        // Edge: a substring of the assignee must not match.
        assert!(!Filters::parse(None, None, None, Some("alic"), None)
            .unwrap()
            .matches(&f));
    }

    #[test]
    fn sort_orders_by_priority_then_rank_then_id() {
        let mut a = fm("a", ItemStatus::Open, ItemType::Bug, 2, &[]);
        a.id = CloveId::new("proj-0000000A").unwrap();
        let mut b = fm("b", ItemStatus::Open, ItemType::Bug, 1, &[]);
        b.id = CloveId::new("proj-0000000B").unwrap();
        let mut c = fm("c", ItemStatus::Open, ItemType::Bug, 1, &[]);
        c.id = CloveId::new("proj-0000000C").unwrap();

        let mut ranks = HashMap::new();
        ranks.insert(b.id.clone(), 5usize);
        ranks.insert(c.id.clone(), 1usize); // a's rank is intentionally absent

        let mut items = vec![a.clone(), b.clone(), c.clone()];
        sort_by_rank(&mut items, &ranks);
        // priority 1 before priority 2; within p1, lower rank (c) before b.
        let order: Vec<&str> = items.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            order,
            vec!["proj-0000000C", "proj-0000000B", "proj-0000000A"]
        );
        // Edge: a missing rank sorts last (usize::MAX) — a is p2 anyway, but
        // rank_of must report MAX for the absent id.
        assert_eq!(rank_of(&ranks, &a.id), usize::MAX);
    }

    #[test]
    fn project_keeps_named_fields_and_ignores_unknown() {
        let f = fm("hi", ItemStatus::Open, ItemType::Bug, 0, &[]);
        let obj = frontmatter_object(&f);
        let projected = project(
            obj,
            &[
                "status".to_owned(),
                "id".to_owned(),
                "nonexistent".to_owned(),
            ],
        );
        // Exactly the two known fields survive; the unknown key is dropped.
        // (serde_json's Map is a sorted BTreeMap by default, so key *order* is
        // not significant here — only membership.)
        assert_eq!(projected.len(), 2);
        assert!(projected.contains_key("status"));
        assert!(projected.contains_key("id"));
        assert!(!projected.contains_key("nonexistent"));
    }

    #[test]
    fn project_empty_fields_yields_empty_object() {
        let f = fm("hi", ItemStatus::Open, ItemType::Bug, 0, &[]);
        let projected = project(frontmatter_object(&f), &[]);
        assert!(projected.is_empty());
    }
}
