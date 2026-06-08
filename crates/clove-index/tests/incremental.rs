//! M4 incremental-graph integration tests: the `excluded` cycle-exclusion path
//! through the real `ready` query, the topology-change guard's status semantics,
//! and the daemon's DB-sourced graph equivalence (`graph_frontmatters`).

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::GraphStore;
use clove_index::{reindex, Filter, Index, QueryMode};
use clove_types::{CloveId, ItemFrontmatter, ItemStatus, ItemType, Priority};
use tempfile::TempDir;

fn ts() -> chrono::DateTime<chrono::Utc> {
    "2026-06-02T10:00:00Z".parse().unwrap()
}

/// Build a frontmatter (id/status/deps + optional parent), for the expected graph.
fn fm(id: &str, status: ItemStatus, deps: &[&str], parent: Option<&str>) -> ItemFrontmatter {
    ItemFrontmatter {
        schema: 1,
        id: CloveId::new(id).unwrap(),
        title: id.to_owned(),
        status,
        item_type: ItemType::Feature,
        priority: Priority::DEFAULT,
        created: ts(),
        updated: ts(),
        closed: matches!(status, ItemStatus::Closed).then(ts),
        assignee: None,
        parent: parent.map(|p| CloveId::new(p).unwrap()),
        labels: Vec::new(),
        deps: deps.iter().map(|d| CloveId::new(d).unwrap()).collect(),
        relates: Vec::new(),
        duplicates: Vec::new(),
        supersedes: Vec::new(),
        source_system: None,
        external_ref: None,
    }
}

fn write(issues: &Utf8Path, fm: &ItemFrontmatter) {
    let status = fm.status.as_str();
    let mut s = format!(
        "---\nschema: 1\nid: {}\ntitle: {}\nstatus: {status}\ntype: feature\n\
         priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n",
        fm.id, fm.title
    );
    if status == "closed" {
        s.push_str("closed: 2026-06-02T11:00:00Z\n");
    }
    if let Some(p) = &fm.parent {
        s.push_str(&format!("parent: {p}\n"));
    }
    if !fm.deps.is_empty() {
        s.push_str("deps:\n");
        for d in &fm.deps {
            s.push_str(&format!("  - {d}\n"));
        }
    }
    s.push_str("---\nbody\n");
    std::fs::write(issues.join(format!("{}.md", fm.id)), s).unwrap();
}

struct Fx {
    _dir: TempDir,
    issues: Utf8PathBuf,
    db: Utf8PathBuf,
}

fn fixture(items: &[ItemFrontmatter]) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    let issues = root.join(".clove/issues");
    std::fs::create_dir_all(&issues).unwrap();
    for fm in items {
        write(&issues, fm);
    }
    let db = root.join(".clove/index.db");
    reindex(&issues, &db).unwrap();
    Fx {
        _dir: dir,
        issues,
        db,
    }
}

fn ready_ids(index: &Index) -> Vec<String> {
    query_ids(index, QueryMode::Ready)
}

fn query_ids(index: &Index, mode: QueryMode) -> Vec<String> {
    index
        .query_items(&Filter {
            mode,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// Reindex `issues` into a throwaway db and return it (the "gold" oracle).
fn gold(issues: &Utf8Path) -> (TempDir, Index) {
    let dir = tempfile::tempdir().unwrap();
    let db = Utf8PathBuf::from_path_buf(dir.path().join("gold.db")).unwrap();
    reindex(issues, &db).unwrap();
    (dir, Index::open(&db).unwrap())
}

/// The `excluded` column makes the SQL `ready` query exclude a hard-cycle member
/// even when its only dependency is *closed* (so the open-dep check alone would
/// wrongly admit it) — matching the in-memory `GraphStore::ready_items`.
#[test]
fn index_ready_excludes_cycle_with_closed_member() {
    let items = [
        fm("proj-RRRRRRRR", ItemStatus::Open, &[], None),
        fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"], None),
        fm(
            "proj-BBBBBBBB",
            ItemStatus::Closed,
            &["proj-AAAAAAAA"],
            None,
        ),
    ];
    let fx = fixture(&items);
    let index = Index::open(&fx.db).unwrap();

    // Only the independent item is ready; A is a cycle member (its dep B is
    // closed, but the cycle still excludes it).
    assert_eq!(ready_ids(&index), vec!["proj-RRRRRRRR".to_string()]);

    // Parity with the in-memory graph.
    let (graph, _) = GraphStore::build(&items);
    let graph_ready: Vec<String> = graph.ready_items().iter().map(|i| i.to_string()).collect();
    assert_eq!(ready_ids(&index), graph_ready);
}

/// A status-only edit goes through the topology-change guard's fast path (no
/// derived recompute), yet `ready` still updates — because readiness is computed
/// at query time from the live `status` + edges, not from a stored column.
#[test]
fn status_edit_updates_ready_without_recompute() {
    let items = [
        fm("proj-AAAAAAAA", ItemStatus::Closed, &[], None),
        fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-AAAAAAAA"], None),
    ];
    let fx = fixture(&items);
    let mut index = Index::open(&fx.db).unwrap();
    // B is ready: its only dep A is closed.
    assert_eq!(ready_ids(&index), vec!["proj-BBBBBBBB".to_string()]);

    // Reopen A (status-only edit) and apply incrementally.
    write(
        &fx.issues,
        &fm("proj-AAAAAAAA", ItemStatus::Open, &[], None),
    );
    let report = index.check_staleness(&fx.issues).unwrap();
    assert_eq!(report.stale_ids.len(), 1);
    index.apply_staleness(&report, &fx.issues).unwrap();

    // A is open now → A itself is ready (no deps) and B is blocked (dep A open).
    // The flip is visible with no derived recompute: readiness reads live status.
    assert_eq!(ready_ids(&index), vec!["proj-AAAAAAAA".to_string()]);
}

/// The daemon's DB-sourced graph (`graph_frontmatters` → `GraphStore`) is
/// equivalent to building the graph from the files: same ready/blocked order,
/// cycles, and ranks. This underpins the P3 file-scan → DB-build swap.
#[test]
fn graph_frontmatters_reproduces_file_graph() {
    let items = [
        // chain + diamond
        fm(
            "proj-AAAAAAAA",
            ItemStatus::Open,
            &["proj-BBBBBBBB", "proj-CCCCCCCC"],
            None,
        ),
        fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-DDDDDDDD"], None),
        fm(
            "proj-CCCCCCCC",
            ItemStatus::Closed,
            &["proj-DDDDDDDD"],
            None,
        ),
        fm("proj-DDDDDDDD", ItemStatus::Closed, &[], None),
        // dangling reference
        fm("proj-EEEEEEEE", ItemStatus::Open, &["proj-MISSING0"], None),
        // hard cycle
        fm("proj-FFFFFFFF", ItemStatus::Open, &["proj-GGGGGGGG"], None),
        fm("proj-GGGGGGGG", ItemStatus::Open, &["proj-FFFFFFFF"], None),
        // parent link
        fm(
            "proj-HHHHHHHH",
            ItemStatus::Open,
            &[],
            Some("proj-AAAAAAAA"),
        ),
    ];
    let fx = fixture(&items);
    let index = Index::open(&fx.db).unwrap();

    let (expected, _) = GraphStore::build(&items);
    let from_db = index.graph_frontmatters().unwrap();
    let (actual, _) = GraphStore::build(&from_db);

    assert_eq!(actual.ready_items(), expected.ready_items(), "ready");
    let bl = |g: &GraphStore| -> Vec<String> {
        g.blocked_items()
            .into_iter()
            .map(|b| b.id.to_string())
            .collect()
    };
    assert_eq!(bl(&actual), bl(&expected), "blocked");
    assert_eq!(actual.all_cycles(), expected.all_cycles(), "cycles");
    assert_eq!(
        actual.topological_ranks(),
        expected.topological_ranks(),
        "ranks"
    );
    assert_eq!(actual.excluded_ids(), expected.excluded_ids(), "excluded");
}

/// A sequence of incremental edits (add, re-dep, delete, status flip) must leave
/// the index serving exactly what a single from-scratch reindex of the final
/// files would — no derived-state drift accumulates across edits.
#[test]
fn multi_edit_sequence_matches_reindex() {
    let fx = fixture(&[
        fm("proj-AAAAAAAA", ItemStatus::Closed, &[], None),
        fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-AAAAAAAA"], None),
        fm("proj-CCCCCCCC", ItemStatus::Open, &["proj-BBBBBBBB"], None),
    ]);
    let mut index = Index::open(&fx.db).unwrap();

    let step = |index: &mut Index, f: &dyn Fn()| {
        f();
        let report = index.check_staleness(&fx.issues).unwrap();
        index.apply_staleness(&report, &fx.issues).unwrap();
    };

    // 1. Add D depending on C.
    step(&mut index, &|| {
        write(
            &fx.issues,
            &fm("proj-DDDDDDDD", ItemStatus::Open, &["proj-CCCCCCCC"], None),
        );
    });
    // 2. Re-point B's dependency to a (currently missing) item → dangling.
    step(&mut index, &|| {
        write(
            &fx.issues,
            &fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-ZZZZZZZZ"], None),
        );
    });
    // 3. Create Z, resolving B's dangling dep.
    step(&mut index, &|| {
        write(
            &fx.issues,
            &fm("proj-ZZZZZZZZ", ItemStatus::Closed, &[], None),
        );
    });
    // 4. Delete C (D now references a missing item).
    step(&mut index, &|| {
        std::fs::remove_file(fx.issues.join("proj-CCCCCCCC.md")).unwrap();
    });
    // 5. Status-only flip on A (fast-path, no recompute).
    step(&mut index, &|| {
        write(
            &fx.issues,
            &fm("proj-AAAAAAAA", ItemStatus::Open, &[], None),
        );
    });

    let (_g, oracle) = gold(&fx.issues);
    assert_eq!(
        query_ids(&index, QueryMode::List),
        query_ids(&oracle, QueryMode::List),
        "list order (priority, topo_rank, id) must match reindex"
    );
    assert_eq!(
        ready_ids(&index),
        ready_ids(&oracle),
        "ready set must match reindex"
    );
}
