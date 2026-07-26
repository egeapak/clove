//! Index-path queries (T-S07, DESIGN §6.5).
//!
//! [`query_items`] serves the `ready`, `ls`, and `query` read commands from the
//! index. The [`Filter`] here is the index-side query shape; the CLI maps its
//! flags onto it (the M0 command surface that does so is built separately).
//! Results sort by `(priority, topological_rank, id)` — the rank is stored for
//! ordering but never surfaced in the public JSON schema.

use clove_core::graph::EdgeKind;
use clove_core::view::{Order, SortField};
use clove_types::{CloveId, ItemStatus, ItemType, Priority};
use rusqlite::{Connection, ToSql};

use crate::db::{IndexError, ItemListRow, ItemRow, ITEM_COLUMNS, LIST_COLUMNS};

/// Which query to run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// All items matching the filter (the `ls`/`query` path).
    #[default]
    List,
    /// Only items eligible to start: active, no dangling hard deps, and every
    /// hard dependency closed (DESIGN §6.5).
    Ready,
}

/// Filter criteria for an index query. All `Some` fields are ANDed together.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub mode: QueryMode,
    /// Restrict to these statuses. In [`QueryMode::Ready`] the requested set is
    /// *intersected* with the fixed active set (open/in_progress), matching the
    /// file path, which applies the status filter to the ready list.
    pub status: Option<Vec<ItemStatus>>,
    pub item_type: Option<ItemType>,
    pub priority: Option<Priority>,
    pub assignee: Option<String>,
    /// A single canonical label the item must carry.
    pub label: Option<String>,
    pub parent: Option<CloveId>,
    /// The result ordering. `Order::default()` is `rank` ascending — the
    /// historical `(priority, topological_rank, id)`.
    pub order: Order,
    pub limit: Option<usize>,
}

/// Build the `WHERE` clause (and bound params) for a filter — shared by the row
/// queries and the `COUNT(*)` used to report the unpaginated total.
fn where_clause(filter: &Filter) -> (String, Vec<Box<dyn ToSql>>) {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    match filter.mode {
        QueryMode::Ready => {
            // The ready set is fixed to active statuses; an explicit --status
            // filter narrows *within* it (`ready --status in_progress` must
            // return the same rows the file path returns, not ignore the flag).
            let active: Vec<&ItemStatus> = match &filter.status {
                Some(statuses) => statuses
                    .iter()
                    .filter(|s| matches!(s, ItemStatus::Open | ItemStatus::InProgress))
                    .collect(),
                None => Vec::new(),
            };
            if filter.status.is_some() {
                if active.is_empty() {
                    // e.g. `ready --status closed`: nothing can match.
                    where_clauses.push("FALSE".to_owned());
                } else {
                    let placeholders = vec!["?"; active.len()].join(", ");
                    where_clauses.push(format!("status IN ({placeholders})"));
                    for s in active {
                        params.push(Box::new(s.as_str().to_owned()));
                    }
                }
            } else {
                where_clauses.push("status IN ('open', 'in_progress')".to_owned());
            }
            where_clauses.push("has_dangling_deps = FALSE".to_owned());
            // Exclude hard-cycle / malformed-parent members, matching the
            // in-memory `GraphStore::ready_items` exactly (M4 P1). `excluded` is
            // kept current by `recompute_derived` on every reindex/incremental
            // apply.
            where_clauses.push("excluded = FALSE".to_owned());
            where_clauses.push(format!(
                "NOT EXISTS (SELECT 1 FROM edges e JOIN items dep ON e.to_id = dep.id \
                 WHERE e.from_id = items.id AND e.kind = {} AND dep.status != 'closed')",
                EdgeKind::DependsOn as u8
            ));
        }
        QueryMode::List => {
            if let Some(statuses) = &filter.status {
                if !statuses.is_empty() {
                    let placeholders = vec!["?"; statuses.len()].join(", ");
                    where_clauses.push(format!("status IN ({placeholders})"));
                    for s in statuses {
                        params.push(Box::new(s.as_str().to_owned()));
                    }
                }
            }
        }
    }

    if let Some(t) = filter.item_type {
        where_clauses.push("item_type = ?".to_owned());
        params.push(Box::new(t.as_str().to_owned()));
    }
    if let Some(p) = filter.priority {
        where_clauses.push("priority = ?".to_owned());
        params.push(Box::new(p.get()));
    }
    if let Some(a) = &filter.assignee {
        where_clauses.push("assignee = ?".to_owned());
        params.push(Box::new(a.clone()));
    }
    if let Some(parent) = &filter.parent {
        where_clauses.push("parent_id = ?".to_owned());
        params.push(Box::new(parent.as_str().to_owned()));
    }
    if let Some(label) = &filter.label {
        where_clauses.push("id IN (SELECT item_id FROM labels WHERE label = ?)".to_owned());
        params.push(Box::new(label.clone()));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clauses.join(" AND "))
    };
    (where_sql, params)
}

/// The `ORDER BY` clause for an [`Order`] — the index-path twin of
/// `clove_core::view::Order::apply`, and the only place SQL ordering is spelled.
///
/// Built from an exhaustive `match` on [`SortField`], never by interpolating a
/// caller-supplied string: a new variant is a compile error here, and a hostile
/// `?sort=` value can never reach SQLite as syntax. Every clause ends in `id`,
/// so each is a **total** order — `LIMIT`/`OFFSET` over a partial one silently
/// repeats and skips rows, and this path pushes the limit into SQL.
///
/// Two clauses need more than a bare column:
///
/// - `status`/`type` sort in lifecycle/declaration order, which is *not* the
///   alphabetical order the stored words give (`closed` < `in_progress` <
///   `open`). The `CASE` is generated from `SortField::{STATUS_ORDER,
///   TYPE_ORDER}`, the same arrays the file path ranks by.
/// - `created`/`updated` are TEXT, and lexicographic order *is* chronological
///   order here: the column is written from a parsed `DateTime<Utc>` via
///   `to_rfc3339()` (`crate::write`), so the index normalizes to one `+00:00`
///   spelling no matter how the file was written. A hand-edited `Z` or `+02:00`
///   in the frontmatter therefore cannot desynchronize the index path from the
///   file path — it is re-spelled on the way in. Differing sub-second precision
///   is safe too, and not by accident: `'+' (0x2B) < '.' (0x2E)`, so a truncated
///   fraction sorts before a longer one at the same instant.
///
/// Unranked items carry the sentinel `topological_rank`
/// ([`crate::write::UNRANKED_TOPO`], a large value) rather than `NULL`, so they
/// sort last — matching clove-core's file-path `usize::MAX` treatment — and the
/// default `rank` clause can still ride the `idx_items_list` covering index
/// instead of sorting.
fn order_by_sql(order: &Order) -> String {
    let dir = if order.descending { "DESC" } else { "ASC" };
    let case = |column: &str, words: &[&str]| {
        let arms: String = words
            .iter()
            .enumerate()
            .map(|(i, w)| format!("WHEN '{w}' THEN {i} "))
            .collect();
        format!("CASE {column} {arms}ELSE {} END", words.len())
    };
    let keys: Vec<String> = match order.field {
        SortField::Rank => vec![
            "priority".to_owned(),
            "topological_rank".to_owned(),
            "id".to_owned(),
        ],
        SortField::Priority => vec!["priority".to_owned(), "id".to_owned()],
        SortField::Created => vec!["created_at".to_owned(), "id".to_owned()],
        SortField::Updated => vec!["updated_at".to_owned(), "id".to_owned()],
        SortField::Id => vec!["id".to_owned()],
        SortField::Status => {
            let words: Vec<&str> = SortField::STATUS_ORDER.iter().map(|s| s.as_str()).collect();
            vec![case("status", &words), "id".to_owned()]
        }
        SortField::Type => {
            let words: Vec<&str> = SortField::TYPE_ORDER.iter().map(|t| t.as_str()).collect();
            vec![case("item_type", &words), "id".to_owned()]
        }
    };
    let terms: Vec<String> = keys.into_iter().map(|k| format!("{k} {dir}")).collect();
    format!(" ORDER BY {}", terms.join(", "))
}

/// The `ORDER BY … [LIMIT …]` tail.
///
/// A `LIMIT` (from `Filter::limit`) is pushed into SQL, but be precise about
/// what that buys: only the default `rank` order rides a covering index
/// (`idx_items_list`) and streams. `id` walks the `WITHOUT ROWID` primary key.
/// Every other field — `priority`, `created`, `updated`, `status`, `type` —
/// plans as `SCAN items` + `USE TEMP B-TREE FOR ORDER BY`, so the whole table is
/// sorted before `LIMIT` takes its slice. Measured on 10k rows: `rank` 0.012 ms,
/// `updated DESC` 0.931 ms. Sub-millisecond, but linear in store size, and it is
/// the flagship query (`ls --sort updated --desc --limit 10`). If that becomes
/// hot, the fix is an index per sortable column, not a change here.
fn order_limit_sql(filter: &Filter) -> String {
    let limit_sql = match filter.limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    format!("{}{limit_sql}", order_by_sql(&filter.order))
}

/// The combined `WHERE … ORDER BY … [LIMIT …]` tail (and params) for a row query.
fn where_order_sql(filter: &Filter) -> (String, Vec<Box<dyn ToSql>>) {
    let (where_sql, params) = where_clause(filter);
    (format!("{where_sql}{}", order_limit_sql(filter)), params)
}

/// Count the items matching `filter` (ignoring its `limit`) — the unpaginated
/// total for `_meta.total`. Cheap: `COUNT(*)` steps the matching rows but
/// materializes no columns.
pub fn count_items(conn: &Connection, filter: &Filter) -> Result<usize, IndexError> {
    let (where_sql, params) = where_clause(filter);
    let sql = format!("SELECT COUNT(*) FROM items{where_sql}");
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let n: i64 = conn.query_row(&sql, param_refs.as_slice(), |r| r.get(0))?;
    Ok(n as usize)
}

/// Run a filtered query returning full item rows (T-S07).
pub fn query_items(conn: &Connection, filter: &Filter) -> Result<Vec<ItemRow>, IndexError> {
    let (tail, params) = where_order_sql(filter);
    let sql = format!("SELECT {ITEM_COLUMNS} FROM items{tail}");
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), ItemRow::from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Run a filtered query returning the lean `clove ls` projection — same
/// selection and order as [`query_items`], but only the columns `ls` renders.
/// This is the index fast path for large lists.
pub fn query_list(conn: &Connection, filter: &Filter) -> Result<Vec<ItemListRow>, IndexError> {
    let (tail, params) = where_order_sql(filter);
    let sql = format!("SELECT {LIST_COLUMNS} FROM items{tail}");
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), ItemListRow::from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Full-text search over the FTS5 index (T-S05, index path).
///
/// The match runs in a subquery that resolves matched FTS rowids back to item
/// ids via `fts_map` (a contentless FTS table exposes only rowids); the outer
/// query then reads full item rows. Relevance ordering is left to the caller
/// (the CLI re-ranks title matches ahead of body matches), so `order` decides
/// only which rows a `limit` keeps.
///
/// This clause is built by the *same* [`order_by_sql`] as the list query. It
/// used to be a second, hand-written literal that differed from the list one by
/// a dead `topological_rank IS NULL ASC` term — a divergence that would have
/// survived changing only the clause you find first.
pub fn search(
    conn: &Connection,
    text: &str,
    order: &Order,
    limit: Option<usize>,
) -> Result<Vec<ItemRow>, IndexError> {
    // Quote the user text as a single FTS5 string token, escaping embedded
    // quotes, so arbitrary input can't be interpreted as FTS query syntax.
    let match_query = format!("\"{}\"", text.replace('"', "\"\""));
    let limit_sql = match limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let order_sql = order_by_sql(order);
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items WHERE id IN (\
           SELECT m.item_id FROM items_fts JOIN fts_map m ON m.fts_rowid = items_fts.rowid \
           WHERE items_fts MATCH ?1\
         ){order_sql}{limit_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([match_query], ItemRow::from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

impl crate::db::Index {
    /// Run a filtered query against the index (T-S07).
    pub fn query_items(&self, filter: &Filter) -> Result<Vec<ItemRow>, IndexError> {
        query_items(self.conn(), filter)
    }

    /// Run a filtered query returning the lean list projection.
    pub fn query_list(&self, filter: &Filter) -> Result<Vec<ItemListRow>, IndexError> {
        query_list(self.conn(), filter)
    }

    /// Count items matching `filter` (ignoring `limit`) — the unpaginated total.
    pub fn count_items(&self, filter: &Filter) -> Result<usize, IndexError> {
        count_items(self.conn(), filter)
    }

    /// Full-text search (T-S05).
    pub fn search(
        &self,
        text: &str,
        order: &Order,
        limit: Option<usize>,
    ) -> Result<Vec<ItemRow>, IndexError> {
        search(self.conn(), text, order, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reindex::reindex;
    use camino::Utf8PathBuf;

    /// Write one item file with optional deps/status/priority.
    #[allow(clippy::too_many_arguments)]
    fn write_item(
        issues: &camino::Utf8Path,
        id: &str,
        status: &str,
        priority: u8,
        item_type: &str,
        deps: &[&str],
        labels: &[&str],
    ) {
        let mut s = format!(
            "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: {status}\ntype: {item_type}\n\
             priority: {priority}\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n"
        );
        if status == "closed" {
            s.push_str("closed: 2026-06-02T11:00:00Z\n");
        }
        if !labels.is_empty() {
            s.push_str("labels:\n");
            for l in labels {
                s.push_str(&format!("  - {l}\n"));
            }
        }
        if !deps.is_empty() {
            s.push_str("deps:\n");
            for d in deps {
                s.push_str(&format!("  - {d}\n"));
            }
        }
        s.push_str("---\nbody\n");
        std::fs::write(issues.join(format!("{id}.md")), s).unwrap();
    }

    struct Fx {
        _dir: tempfile::TempDir,
        issues: Utf8PathBuf,
        db: Utf8PathBuf,
    }

    fn setup() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        let db = root.join(".clove/index.db");
        Fx {
            _dir: dir,
            issues,
            db,
        }
    }

    #[test]
    fn ready_excludes_blocked_and_dangling() {
        let fx = setup();
        // base: closed dependency -> dependent is ready.
        write_item(
            &fx.issues,
            "proj-AAAAAAAA",
            "closed",
            2,
            "feature",
            &[],
            &[],
        );
        write_item(
            &fx.issues,
            "proj-BBBBBBBB",
            "open",
            2,
            "feature",
            &["proj-AAAAAAAA"],
            &[],
        );
        // open dependency -> dependent is blocked.
        write_item(&fx.issues, "proj-CCCCCCCC", "open", 2, "feature", &[], &[]);
        write_item(
            &fx.issues,
            "proj-DDDDDDDD",
            "open",
            2,
            "feature",
            &["proj-CCCCCCCC"],
            &[],
        );
        // dangling dependency -> not ready.
        write_item(
            &fx.issues,
            "proj-EEEEEEEE",
            "open",
            2,
            "feature",
            &["proj-ZZZZZZZZ"],
            &[],
        );
        reindex(&fx.issues, &fx.db).unwrap();

        let index = crate::db::Index::open(&fx.db).unwrap();
        let ready = index
            .query_items(&Filter {
                mode: QueryMode::Ready,
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = ready.iter().map(|r| r.id.as_str()).collect();
        // A (no deps, but it's closed -> not active), C (no deps, open) and B
        // (dep closed) are ready; D (dep open) and E (dangling) are not.
        assert!(ids.contains(&"proj-BBBBBBBB"), "{ids:?}");
        assert!(ids.contains(&"proj-CCCCCCCC"), "{ids:?}");
        assert!(!ids.contains(&"proj-DDDDDDDD"), "{ids:?}");
        assert!(!ids.contains(&"proj-EEEEEEEE"), "{ids:?}");
        assert!(!ids.contains(&"proj-AAAAAAAA"), "{ids:?}");
    }

    #[test]
    fn list_orders_by_priority_then_topo_rank() {
        let fx = setup();
        // x depends on y (both p1). The dependent x ranks before its dependency
        // y (toposort: edge source first), so x sorts before y. z is p0 -> first.
        write_item(
            &fx.issues,
            "proj-XXXXXXXX",
            "open",
            1,
            "feature",
            &["proj-YYYYYYYY"],
            &[],
        );
        write_item(&fx.issues, "proj-YYYYYYYY", "open", 1, "feature", &[], &[]);
        write_item(&fx.issues, "proj-ZZZZZZZZ", "open", 0, "bug", &[], &[]);
        reindex(&fx.issues, &fx.db).unwrap();

        let index = crate::db::Index::open(&fx.db).unwrap();
        let rows = index.query_items(&Filter::default()).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["proj-ZZZZZZZZ", "proj-XXXXXXXX", "proj-YYYYYYYY"],
            "{ids:?}"
        );
    }

    #[test]
    fn filters_by_type_priority_and_label() {
        let fx = setup();
        write_item(
            &fx.issues,
            "proj-AAAAAAAA",
            "open",
            1,
            "bug",
            &[],
            &["area:core"],
        );
        write_item(
            &fx.issues,
            "proj-BBBBBBBB",
            "open",
            2,
            "feature",
            &[],
            &["area:ui"],
        );
        write_item(
            &fx.issues,
            "proj-CCCCCCCC",
            "open",
            1,
            "bug",
            &[],
            &["area:ui"],
        );
        reindex(&fx.issues, &fx.db).unwrap();
        let index = crate::db::Index::open(&fx.db).unwrap();

        let bugs = index
            .query_items(&Filter {
                item_type: Some(ItemType::Bug),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(bugs.len(), 2);

        let p1 = index
            .query_items(&Filter {
                priority: Some(Priority(1)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(p1.len(), 2);

        let ui = index
            .query_items(&Filter {
                label: Some("area:ui".to_owned()),
                ..Default::default()
            })
            .unwrap();
        let ui_ids: Vec<&str> = ui.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ui_ids.len(), 2);
        assert!(ui_ids.contains(&"proj-BBBBBBBB") && ui_ids.contains(&"proj-CCCCCCCC"));

        // Labels round-trip into the row's parsed JSON array.
        let core = index
            .query_items(&Filter {
                label: Some("area:core".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(core[0].labels, vec!["area:core".to_owned()]);
    }

    /// Every generated clause ends in an `id` tiebreak, in the same direction as
    /// the rest of the key. Without it, `LIMIT`/`OFFSET` over ties returns rows
    /// in whatever order SQLite's scan produced.
    #[test]
    fn every_order_clause_ends_in_an_id_tiebreak() {
        for field in [
            SortField::Rank,
            SortField::Priority,
            SortField::Created,
            SortField::Updated,
            SortField::Id,
            SortField::Status,
            SortField::Type,
        ] {
            for (descending, dir) in [(false, "ASC"), (true, "DESC")] {
                let sql = order_by_sql(&Order { field, descending });
                assert!(
                    sql.trim_end().ends_with(&format!("id {dir}")),
                    "{field:?} desc={descending}: {sql}"
                );
                assert!(!sql.contains(&format!("{} ", opposite(dir))), "{sql}");
            }
        }
    }

    fn opposite(dir: &str) -> &'static str {
        if dir == "ASC" {
            "DESC"
        } else {
            "ASC"
        }
    }

    /// The `status`/`type` clauses map to the *declared* order, not the
    /// alphabetical order the stored words would give — and the mapping is
    /// generated from the shared `SortField::{STATUS_ORDER, TYPE_ORDER}`, so a
    /// change to those arrays moves the SQL with it.
    #[test]
    fn status_and_type_clauses_encode_the_shared_order() {
        let status = order_by_sql(&Order {
            field: SortField::Status,
            descending: false,
        });
        assert!(
            status.contains("WHEN 'open' THEN 0")
                && status.contains("WHEN 'in_progress' THEN 1")
                && status.contains("WHEN 'closed' THEN 2"),
            "{status}"
        );
        let item_type = order_by_sql(&Order {
            field: SortField::Type,
            descending: false,
        });
        assert!(
            item_type.contains("WHEN 'bug' THEN 0") && item_type.contains("WHEN 'epic' THEN 4"),
            "{item_type}"
        );
    }

    /// `search`'s `ORDER BY` is the *second* order clause in this module, and it
    /// used to be a separate literal. It is only observable through a `limit`
    /// (the CLI asks for every candidate and re-ranks), so that is how it is
    /// tested: with `LIMIT 1`, the kept row is whichever the order puts first.
    #[test]
    fn search_honours_the_requested_order() {
        let fx = setup();
        // Both match the needle; A is p0/oldest-id, B is p3.
        for (id, priority) in [("proj-AAAAAAAA", 0u8), ("proj-BBBBBBBB", 3u8)] {
            std::fs::write(
                fx.issues.join(format!("{id}.md")),
                format!(
                    "---\nschema: 1\nid: {id}\ntitle: needle {id}\nstatus: open\ntype: bug\n\
                     priority: {priority}\ncreated: 2026-06-02T10:00:00Z\n\
                     updated: 2026-06-02T10:00:00Z\n---\nbody\n"
                ),
            )
            .unwrap();
        }
        reindex(&fx.issues, &fx.db).unwrap();
        let index = crate::db::Index::open(&fx.db).unwrap();

        let top = |field, descending| -> String {
            index
                .search("needle", &Order { field, descending }, Some(1))
                .unwrap()[0]
                .id
                .clone()
        };
        assert_eq!(top(SortField::Priority, false), "proj-AAAAAAAA");
        assert_eq!(
            top(SortField::Priority, true),
            "proj-BBBBBBBB",
            "the search clause must honour the direction, not just the field"
        );
        assert_eq!(top(SortField::Id, true), "proj-BBBBBBBB");
    }

    /// Each `SortField` really discriminates on the index path: the fixture is
    /// built so a wrong column produces a different id sequence.
    #[test]
    fn index_orders_by_each_sort_field() {
        let fx = setup();
        // A: p0, closed, epic, oldest. B: p2, open, bug, newest.
        // C: p1, in_progress, docs, middle — and depends on A, so its topo rank
        // differs from its id order.
        let write =
            |id: &str, status: &str, priority: u8, item_type: &str, day: u8, deps: &[&str]| {
                let mut s = format!(
                    "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: {status}\ntype: {item_type}\n\
                 priority: {priority}\ncreated: 2026-06-0{day}T10:00:00Z\n\
                 updated: 2026-06-0{day}T10:00:00Z\n"
                );
                if status == "closed" {
                    s.push_str("closed: 2026-06-09T11:00:00Z\n");
                }
                if !deps.is_empty() {
                    s.push_str("deps:\n");
                    for d in deps {
                        s.push_str(&format!("  - {d}\n"));
                    }
                }
                s.push_str("---\nbody\n");
                std::fs::write(fx.issues.join(format!("{id}.md")), s).unwrap();
            };
        write("proj-AAAAAAAA", "closed", 0, "epic", 1, &[]);
        write("proj-BBBBBBBB", "open", 2, "bug", 3, &[]);
        write(
            "proj-CCCCCCCC",
            "in_progress",
            1,
            "docs",
            2,
            &["proj-AAAAAAAA"],
        );
        reindex(&fx.issues, &fx.db).unwrap();
        let index = crate::db::Index::open(&fx.db).unwrap();

        let ids = |field| -> Vec<String> {
            index
                .query_items(&Filter {
                    order: Order {
                        field,
                        descending: false,
                    },
                    ..Default::default()
                })
                .unwrap()
                .iter()
                .map(|r| r.id.clone())
                .collect()
        };
        let (a, b, c) = ("proj-AAAAAAAA", "proj-BBBBBBBB", "proj-CCCCCCCC");
        assert_eq!(ids(SortField::Id), vec![a, b, c]);
        assert_eq!(ids(SortField::Priority), vec![a, c, b]);
        assert_eq!(ids(SortField::Created), vec![a, c, b]);
        assert_eq!(ids(SortField::Updated), vec![a, c, b]);
        assert_eq!(ids(SortField::Status), vec![b, c, a], "open → closed");
        assert_eq!(ids(SortField::Type), vec![b, c, a], "bug → docs → epic");
        // Rank: priority first, so a (p0) leads, then c (p1), then b (p2).
        assert_eq!(ids(SortField::Rank), vec![a, c, b]);

        // Descending reverses the whole key, id tiebreak included.
        let desc: Vec<String> = index
            .query_items(&Filter {
                order: Order {
                    field: SortField::Status,
                    descending: true,
                },
                ..Default::default()
            })
            .unwrap()
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(desc, vec![a, c, b]);
    }
}
