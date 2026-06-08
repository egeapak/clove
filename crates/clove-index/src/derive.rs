//! Exact recomputation of the index's derived graph state (M4, P1).
//!
//! Three `items` columns are *derived* from the dependency graph rather than read
//! straight from a file: `topological_rank`, `has_dangling_deps`, and `excluded`
//! (hard-cycle / malformed-parent membership). A full [`crate::reindex`] computes
//! them from the file graph; the incremental [`crate::stale::apply_staleness`]
//! path historically left `topological_rank` unset and computed dangling only
//! locally — so ordering and exclusion drifted until the next reindex.
//!
//! [`recompute_derived`] closes that gap **without re-reading the files**: it
//! reconstructs the dependency graph from the index's own `items` + `edges`
//! tables (which the write path already keeps current), runs the same
//! `clove_core::GraphStore` the file path uses, and writes back the three derived
//! columns — touching only the rows whose values actually changed (so the
//! `idx_items_list` covering index is not churned for the whole table on every
//! batch). Because the toposort is canonical (a pure function of edges + ids,
//! see `GraphStore::topological_ranks`), the result is byte-identical to a full
//! reindex.

use std::collections::{HashMap, HashSet};

use clove_core::GraphStore;
use clove_types::{CloveId, ItemFrontmatter, ItemStatus, ItemType, Priority};
use rusqlite::Connection;

use crate::db::IndexError;
use crate::write::UNRANKED_TOPO;

/// A timestamp the graph never reads; reconstructed frontmatters only need the
/// graph-relevant fields (id/status/type/priority/parent/edges).
fn epoch() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid")
}

/// Parse the stored status string back into the enum (defaults to `open` on an
/// unexpected value — the DB is written from validated items, so this is just a
/// total function).
fn parse_status(s: &str) -> ItemStatus {
    match s {
        "in_progress" => ItemStatus::InProgress,
        "closed" => ItemStatus::Closed,
        _ => ItemStatus::Open,
    }
}

fn parse_type(s: &str) -> ItemType {
    match s {
        "bug" => ItemType::Bug,
        "chore" => ItemType::Chore,
        "docs" => ItemType::Docs,
        "epic" => ItemType::Epic,
        _ => ItemType::Feature,
    }
}

/// Reconstruct the graph-relevant frontmatters from the index tables.
///
/// Only the fields `GraphStore::build` consults are populated truthfully
/// (`id`, `status`, `item_type`, `priority`, `parent`, and the four edge lists);
/// the rest carry harmless defaults. Edges come from the `edges` table by kind;
/// the parent link comes from `items.parent_id` (ParentOf is not stored in
/// `edges`).
pub(crate) fn reconstruct_frontmatters(
    conn: &Connection,
) -> Result<Vec<ItemFrontmatter>, IndexError> {
    use clove_core::graph::EdgeKind;

    // Edge lists keyed by from_id.
    let mut deps: HashMap<String, Vec<CloveId>> = HashMap::new();
    let mut relates: HashMap<String, Vec<CloveId>> = HashMap::new();
    let mut duplicates: HashMap<String, Vec<CloveId>> = HashMap::new();
    let mut supersedes: HashMap<String, Vec<CloveId>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT from_id, to_id, kind FROM edges")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u8>(2)?,
            ))
        })?;
        for row in rows {
            let (from, to, kind) = row?;
            let Ok(to_id) = CloveId::new(&to) else {
                continue;
            };
            let bucket = if kind == EdgeKind::DependsOn as u8 {
                &mut deps
            } else if kind == EdgeKind::Relates as u8 {
                &mut relates
            } else if kind == EdgeKind::Duplicates as u8 {
                &mut duplicates
            } else if kind == EdgeKind::Supersedes as u8 {
                &mut supersedes
            } else {
                continue;
            };
            bucket.entry(from).or_default().push(to_id);
        }
    }

    let mut stmt =
        conn.prepare("SELECT id, title, status, item_type, priority, parent_id FROM items")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u8>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, title, status, item_type, priority, parent_id) = row?;
        let Ok(cid) = CloveId::new(&id) else {
            continue;
        };
        let parent = parent_id.and_then(|p| CloveId::new(&p).ok());
        out.push(ItemFrontmatter {
            schema: 1,
            id: cid,
            title,
            status: parse_status(&status),
            item_type: parse_type(&item_type),
            priority: Priority(priority),
            created: epoch(),
            updated: epoch(),
            closed: None,
            assignee: None,
            parent,
            labels: Vec::new(),
            deps: deps.remove(&id).unwrap_or_default(),
            relates: relates.remove(&id).unwrap_or_default(),
            duplicates: duplicates.remove(&id).unwrap_or_default(),
            supersedes: supersedes.remove(&id).unwrap_or_default(),
            source_system: None,
            external_ref: None,
        });
    }
    Ok(out)
}

/// Recompute and persist the derived columns (`topological_rank`,
/// `has_dangling_deps`, `excluded`) for every item, from the graph rebuilt out of
/// the index tables. Runs inside the caller's transaction. Only rows whose
/// derived values changed are written.
pub(crate) fn recompute_derived(conn: &Connection) -> Result<(), IndexError> {
    let frontmatters = reconstruct_frontmatters(conn)?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();
    let excluded: HashSet<CloveId> = graph.excluded_ids().into_iter().collect();

    // Current persisted derived values, to write only the deltas.
    let mut current: HashMap<String, (i64, bool, bool)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT id, topological_rank, has_dangling_deps, excluded FROM items")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        for row in rows {
            let (id, rank, dangling, excl) = row?;
            current.insert(id, (rank, dangling, excl));
        }
    }

    let mut update = conn.prepare(
        "UPDATE items SET topological_rank = ?1, has_dangling_deps = ?2, excluded = ?3 \
         WHERE id = ?4",
    )?;
    for fm in &frontmatters {
        let id = fm.id.as_str();
        let rank = ranks
            .get(&fm.id)
            .map(|r| *r as i64)
            .unwrap_or(UNRANKED_TOPO);
        let dangling = graph
            .meta(&fm.id)
            .map(|m| m.has_dangling_deps())
            .unwrap_or(false);
        let excl = excluded.contains(&fm.id);
        if current.get(id) != Some(&(rank, dangling, excl)) {
            update.execute(rusqlite::params![rank, dangling, excl, id])?;
        }
    }
    Ok(())
}

impl crate::db::Index {
    /// Recompute the derived graph columns from the index tables (test/diagnostic
    /// entry point; the incremental path calls the internal function in-txn).
    pub fn recompute_derived(&self) -> Result<(), IndexError> {
        recompute_derived(self.conn())
    }

    /// Reconstruct the graph-relevant frontmatters from the index tables, so a
    /// caller can build a `clove_core::GraphStore` **without re-reading the item
    /// files**. The daemon uses this to keep its hot dependency graph in sync from
    /// the (already-fresh) index rather than re-scanning `.clove/issues/` on every
    /// change (M4, P3). The result is graph-equivalent to scanning the files,
    /// because the index `items`/`edges` tables are an exact mirror of them.
    pub fn graph_frontmatters(&self) -> Result<Vec<ItemFrontmatter>, IndexError> {
        reconstruct_frontmatters(self.conn())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Index;
    use crate::reindex::reindex;
    use camino::{Utf8Path, Utf8PathBuf};

    fn write_item(issues: &Utf8Path, id: &str, status: &str, item_type: &str, deps: &[&str]) {
        let mut s = format!(
            "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: {status}\ntype: {item_type}\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n"
        );
        if status == "closed" {
            s.push_str("closed: 2026-06-02T11:00:00Z\n");
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

    /// Read the derived columns into a sorted vec for comparison.
    fn derived(index: &Index) -> Vec<(String, i64, bool, bool)> {
        let mut stmt = index
            .conn()
            .prepare(
                "SELECT id, topological_rank, has_dangling_deps, excluded \
                 FROM items ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
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
        Fx {
            _dir: dir,
            issues,
            db: root.join(".clove/index.db"),
        }
    }

    /// A fresh reindex of the same files (the "gold" derived state) into a
    /// throwaway db, for differential comparison.
    fn gold(issues: &Utf8Path) -> (tempfile::TempDir, Vec<(String, i64, bool, bool)>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Utf8PathBuf::from_path_buf(dir.path().join("gold.db")).unwrap();
        reindex(issues, &db).unwrap();
        let index = Index::open(&db).unwrap();
        let d = derived(&index);
        (dir, d)
    }

    /// The headline guarantee: after an incremental `apply_staleness`, the derived
    /// columns are byte-identical to a from-scratch reindex of the same files.
    #[test]
    fn incremental_apply_matches_full_reindex() {
        let fx = setup();
        // Chain: B→A, C→B; D depends on a missing X (dangling).
        write_item(&fx.issues, "proj-AAAAAAAA", "closed", "feature", &[]);
        write_item(
            &fx.issues,
            "proj-BBBBBBBB",
            "open",
            "feature",
            &["proj-AAAAAAAA"],
        );
        write_item(
            &fx.issues,
            "proj-CCCCCCCC",
            "open",
            "feature",
            &["proj-BBBBBBBB"],
        );
        write_item(
            &fx.issues,
            "proj-DDDDDDDD",
            "open",
            "feature",
            &["proj-XXXXXXXX"],
        );
        reindex(&fx.issues, &fx.db).unwrap();

        // Incremental edits: create X (resolves D's dangling) and add E→A, which
        // shifts ranks across the graph.
        write_item(&fx.issues, "proj-XXXXXXXX", "open", "feature", &[]);
        write_item(
            &fx.issues,
            "proj-EEEEEEEE",
            "open",
            "feature",
            &["proj-AAAAAAAA"],
        );
        let mut index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        index.apply_staleness(&report, &fx.issues).unwrap();

        let (_g, gold) = gold(&fx.issues);
        assert_eq!(
            derived(&index),
            gold,
            "incremental derived state != reindex"
        );

        // Specifically: D no longer dangling (X exists now).
        let d_row = derived(&index)
            .into_iter()
            .find(|(id, ..)| id == "proj-DDDDDDDD")
            .unwrap();
        assert!(
            !d_row.2,
            "D should no longer be dangling once X exists: {d_row:?}"
        );
    }

    /// Introducing a hard cycle incrementally must mark the members `excluded`
    /// and unset ranks exactly as a reindex would.
    #[test]
    fn incremental_cycle_matches_reindex() {
        let fx = setup();
        write_item(
            &fx.issues,
            "proj-AAAAAAAA",
            "open",
            "feature",
            &["proj-BBBBBBBB"],
        );
        write_item(&fx.issues, "proj-BBBBBBBB", "open", "feature", &[]);
        reindex(&fx.issues, &fx.db).unwrap();

        // Close the loop: B now depends on A → cycle A↔B.
        write_item(
            &fx.issues,
            "proj-BBBBBBBB",
            "open",
            "feature",
            &["proj-AAAAAAAA"],
        );
        let mut index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        index.apply_staleness(&report, &fx.issues).unwrap();

        let (_g, gold) = gold(&fx.issues);
        assert_eq!(
            derived(&index),
            gold,
            "cyclic incremental derived != reindex"
        );
        // Both members excluded.
        assert!(derived(&index).iter().all(|(_, _, _, excl)| *excl));
    }
}
