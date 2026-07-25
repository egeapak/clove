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

/// Per-surface page-size defaults.
///
/// These are the *only* place a read default may be written. The numbers differ
/// on purpose — a terminal, an agent's context budget, and a browser that
/// virtualizes the whole store have genuinely different cost functions — but the
/// *semantics* around them are identical everywhere, and every list response
/// echoes the effective limit back in `_meta.limit` so the default is never
/// folklore.
pub mod defaults {
    /// One screenful of terminal scrollback; `_meta.total` still reports the
    /// full match count.
    pub const CLI_LIMIT: usize = 100;
    /// An agent's context budget: 50 full items is already ~20 KB.
    pub const MCP_LIMIT: usize = 50;
    /// Unlimited. The bundled SPA fetches the store once and virtualizes, and
    /// the endpoint already scans and graphs everything per request, so a
    /// serialization cap would buy nothing and silently truncate the UI.
    pub const WEB_LIMIT: usize = 0;
    /// `clove dep tree` / `clove_dep_tree` / `GET /deptree`.
    pub const DEP_TREE_DEPTH: usize = 5;
    /// `clove stats --top`.
    pub const STATS_TOP: usize = 10;
    /// `clove comments` / `clove_comments`.
    pub const COMMENTS_LIMIT: usize = 50;
}

/// An offset/limit window over a result set.
///
/// The single decoding of the limit contract, shared by every surface:
/// **absent → that surface's default; `0` → unlimited; `n` → at most `n`**.
/// Before this existed the three surfaces each parsed it inline and disagreed —
/// most visibly, `?limit=0` on the web API returned *zero* rows where the CLI
/// and MCP returned *everything*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub offset: usize,
    /// `None` is unlimited.
    pub limit: Option<usize>,
}

impl Page {
    /// Decode a raw `--limit`/`?limit=`/`"limit"` value against a default.
    pub fn new(offset: usize, raw_limit: Option<usize>, default: usize) -> Page {
        let limit = match raw_limit.unwrap_or(default) {
            0 => None,
            n => Some(n),
        };
        Page { offset, limit }
    }

    /// Everything, from the start.
    pub fn unlimited() -> Page {
        Page {
            offset: 0,
            limit: None,
        }
    }

    /// Apply the window, returning `(page, total)` where `total` is always the
    /// match count *before* pagination.
    pub fn apply<T>(&self, all: Vec<T>) -> (Vec<T>, usize) {
        let total = all.len();
        let page = all
            .into_iter()
            .skip(self.offset)
            .take(self.limit.unwrap_or(usize::MAX))
            .collect();
        (page, total)
    }

    /// The effective limit as it appears in `_meta.limit`: `0` for unlimited,
    /// matching the encoding callers pass in.
    pub fn reported_limit(&self) -> usize {
        self.limit.unwrap_or(0)
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

/// Drop keys whose value carries no information: JSON `null` and empty arrays.
///
/// Booleans (including `false`), numbers, and strings (including `""`) are
/// always kept — `ready: false` and `body: ""` are answers, not absences, and
/// dropping them would turn a definite result into an ambiguity.
///
/// This is a *presentation* filter for token-sensitive consumers (the MCP read
/// tools). [`frontmatter_object`] is deliberately left alone: the CLI's human
/// renderer, the web DTOs, `export json`, and the GitHub sync fingerprints all
/// depend on the full-key shape. Absent keys are v1-legal — only
/// `id`/`title`/`status`/`type`/`priority`/`created`/`updated` are `required` in
/// `item.json`, and the index-backed `clove ls` path has always returned a
/// reduced shape.
pub fn compact(obj: Map<String, Value>) -> Map<String, Value> {
    obj.into_iter()
        .filter_map(|(key, value)| match value {
            Value::Null => None,
            Value::Array(items) if items.is_empty() => None,
            Value::Array(items) => Some((
                key,
                Value::Array(
                    items
                        .into_iter()
                        .map(|item| match item {
                            Value::Object(inner) => Value::Object(compact(inner)),
                            other => other,
                        })
                        .collect(),
                ),
            )),
            Value::Object(inner) => Some((key, Value::Object(compact(inner)))),
            other => Some((key, other)),
        })
        .collect()
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
    fn page_decodes_the_limit_contract() {
        // Absent → the surface default.
        assert_eq!(Page::new(0, None, 100).limit, Some(100));
        // 0 → unlimited, everywhere. This is the case the web API used to read
        // as "return nothing".
        assert_eq!(Page::new(0, Some(0), 100).limit, None);
        // n → n, and n may exceed the default.
        assert_eq!(Page::new(0, Some(7), 100).limit, Some(7));
        assert_eq!(Page::new(0, Some(500), 100).limit, Some(500));
        // A default of 0 means the surface is unlimited by default.
        assert_eq!(Page::new(0, None, 0).limit, None);
    }

    #[test]
    fn page_reports_total_before_pagination() {
        let all: Vec<u8> = (0..10).collect();
        let (rows, total) = Page::new(2, Some(3), 100).apply(all.clone());
        assert_eq!(rows, vec![2, 3, 4]);
        assert_eq!(total, 10, "total is the match count, not the page length");

        // An offset past the end is an empty page, not a panic.
        let (rows, total) = Page::new(99, Some(3), 100).apply(all.clone());
        assert!(rows.is_empty());
        assert_eq!(total, 10);

        // Unlimited ignores the limit but still honours the offset.
        let (rows, total) = Page::new(8, Some(0), 100).apply(all);
        assert_eq!(rows, vec![8, 9]);
        assert_eq!(total, 10);
    }

    #[test]
    fn reported_limit_round_trips_the_wire_encoding() {
        assert_eq!(Page::new(0, Some(0), 100).reported_limit(), 0);
        assert_eq!(Page::new(0, Some(25), 100).reported_limit(), 25);
        assert_eq!(Page::new(0, None, 50).reported_limit(), 50);
    }

    #[test]
    fn compact_drops_null_and_empty_lists() {
        let f = fm("hi", ItemStatus::Open, ItemType::Bug, 0, &[]);
        let out = compact(frontmatter_object(&f));
        for gone in ["assignee", "parent", "closed", "labels", "deps", "relates"] {
            assert!(!out.contains_key(gone), "{gone} should have been dropped");
        }
        // Real values stay.
        assert_eq!(out["title"], "hi");
        assert_eq!(out["priority"], 0);
    }

    /// The load-bearing negative: `false` and `""` are answers, not absences.
    /// Dropping `ready: false` would make "definitely not ready" indistinguishable
    /// from "not computed".
    #[test]
    fn compact_keeps_false_zero_and_empty_string() {
        let mut obj = Map::new();
        obj.insert("ready".to_owned(), serde_json::json!(false));
        obj.insert("body".to_owned(), serde_json::json!(""));
        obj.insert("priority".to_owned(), serde_json::json!(0));
        obj.insert("comment_count".to_owned(), serde_json::json!(0));
        let out = compact(obj);
        assert_eq!(out.len(), 4, "nothing informative may be dropped: {out:?}");
    }

    #[test]
    fn compact_recurses_into_nested_objects_and_arrays() {
        let obj = serde_json::json!({
            "items": [{ "id": "a", "children": [], "parent": null }],
            "tree": { "id": "root", "children": [] },
        });
        let Value::Object(map) = obj else {
            unreachable!()
        };
        let out = compact(map);
        let item = &out["items"][0];
        assert_eq!(item["id"], "a");
        assert!(item.get("children").is_none(), "empty child list dropped");
        assert!(item.get("parent").is_none(), "null dropped inside an array");
        assert!(
            out["tree"].get("children").is_none(),
            "nested object recursed"
        );
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
