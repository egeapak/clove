//! Index-path queries (T-S07, DESIGN §6.5).
//!
//! [`query_items`] serves the `ready`, `ls`, and `query` read commands from the
//! index. The [`Filter`] here is the index-side query shape; the CLI maps its
//! flags onto it (the M0 command surface that does so is built separately).
//! Results sort by `(priority, topological_rank, id)` — the rank is stored for
//! ordering but never surfaced in the public JSON schema.

use clove_core::graph::EdgeKind;
use clove_core::{CloveId, ItemStatus, ItemType, Priority};
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
    /// Restrict to these statuses (ignored in [`QueryMode::Ready`], which fixes
    /// the active set).
    pub status: Option<Vec<ItemStatus>>,
    pub item_type: Option<ItemType>,
    pub priority: Option<Priority>,
    pub assignee: Option<String>,
    /// A single canonical label the item must carry.
    pub label: Option<String>,
    pub parent: Option<CloveId>,
    pub limit: Option<usize>,
}

/// Build the shared `WHERE … ORDER BY … [LIMIT …]` tail (and bound params) for a
/// filtered query, so the full and lean projections stay identical in selection
/// and ordering.
///
/// `topological_rank IS NULL` sorts unranked items (cyclic graph, or not yet
/// reindexed) last — matching clove-core's file-path ordering, which treats a
/// missing rank as `usize::MAX` (graph.rs `ready_items`).
fn where_order_sql(filter: &Filter) -> (String, Vec<Box<dyn ToSql>>) {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    match filter.mode {
        QueryMode::Ready => {
            where_clauses.push("status IN ('open', 'in_progress')".to_owned());
            where_clauses.push("has_dangling_deps = FALSE".to_owned());
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
    let limit_sql = match filter.limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let tail = format!(
        "{where_sql} ORDER BY priority ASC, topological_rank IS NULL ASC, \
         topological_rank ASC, id ASC{limit_sql}"
    );
    (tail, params)
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
/// (the CLI re-ranks title matches ahead of body matches).
pub fn search(
    conn: &Connection,
    text: &str,
    limit: Option<usize>,
) -> Result<Vec<ItemRow>, IndexError> {
    // Quote the user text as a single FTS5 string token, escaping embedded
    // quotes, so arbitrary input can't be interpreted as FTS query syntax.
    let match_query = format!("\"{}\"", text.replace('"', "\"\""));
    let limit_sql = match limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items WHERE id IN (\
           SELECT m.item_id FROM items_fts JOIN fts_map m ON m.fts_rowid = items_fts.rowid \
           WHERE items_fts MATCH ?1\
         ) ORDER BY priority ASC, topological_rank IS NULL ASC, topological_rank ASC, id ASC{limit_sql}"
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

    /// Full-text search (T-S05).
    pub fn search(&self, text: &str, limit: Option<usize>) -> Result<Vec<ItemRow>, IndexError> {
        search(self.conn(), text, limit)
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
}
