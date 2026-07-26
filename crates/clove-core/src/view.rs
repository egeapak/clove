//! Shared read-side presentation: filters, list ordering, and item→JSON shaping.
//!
//! This is the single definition consumed by every read surface — the `clove`
//! CLI, the `clove-mcp` server, the daemon, and (later) the web UI — so they all
//! filter, sort, and serialize items identically. It is pure (no I/O); the JSON
//! it produces is the DESIGN §7.4 item shape, minus the response envelope.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
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
/// A comment thread is a list like any other, so `clove comments` /
/// `clove_comments` / `GET /items/:id/comments` page on the *same* per-surface
/// default as the item lists rather than on one of their own.
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
    /// `clove dep tree` / `clove_dep_tree` / `GET /deptree`. `0` is unlimited
    /// here too, matching the page limits (`clove dep tree --full`).
    pub const DEP_TREE_DEPTH: usize = 5;
    /// `clove stats --top`.
    pub const STATS_TOP: usize = 10;
}

/// An offset/limit window over a result set.
///
/// The single decoding of the limit contract, shared by every surface:
/// **absent → that surface's default; `0` → unlimited; `n` → at most `n`**.
/// Before this existed the three surfaces each parsed it inline and disagreed —
/// most visibly, `?limit=0` on the web API returned *zero* rows where the CLI
/// and MCP returned *everything*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

    /// How many rows a SQL `LIMIT` must fetch for this window: `offset + limit`,
    /// since the offset is applied after the query. `None` is unlimited.
    ///
    /// Clamped to `i64::MAX` because SQLite binds integers as `i64` — a window
    /// near the top of the `usize` range is a legal (empty) page that the file
    /// path answers with `[]`, and without the clamp the index path answered
    /// the same query with a `datatype mismatch` reported as `IO_ERROR`.
    pub fn sql_fetch(&self) -> Option<usize> {
        self.limit
            .map(|n| self.offset.saturating_add(n).min(i64::MAX as usize))
    }
}

/// Where a search needle was found in an item, best match first.
///
/// The single definition of "what counts as a hit, and how well" — shared by
/// `ops::search` (the MCP tool) and `clove search`'s file and index paths, which
/// previously disagreed on both. The CLI matched title and body only and ranked
/// in two classes; `ops::search` matched labels too and ranked in three, so a
/// label-only hit was found by the MCP tool and by neither CLI path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchClass {
    /// In the title — the strongest signal, ranked first.
    Title,
    /// In a label.
    Label,
    /// Somewhere in the body.
    Body,
}

/// Classify `item` against an already-lowercased `needle`, or `None` for no hit.
///
/// `needle` is taken pre-lowered so a caller scanning a whole store lowers it
/// once rather than per item.
pub fn match_class(item: &Item, needle: &str) -> Option<MatchClass> {
    let fm = &item.frontmatter;
    if fm.title.to_lowercase().contains(needle) {
        Some(MatchClass::Title)
    } else if fm.labels.iter().any(|l| l.to_lowercase().contains(needle)) {
        Some(MatchClass::Label)
    } else if item.body.to_lowercase().contains(needle) {
        Some(MatchClass::Body)
    } else {
        None
    }
}

/// Order search hits by `(match class, priority, id)` — a *total* order.
///
/// Ranking by match class alone left ties in the caller's input order, which for
/// the file paths is raw `read_dir` order: undefined, and it reshuffles when a
/// file is added. That was survivable while search had no `offset`, but paging
/// over a non-total order silently repeats and skips rows between requests.
///
/// Takes the classified pairs rather than a store, so the ordering can be tested
/// against a deliberately scrambled input — a test going through the store is at
/// the mercy of `read_dir` and passes with the tiebreak removed.
pub fn sort_by_match<T>(hits: &mut [(MatchClass, T)], key: impl Fn(&T) -> (Priority, CloveId)) {
    hits.sort_by(|a, b| {
        let (ap, ai) = key(&a.1);
        let (bp, bi) = key(&b.1);
        a.0.cmp(&b.0)
            .then_with(|| ap.cmp(&bp))
            .then_with(|| ai.cmp(&bi))
    });
}

/// Classify, filter, and order a set of items against a search `text`.
///
/// The whole shared search pipeline, so every surface's ranking is the same
/// function and not three that happen to agree.
///
/// `order` defaults to relevance (`(match class, priority, id)`); an explicit
/// [`SortField`] replaces that key entirely. `ranks` is consulted only for
/// [`SortField::Rank`] — pass an empty map when [`SearchOrder::needs_ranks`] is
/// false.
pub fn rank_search_hits(
    items: Vec<Item>,
    text: &str,
    order: SearchOrder,
    ranks: &HashMap<CloveId, usize>,
) -> Vec<Item> {
    let needle = text.to_lowercase();
    let mut hits: Vec<(MatchClass, Item)> = items
        .into_iter()
        .filter_map(|item| match_class(&item, &needle).map(|class| (class, item)))
        .collect();
    match order.explicit() {
        Some(explicit) => explicit.apply_by(&mut hits, ranks, |(_, item)| &item.frontmatter),
        None => {
            sort_by_match(&mut hits, |item| {
                (item.frontmatter.priority, item.frontmatter.id.clone())
            });
            // Reverse the *whole* relevance key (class, priority, id), which is
            // still a total order — the same thing `Order::descending` does.
            if order.descending {
                hits.reverse();
            }
        }
    }
    hits.into_iter().map(|(_, item)| item).collect()
}

/// A topological rank lookup that sorts unknown ids last.
pub fn rank_of(ranks: &HashMap<CloveId, usize>, id: &CloveId) -> usize {
    ranks.get(id).copied().unwrap_or(usize::MAX)
}

/// What a list is ordered by. The single sort vocabulary, shared by the CLI, the
/// MCP tools, the web API, `cloved`'s query RPC, and the SQL the index path runs
/// — so `--sort updated` on one surface and `?sort=updated` on another are the
/// same question with the same answer.
///
/// [`SortField::Rank`] is the default and reproduces clove's historical list
/// order, `(priority, topological rank, id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    /// `(priority, topological rank, id)` — the work-order default: the most
    /// urgent item whose dependencies come first.
    #[default]
    Rank,
    /// Priority 0 (highest) → 4.
    Priority,
    /// Creation timestamp, oldest first.
    Created,
    /// Last-modified timestamp, oldest first.
    Updated,
    /// Lexicographic by id — arbitrary, but stable and cheap.
    Id,
    /// Lifecycle order: `open` → `in_progress` → `closed` (see
    /// [`SortField::STATUS_ORDER`]).
    Status,
    /// Declaration order: `bug` → `feature` → `chore` → `docs` → `epic` (see
    /// [`SortField::TYPE_ORDER`]).
    Type,
}

impl SortField {
    /// `status` values in the order [`SortField::Status`] sorts them.
    ///
    /// Lifecycle order, *not* the alphabetical order a bare SQL `ORDER BY
    /// status` would give (`closed` < `in_progress` < `open`). It is the same
    /// order the board columns and `GET /api/v1/meta` already advertise, and the
    /// index path builds its `CASE` from this very array so the two cannot
    /// drift.
    pub const STATUS_ORDER: [ItemStatus; 3] =
        [ItemStatus::Open, ItemStatus::InProgress, ItemStatus::Closed];

    /// `type` values in the order [`SortField::Type`] sorts them — again the
    /// declared/displayed order rather than alphabetical.
    pub const TYPE_ORDER: [ItemType; 5] = [
        ItemType::Bug,
        ItemType::Feature,
        ItemType::Chore,
        ItemType::Docs,
        ItemType::Epic,
    ];

    /// Parse a sort-field word (case-insensitive).
    pub fn parse(raw: &str) -> Result<SortField, CloveError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rank" => Ok(SortField::Rank),
            "priority" => Ok(SortField::Priority),
            "created" => Ok(SortField::Created),
            "updated" => Ok(SortField::Updated),
            "id" => Ok(SortField::Id),
            "status" => Ok(SortField::Status),
            "type" => Ok(SortField::Type),
            other => Err(CloveError::InvalidField {
                field: "sort".to_owned(),
                reason: format!(
                    "expected rank|priority|created|updated|id|status|type, got `{other}`"
                ),
            }),
        }
    }

    /// The canonical wire word, as echoed in `_meta.sort`.
    pub fn as_str(self) -> &'static str {
        match self {
            SortField::Rank => "rank",
            SortField::Priority => "priority",
            SortField::Created => "created",
            SortField::Updated => "updated",
            SortField::Id => "id",
            SortField::Status => "status",
            SortField::Type => "type",
        }
    }
}

/// Position of `status` in [`SortField::STATUS_ORDER`].
pub fn status_rank(status: ItemStatus) -> usize {
    SortField::STATUS_ORDER
        .iter()
        .position(|s| *s == status)
        .unwrap_or(SortField::STATUS_ORDER.len())
}

/// Position of `item_type` in [`SortField::TYPE_ORDER`].
pub fn type_rank(item_type: ItemType) -> usize {
    SortField::TYPE_ORDER
        .iter()
        .position(|t| *t == item_type)
        .unwrap_or(SortField::TYPE_ORDER.len())
}

/// A complete list ordering: a [`SortField`] plus a direction.
///
/// `Order::default()` is `rank` ascending — exactly the order every list read
/// returned before sorting was configurable, which is why threading this through
/// changes nothing for a caller that does not ask for a sort.
///
/// **Every variant is a total order**, ending in an id tiebreak; `descending`
/// reverses the *whole* comparison, id included, so it is a total order too.
/// This is not a nicety: `offset`/`limit` paging over a partial order silently
/// repeats and skips rows between requests, because ties resolve to whatever the
/// input order happened to be (for the file paths, raw `read_dir` order). See
/// [`sort_by_match`], which documents the same hazard for search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Order {
    #[serde(default)]
    pub field: SortField,
    #[serde(default)]
    pub descending: bool,
}

impl Order {
    /// Build an order from raw `--sort`/`?sort=` and `--desc`/`?dir=` values.
    /// Absent field → [`SortField::Rank`]; absent direction → ascending.
    pub fn parse(field: Option<&str>, dir: Option<&str>) -> Result<Order, CloveError> {
        let descending = match dir.map(|d| d.trim().to_ascii_lowercase()) {
            None => false,
            Some(d) => match d.as_str() {
                "asc" | "ascending" => false,
                "desc" | "descending" => true,
                other => {
                    return Err(CloveError::InvalidField {
                        field: "dir".to_owned(),
                        reason: format!("expected asc|desc, got `{other}`"),
                    })
                }
            },
        };
        Ok(Order {
            field: field.map(SortField::parse).transpose()?.unwrap_or_default(),
            descending,
        })
    }

    /// The direction word, as echoed in `_meta.dir`.
    pub fn dir_str(self) -> &'static str {
        if self.descending {
            "desc"
        } else {
            "asc"
        }
    }

    /// Whether [`Order::apply`] consults the topological ranks. Only
    /// [`SortField::Rank`] does, so a caller with no graph in hand can skip
    /// building one for every other field.
    pub fn needs_ranks(self) -> bool {
        self.field == SortField::Rank
    }

    /// Sort frontmatter in place.
    pub fn apply(&self, items: &mut [ItemFrontmatter], ranks: &HashMap<CloveId, usize>) {
        self.apply_by(items, ranks, |fm| fm);
    }

    /// Sort rows that *carry* frontmatter (e.g. `blocked`'s `(frontmatter,
    /// blocked_by)` pairs, or full [`Item`]s) in place, by the same key.
    pub fn apply_by<T>(
        &self,
        rows: &mut [T],
        ranks: &HashMap<CloveId, usize>,
        fm: impl Fn(&T) -> &ItemFrontmatter,
    ) {
        rows.sort_by(|a, b| {
            let (a, b) = (fm(a), fm(b));
            let ord = match self.field {
                SortField::Rank => a
                    .priority
                    .cmp(&b.priority)
                    .then_with(|| rank_of(ranks, &a.id).cmp(&rank_of(ranks, &b.id))),
                SortField::Priority => a.priority.cmp(&b.priority),
                SortField::Created => a.created.cmp(&b.created),
                SortField::Updated => a.updated.cmp(&b.updated),
                SortField::Id => std::cmp::Ordering::Equal,
                SortField::Status => status_rank(a.status).cmp(&status_rank(b.status)),
                SortField::Type => type_rank(a.item_type).cmp(&type_rank(b.item_type)),
            }
            // The tiebreak that makes every variant a total order.
            .then_with(|| a.id.cmp(&b.id));
            if self.descending {
                ord.reverse()
            } else {
                ord
            }
        });
    }
}

/// How a *search* result set is ordered.
///
/// Search is the one list whose default key is not [`SortField::Rank`]: it ranks
/// by relevance, `(match class, priority, id)`. An explicit sort field replaces
/// that key **entirely** rather than extending it — "sort by updated" means the
/// newest item first, not the newest within each match class — so the two are
/// spelled as one type with `field: None` meaning relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOrder {
    /// `None` → relevance-first (the default).
    pub field: Option<SortField>,
    pub descending: bool,
}

impl SearchOrder {
    /// Build a search order from raw `--sort`/`sort` and `--desc`/`dir` values.
    ///
    /// An absent field keeps relevance — but a direction still applies to it, so
    /// `--desc` alone reverses the ranking rather than being silently dropped.
    pub fn parse(field: Option<&str>, dir: Option<&str>) -> Result<SearchOrder, CloveError> {
        let order = Order::parse(field, dir)?;
        Ok(SearchOrder {
            field: field.map(|_| order.field),
            descending: order.descending,
        })
    }

    /// The explicit order, if one was requested.
    pub fn explicit(self) -> Option<Order> {
        self.field.map(|field| Order {
            field,
            descending: self.descending,
        })
    }

    /// Whether ranking consults the topological ranks (only an explicit
    /// `--sort rank` does; relevance does not).
    pub fn needs_ranks(self) -> bool {
        self.explicit().is_some_and(Order::needs_ranks)
    }

    /// The sort word for `_meta.sort` — `"relevance"` when no field was given.
    pub fn reported_sort(self) -> &'static str {
        match self.field {
            Some(field) => field.as_str(),
            None => "relevance",
        }
    }

    /// The direction word for `_meta.dir`.
    pub fn dir_str(self) -> &'static str {
        if self.descending {
            "desc"
        } else {
            "asc"
        }
    }
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

/// Keys a *read* result drops whenever compaction is on, on top of null and
/// empty-list values: internal bookkeeping a reader never acts on.
///
/// `schema` is a per-file migration marker, not item data. It lives here rather
/// than in one surface so `clove ls --compact` and `clove_list` produce the same
/// key set — they did not, and the difference was a single silent key.
pub const READ_NOISE: &[&str] = &["schema"];

/// [`compact`], plus the [`READ_NOISE`] keys. The shaping every read surface
/// applies for `--compact` / `"compact": true`.
pub fn compact_read(mut map: Map<String, Value>) -> Map<String, Value> {
    for key in READ_NOISE {
        map.remove(*key);
    }
    compact(map)
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
        Order::default().apply(&mut items, &ranks);
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

    /// The default order is the historical one: `rank` ascending.
    #[test]
    fn order_defaults_to_rank_ascending() {
        let order = Order::default();
        assert_eq!(order.field, SortField::Rank);
        assert!(!order.descending);
        assert_eq!(order.dir_str(), "asc");
        assert_eq!(Order::parse(None, None).unwrap(), order);
        // The web's historical spellings still parse.
        assert_eq!(Order::parse(Some("rank"), Some("asc")).unwrap(), order);
        assert_eq!(
            Order::parse(Some("updated"), Some("desc")).unwrap(),
            Order {
                field: SortField::Updated,
                descending: true
            }
        );
        // Case-insensitive, whitespace-tolerant.
        assert_eq!(
            Order::parse(Some(" Created "), Some("DESC")).unwrap().field,
            SortField::Created
        );
        // Negative: an unknown field or direction is a validation error, not a
        // silent fallback to the default.
        assert!(Order::parse(Some("nope"), None).is_err());
        assert!(Order::parse(None, Some("sideways")).is_err());
    }

    /// Every `SortField` is a **total** order: an input permutation must not
    /// change the output. A missing id tiebreak leaves ties in input order, and
    /// paging over that silently repeats and skips rows.
    #[test]
    fn every_sort_field_is_a_total_order() {
        // Four items that tie on *every* non-id key in at least one pair —
        // including the timestamps, which `fm` would otherwise stamp with
        // distinct `Utc::now()` values and hide the missing tiebreak.
        let mk = |raw: &str, status, t, p| {
            let mut f = fm("t", status, t, p, &[]);
            f.id = CloveId::new(raw).unwrap();
            f.created = "2026-01-01T00:00:00Z".parse().unwrap();
            f.updated = "2026-01-02T00:00:00Z".parse().unwrap();
            f
        };
        let items = vec![
            mk("proj-0000000D", ItemStatus::Open, ItemType::Bug, 1),
            mk("proj-0000000A", ItemStatus::Open, ItemType::Bug, 1),
            mk("proj-0000000C", ItemStatus::Closed, ItemType::Epic, 1),
            mk("proj-0000000B", ItemStatus::Closed, ItemType::Epic, 1),
        ];
        // All four share a rank, so `Rank` also falls through to the tiebreak.
        let ranks: HashMap<CloveId, usize> = items.iter().map(|f| (f.id.clone(), 7usize)).collect();

        for field in [
            SortField::Rank,
            SortField::Priority,
            SortField::Created,
            SortField::Updated,
            SortField::Id,
            SortField::Status,
            SortField::Type,
        ] {
            for descending in [false, true] {
                let order = Order { field, descending };
                let mut forward = items.clone();
                order.apply(&mut forward, &ranks);
                let mut reversed: Vec<_> = items.iter().rev().cloned().collect();
                order.apply(&mut reversed, &ranks);
                let ids = |v: &[ItemFrontmatter]| -> Vec<String> {
                    v.iter().map(|f| f.id.to_string()).collect()
                };
                assert_eq!(
                    ids(&forward),
                    ids(&reversed),
                    "{field:?} desc={descending} is not a total order — \
                     a permuted input sorted differently"
                );
                // Reversing must actually reverse, id tiebreak included.
                let mut ascending = items.clone();
                Order {
                    field,
                    descending: false,
                }
                .apply(&mut ascending, &ranks);
                let mut expect = ids(&ascending);
                if descending {
                    expect.reverse();
                }
                assert_eq!(ids(&forward), expect, "{field:?} desc={descending}");
            }
        }
    }

    /// Only `rank` reads the topological ranks — callers rely on this to skip a
    /// whole-store graph build for every other field. Getting it wrong is silent:
    /// `rank` with an empty rank map degenerates to `(priority, id)`, i.e. the
    /// `priority` order, with no error anywhere.
    #[test]
    fn only_rank_consults_the_topological_ranks() {
        assert!(Order::default().needs_ranks());
        for field in [
            SortField::Priority,
            SortField::Created,
            SortField::Updated,
            SortField::Id,
            SortField::Status,
            SortField::Type,
        ] {
            for descending in [false, true] {
                assert!(
                    !Order { field, descending }.needs_ranks(),
                    "{field:?} must not force a graph build"
                );
            }
        }
        assert!(Order {
            field: SortField::Rank,
            descending: true
        }
        .needs_ranks());

        // And the degeneracy is real, so the flag is load-bearing: the same
        // items sorted by `rank` with and without ranks differ.
        let mut a = fm("a", ItemStatus::Open, ItemType::Bug, 1, &[]);
        a.id = CloveId::new("proj-0000000A").unwrap();
        let mut b = fm("b", ItemStatus::Open, ItemType::Bug, 1, &[]);
        b.id = CloveId::new("proj-0000000B").unwrap();
        let ranks = HashMap::from([(a.id.clone(), 9usize), (b.id.clone(), 1usize)]);

        let mut with = vec![a.clone(), b.clone()];
        Order::default().apply(&mut with, &ranks);
        let mut without = vec![a, b];
        Order::default().apply(&mut without, &HashMap::new());
        assert_ne!(
            with.iter().map(|f| f.id.to_string()).collect::<Vec<_>>(),
            without.iter().map(|f| f.id.to_string()).collect::<Vec<_>>(),
        );
    }

    /// The enum-valued fields sort in lifecycle/declaration order, not the
    /// alphabetical order a bare SQL `ORDER BY` would produce. The index path
    /// builds its `CASE` from these same arrays.
    #[test]
    fn status_and_type_sort_in_declared_order() {
        assert_eq!(status_rank(ItemStatus::Open), 0);
        assert_eq!(status_rank(ItemStatus::InProgress), 1);
        assert_eq!(status_rank(ItemStatus::Closed), 2);
        assert_eq!(type_rank(ItemType::Bug), 0);
        assert_eq!(type_rank(ItemType::Feature), 1);
        assert_eq!(type_rank(ItemType::Epic), 4);

        let mk = |raw: &str, status| {
            let mut f = fm("t", status, ItemType::Bug, 2, &[]);
            f.id = CloveId::new(raw).unwrap();
            f
        };
        let mut items = vec![
            mk("proj-0000000A", ItemStatus::Closed),
            mk("proj-0000000B", ItemStatus::Open),
            mk("proj-0000000C", ItemStatus::InProgress),
        ];
        Order {
            field: SortField::Status,
            descending: false,
        }
        .apply(&mut items, &HashMap::new());
        let order: Vec<&str> = items.iter().map(|f| f.status.as_str()).collect();
        assert_eq!(
            order,
            vec!["open", "in_progress", "closed"],
            "lifecycle order — alphabetical would put `closed` first"
        );
    }

    /// `SearchOrder` keeps relevance as the default and lets an explicit field
    /// replace the whole key.
    #[test]
    fn search_order_defaults_to_relevance() {
        let default = SearchOrder::parse(None, None).unwrap();
        assert_eq!(default.field, None);
        assert_eq!(default.reported_sort(), "relevance");
        assert_eq!(default.explicit(), None);
        assert!(!default.needs_ranks());

        // `--desc` alone reverses relevance rather than being dropped.
        let desc = SearchOrder::parse(None, Some("desc")).unwrap();
        assert_eq!(desc.field, None);
        assert!(desc.descending);
        assert_eq!(desc.dir_str(), "desc");

        let explicit = SearchOrder::parse(Some("updated"), None).unwrap();
        assert_eq!(explicit.reported_sort(), "updated");
        assert_eq!(
            explicit.explicit(),
            Some(Order {
                field: SortField::Updated,
                descending: false
            })
        );
        assert!(SearchOrder::parse(Some("rank"), None)
            .unwrap()
            .needs_ranks());
    }

    /// An explicit sort on a search replaces the relevance key rather than
    /// extending it: a body hit that is newer outranks a title hit that is older.
    #[test]
    fn explicit_search_sort_replaces_the_relevance_key() {
        let mut old_title = fm("widget", ItemStatus::Open, ItemType::Bug, 1, &[]);
        old_title.id = CloveId::new("proj-0000000A").unwrap();
        old_title.updated = "2020-01-01T00:00:00Z".parse().unwrap();
        let mut new_body = fm("other", ItemStatus::Open, ItemType::Bug, 1, &[]);
        new_body.id = CloveId::new("proj-0000000B").unwrap();
        new_body.updated = "2030-01-01T00:00:00Z".parse().unwrap();

        let items = vec![
            Item {
                frontmatter: old_title,
                body: String::new(),
            },
            Item {
                frontmatter: new_body,
                body: "widget".to_owned(),
            },
        ];

        // Default: relevance — the title hit wins despite being older.
        let ranked = rank_search_hits(
            items.clone(),
            "widget",
            SearchOrder::default(),
            &HashMap::new(),
        );
        assert_eq!(ranked[0].frontmatter.title, "widget");

        // Explicit `updated desc`: the newer body hit wins.
        let by_updated = rank_search_hits(
            items,
            "widget",
            SearchOrder::parse(Some("updated"), Some("desc")).unwrap(),
            &HashMap::new(),
        );
        assert_eq!(
            by_updated[0].frontmatter.title, "other",
            "an explicit sort replaces the match-class key, not just its tail"
        );
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

    /// The SQL fetch count is `offset + limit`, clamped into SQLite's `i64`
    /// range. Without the clamp a legal window near the top of `usize` reached
    /// SQLite as an out-of-range integer and came back as a `datatype mismatch`
    /// — reported as `IO_ERROR`/exit 5 for what the file path answers with an
    /// empty page and exit 0.
    #[test]
    fn sql_fetch_stays_inside_sqlites_integer_range() {
        assert_eq!(Page::new(0, Some(10), 0).sql_fetch(), Some(10));
        assert_eq!(Page::new(5, Some(10), 0).sql_fetch(), Some(15));
        // Unlimited fetches everything, with no row count at all.
        assert_eq!(Page::new(3, Some(0), 100).sql_fetch(), None);

        let max = i64::MAX as usize;
        assert_eq!(Page::new(max, Some(2), 0).sql_fetch(), Some(max));
        assert_eq!(Page::new(usize::MAX, Some(1), 0).sql_fetch(), Some(max));
        assert!(
            Page::new(max, Some(2), 0).sql_fetch().unwrap() <= i64::MAX as usize,
            "an over-range fetch count is a SQLite type error, not a big page"
        );
    }

    /// Search results are totally ordered, so paging over them is stable.
    ///
    /// Driven against a scrambled list rather than through a store: `search`
    /// reads the issues directory, so a store-based test sees whatever order
    /// `read_dir` returns — which on this filesystem is already sorted, making
    /// such a test pass with the tiebreak removed.
    /// A `SortField` has three independent spellings — `serde(rename_all)` on
    /// the daemon wire, `as_str()` in `_meta.sort` and the SQL `CASE`, and
    /// `parse()` for user input. Nothing but this test ties them together.
    ///
    /// The exhaustive match is the point: adding a variant fails to compile
    /// here, which is the prompt to check that all three agree. A variant named
    /// `LastTouched` would serialize as `last_touched` while `as_str()` returned
    /// whatever the author typed, and `_meta.sort` would disagree with the wire.
    #[test]
    fn the_three_spellings_of_a_sort_field_agree() {
        fn every_variant(f: SortField) -> &'static str {
            match f {
                SortField::Rank => "rank",
                SortField::Priority => "priority",
                SortField::Created => "created",
                SortField::Updated => "updated",
                SortField::Id => "id",
                SortField::Status => "status",
                SortField::Type => "type",
            }
        }
        const ALL: &[SortField] = &[
            SortField::Rank,
            SortField::Priority,
            SortField::Created,
            SortField::Updated,
            SortField::Id,
            SortField::Status,
            SortField::Type,
        ];

        for &field in ALL {
            let word = every_variant(field);
            assert_eq!(field.as_str(), word, "as_str disagrees for {field:?}");
            assert_eq!(
                SortField::parse(word).unwrap(),
                field,
                "parse disagrees for {field:?}"
            );
            assert_eq!(
                serde_json::to_value(field).unwrap(),
                Value::String(word.to_owned()),
                "the wire spelling disagrees for {field:?}"
            );
            assert_eq!(
                serde_json::from_value::<SortField>(Value::String(word.to_owned())).unwrap(),
                field,
                "the wire does not round-trip for {field:?}"
            );
        }
    }

    #[test]
    fn search_hits_are_totally_ordered() {
        let hit = |class: MatchClass, priority: u8, raw: &str| {
            (
                class,
                (Priority::new(priority).unwrap(), CloveId::new(raw).unwrap()),
            )
        };
        // Scrambled, and built so every key matters: three share a match class,
        // two of those share a priority.
        let mut hits = vec![
            hit(MatchClass::Body, 0, "proj-BBBBBBBB"),
            hit(MatchClass::Title, 3, "proj-CCCCCCCC"),
            hit(MatchClass::Title, 1, "proj-ZZZZZZZZ"),
            hit(MatchClass::Label, 4, "proj-AAAAAAAA"),
            hit(MatchClass::Title, 3, "proj-AAAAAAAB"),
        ];
        sort_by_match(&mut hits, |k| k.clone());
        let order: Vec<&str> = hits.iter().map(|h| h.1 .1.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "proj-ZZZZZZZZ", // title, p1
                "proj-AAAAAAAB", // title, p3, id sorts first
                "proj-CCCCCCCC", // title, p3
                "proj-AAAAAAAA", // label
                "proj-BBBBBBBB", // body — despite arriving first, at p0
            ],
            "class, then priority, then id — no input order survives"
        );

        // The result does not depend on the order the hits arrive in.
        let mut reversed: Vec<_> = hits.iter().rev().cloned().collect();
        sort_by_match(&mut reversed, |k| k.clone());
        let reordered: Vec<&str> = reversed.iter().map(|h| h.1 .1.as_str()).collect();
        assert_eq!(reordered, order, "a permuted input must sort identically");
    }

    /// A label-only hit is a hit. This is what the CLI's own predicate missed:
    /// it matched title and body, so an item found by the MCP tool was invisible
    /// to `clove search` on both of its paths.
    #[test]
    fn match_class_finds_titles_labels_and_bodies() {
        let mut f = fm("Widget rendering", ItemStatus::Open, ItemType::Bug, 1, &[]);
        f.labels = vec!["area:payments".to_owned()];
        let item = |body: &str| Item {
            frontmatter: f.clone(),
            body: body.to_owned(),
        };

        assert_eq!(match_class(&item(""), "widget"), Some(MatchClass::Title));
        assert_eq!(match_class(&item(""), "payments"), Some(MatchClass::Label));
        assert_eq!(
            match_class(&item("mentions gateway here"), "gateway"),
            Some(MatchClass::Body)
        );
        // Ranking is by strongest match, not by first found.
        assert_eq!(
            match_class(&item("widget widget widget"), "widget"),
            Some(MatchClass::Title),
            "a title hit outranks the same needle in the body"
        );
        // Case-insensitive on every field, and a miss is a miss.
        assert_eq!(
            match_class(&item(""), "WIDGET".to_lowercase().as_str()),
            Some(MatchClass::Title)
        );
        assert_eq!(match_class(&item("nothing"), "absent"), None);
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
