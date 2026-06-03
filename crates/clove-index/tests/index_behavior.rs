//! Integration tests for the public `clove-index` API and its consistency with
//! the file store (M1 contract).
//!
//! These complement, rather than duplicate, the in-module unit tests in
//! `src/{db,write,reindex,stale,query}.rs`. They exercise the crate strictly
//! through its public surface (`Index` methods + the free functions / row
//! types), and they pin down the key M1 guarantee: the index path and the
//! `clove-core` graph path agree on the set of "ready" items for the same
//! frontmatters.

use std::collections::HashSet;
use std::io::Write as _;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{
    parse_item_bytes, CloveId, GraphStore, Item, ItemFrontmatter, ItemStatus, ItemType, Priority,
};
use clove_index::{reindex, Filter, Index, IndexError, QueryMode};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A temp repo with `<root>/.clove/issues/` and the canonical db path.
struct Repo {
    _dir: tempfile::TempDir,
    issues: Utf8PathBuf,
    db: Utf8PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        let db = root.join(".clove/index.db");
        Repo {
            _dir: dir,
            issues,
            db,
        }
    }

    /// Write a raw `.md` file under `issues` for `<id>`.
    fn write_raw(&self, id: &str, contents: &str) {
        std::fs::write(self.issues.join(format!("{id}.md")), contents).unwrap();
    }

    /// Remove an item file.
    fn remove(&self, id: &str) {
        std::fs::remove_file(self.issues.join(format!("{id}.md"))).unwrap();
    }
}

/// Builder for one item `.md` file with the common knobs the tests need.
struct ItemSpec {
    id: String,
    title: String,
    body: String,
    status: String,
    item_type: String,
    priority: u8,
    assignee: Option<String>,
    labels: Vec<String>,
    deps: Vec<String>,
}

impl ItemSpec {
    fn new(id: &str) -> ItemSpec {
        ItemSpec {
            id: id.to_owned(),
            title: id.to_owned(),
            body: "body".to_owned(),
            status: "open".to_owned(),
            item_type: "feature".to_owned(),
            priority: 2,
            assignee: None,
            labels: Vec::new(),
            deps: Vec::new(),
        }
    }

    fn title(mut self, t: &str) -> Self {
        self.title = t.to_owned();
        self
    }
    fn body(mut self, b: &str) -> Self {
        self.body = b.to_owned();
        self
    }
    fn status(mut self, s: &str) -> Self {
        self.status = s.to_owned();
        self
    }
    fn item_type(mut self, t: &str) -> Self {
        self.item_type = t.to_owned();
        self
    }
    fn priority(mut self, p: u8) -> Self {
        self.priority = p;
        self
    }
    fn assignee(mut self, a: &str) -> Self {
        self.assignee = Some(a.to_owned());
        self
    }
    fn labels(mut self, ls: &[&str]) -> Self {
        self.labels = ls.iter().map(|s| s.to_string()).collect();
        self
    }
    fn deps(mut self, ds: &[&str]) -> Self {
        self.deps = ds.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Render the YAML-frontmatter `.md` text.
    fn render(&self) -> String {
        let mut s = format!(
            "---\nschema: 1\nid: {}\ntitle: {}\nstatus: {}\ntype: {}\npriority: {}\n\
             created: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n",
            self.id, self.title, self.status, self.item_type, self.priority
        );
        if self.status == "closed" {
            s.push_str("closed: 2026-06-02T11:00:00Z\n");
        }
        if let Some(a) = &self.assignee {
            s.push_str(&format!("assignee: {a}\n"));
        }
        if !self.labels.is_empty() {
            s.push_str("labels:\n");
            for l in &self.labels {
                s.push_str(&format!("  - {l}\n"));
            }
        }
        if !self.deps.is_empty() {
            s.push_str("deps:\n");
            for d in &self.deps {
                s.push_str(&format!("  - {d}\n"));
            }
        }
        s.push_str(&format!("---\n{}\n", self.body));
        s
    }

    fn write_to(&self, repo: &Repo) {
        repo.write_raw(&self.id, &self.render());
    }
}

/// Build an in-memory [`Item`] (for the write-through `upsert_item` path) by
/// rendering a spec and parsing it back through clove-core, exactly as the file
/// store would. Keeps the in-memory and on-disk items byte-identical.
fn item_from_spec(spec: &ItemSpec) -> Item {
    let id = CloveId::new(&spec.id).unwrap();
    let text = spec.render();
    let path = Utf8PathBuf::from(format!("{}.md", spec.id));
    parse_item_bytes(text.as_bytes(), &path, &id).unwrap()
}

/// Backdate every item file's mtime (and the dir's) past the 2s "recent file"
/// guard, so a `check_staleness` after a fresh `reindex` takes the clean fast
/// path instead of hashing every file.
fn backdate(issues: &Utf8Path) {
    let past = filetime::FileTime::from_unix_time(1_600_000_000, 0);
    for entry in std::fs::read_dir(issues).unwrap() {
        let p = entry.unwrap().path();
        filetime::set_file_mtime(&p, past).unwrap();
    }
    filetime::set_file_mtime(issues.as_std_path(), past).unwrap();
}

/// Parse every `<id>.md` under `issues` into frontmatters (the input shape that
/// `GraphStore::build` consumes) — used by the file<->index consistency test.
fn parse_frontmatters(issues: &Utf8Path) -> Vec<ItemFrontmatter> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(issues).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let Ok(id) = CloveId::new(stem) else {
            continue;
        };
        let bytes = std::fs::read(&path).unwrap();
        let utf8 = Utf8PathBuf::from_path_buf(path.clone()).unwrap();
        // Skip unparseable files, matching the index's malformed-skip rule.
        if let Ok(item) = parse_item_bytes(&bytes, &utf8, &id) {
            out.push(item.frontmatter);
        }
    }
    out
}

fn ready_id_set(rows: &[clove_index::ItemRow]) -> HashSet<String> {
    rows.iter().map(|r| r.id.clone()).collect()
}

// ---------------------------------------------------------------------------
// 1. open / open_or_create / persistence / corruption recovery
// ---------------------------------------------------------------------------

#[test]
fn open_then_reopen_persists_data() {
    let repo = Repo::new();
    let spec = ItemSpec::new("proj-AAAAAAAA").title("persisted");
    {
        let mut index = Index::open(&repo.db).unwrap();
        index.upsert_item(&item_from_spec(&spec)).unwrap();
        assert_eq!(index.item_count().unwrap(), 1);
    }
    // Reopen via open_or_create: schema already present, the row survives.
    let index = Index::open_or_create(&repo.db).unwrap();
    assert_eq!(index.item_count().unwrap(), 1);
    let rows = index.search("persisted", None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "proj-AAAAAAAA");
}

#[test]
fn corrupt_db_is_rebuilt_by_open_or_create() {
    let repo = Repo::new();
    // Garbage bytes: not a SQLite database.
    {
        let mut f = std::fs::File::create(&repo.db).unwrap();
        f.write_all(b"\x00garbage not a database\xff").unwrap();
    }
    // A plain open surfaces the corruption; open_or_create transparently rebuilds.
    let opened = Index::open(&repo.db);
    assert!(
        matches!(
            opened,
            Err(IndexError::CorruptIndex(_)) | Err(IndexError::SqliteError(_))
        ),
        "expected a corruption error, got {opened:?}"
    );
    let index = Index::open_or_create(&repo.db).unwrap();
    assert_eq!(index.item_count().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// 2. upsert + FTS + re-upsert
// ---------------------------------------------------------------------------

#[test]
fn search_finds_by_title_and_body_and_replaces_on_reupsert() {
    let repo = Repo::new();
    let mut index = Index::open(&repo.db).unwrap();

    let a = ItemSpec::new("proj-AAAAAAAA")
        .title("alpha widget")
        .body("the original quokka lives here");
    let b = ItemSpec::new("proj-BBBBBBBB")
        .title("beta gadget")
        .body("unrelated text");
    index.upsert_item(&item_from_spec(&a)).unwrap();
    index.upsert_item(&item_from_spec(&b)).unwrap();
    assert_eq!(index.item_count().unwrap(), 2);

    // Found by a title term...
    let by_title = index.search("widget", None).unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].id, "proj-AAAAAAAA");
    // ...and by a body term.
    let by_body = index.search("quokka", None).unwrap();
    assert_eq!(by_body.len(), 1);
    assert_eq!(by_body[0].id, "proj-AAAAAAAA");

    // Re-upsert A with a new body: the old term disappears, the new one matches,
    // and there is no duplicate row.
    let a2 = ItemSpec::new("proj-AAAAAAAA")
        .title("alpha widget")
        .body("now featuring a narwhal instead");
    index.upsert_item(&item_from_spec(&a2)).unwrap();
    assert_eq!(index.item_count().unwrap(), 2, "no duplicate row");
    assert!(
        index.search("quokka", None).unwrap().is_empty(),
        "old term gone"
    );
    let narwhal = index.search("narwhal", None).unwrap();
    assert_eq!(narwhal.len(), 1);
    assert_eq!(narwhal[0].id, "proj-AAAAAAAA");
}

// ---------------------------------------------------------------------------
// 3. reindex: counts, topo rank ordering, malformed-skip warning
// ---------------------------------------------------------------------------

#[test]
fn reindex_reports_all_items_indexed() {
    let repo = Repo::new();
    for id in ["proj-AAAAAAAA", "proj-BBBBBBBB", "proj-CCCCCCCC"] {
        ItemSpec::new(id).write_to(&repo);
    }
    let report = reindex(&repo.issues, &repo.db).unwrap();
    assert_eq!(report.items_indexed, 3);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let index = Index::open(&repo.db).unwrap();
    assert_eq!(index.item_count().unwrap(), 3);
}

#[test]
fn reindex_stores_topo_rank_so_dependent_sorts_before_dependency() {
    let repo = Repo::new();
    // A depends on B (edge A->B), so toposort places A before B. Same priority,
    // so the List order is purely by topological rank then id.
    ItemSpec::new("proj-AAAAAAAA")
        .deps(&["proj-BBBBBBBB"])
        .write_to(&repo);
    ItemSpec::new("proj-BBBBBBBB").write_to(&repo);
    reindex(&repo.issues, &repo.db).unwrap();

    let index = Index::open(&repo.db).unwrap();
    let rows = index.query_items(&Filter::default()).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["proj-AAAAAAAA", "proj-BBBBBBBB"], "{ids:?}");
    // Ranks are actually populated (not NULL) after a full reindex.
    assert!(rows.iter().all(|r| r.topological_rank.is_some()));
}

#[test]
fn reindex_skips_unparseable_file_and_warns() {
    let repo = Repo::new();
    ItemSpec::new("proj-AAAAAAAA").write_to(&repo);
    ItemSpec::new("proj-BBBBBBBB").write_to(&repo);
    // Valid id name, broken frontmatter -> skipped + surfaced in warnings.
    repo.write_raw("proj-CCCCCCCC", "this is not valid frontmatter");

    let report = reindex(&repo.issues, &repo.db).unwrap();
    assert_eq!(report.items_indexed, 2);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("proj-CCCCCCCC"),
        "warning should name the bad file: {:?}",
        report.warnings
    );

    let index = Index::open(&repo.db).unwrap();
    assert_eq!(index.item_count().unwrap(), 2);
}

// ---------------------------------------------------------------------------
// 4. query_items(List): filters + ordering + limit
// ---------------------------------------------------------------------------

#[test]
fn list_filters_by_each_field() {
    let repo = Repo::new();
    ItemSpec::new("proj-AAAAAAAA")
        .item_type("bug")
        .priority(1)
        .assignee("alice")
        .labels(&["area:core"])
        .status("open")
        .write_to(&repo);
    ItemSpec::new("proj-BBBBBBBB")
        .item_type("feature")
        .priority(2)
        .assignee("bob")
        .labels(&["area:ui"])
        .status("closed")
        .write_to(&repo);
    ItemSpec::new("proj-CCCCCCCC")
        .item_type("bug")
        .priority(1)
        .assignee("alice")
        .labels(&["area:ui"])
        .status("in_progress")
        .write_to(&repo);
    reindex(&repo.issues, &repo.db).unwrap();
    let index = Index::open(&repo.db).unwrap();

    let by_type = index
        .query_items(&Filter {
            item_type: Some(ItemType::Bug),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_type.len(), 2);

    let by_priority = index
        .query_items(&Filter {
            priority: Some(Priority(1)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_priority.len(), 2);

    let by_assignee = index
        .query_items(&Filter {
            assignee: Some("alice".to_owned()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_assignee.len(), 2);

    let by_label = index
        .query_items(&Filter {
            label: Some("area:ui".to_owned()),
            ..Default::default()
        })
        .unwrap();
    let label_ids = ready_id_set(&by_label);
    assert_eq!(label_ids.len(), 2);
    assert!(label_ids.contains("proj-BBBBBBBB") && label_ids.contains("proj-CCCCCCCC"));

    // status is a Vec: match several statuses at once.
    let by_status = index
        .query_items(&Filter {
            status: Some(vec![ItemStatus::Open, ItemStatus::InProgress]),
            ..Default::default()
        })
        .unwrap();
    let status_ids = ready_id_set(&by_status);
    assert_eq!(status_ids.len(), 2);
    assert!(status_ids.contains("proj-AAAAAAAA") && status_ids.contains("proj-CCCCCCCC"));
}

#[test]
fn list_orders_by_priority_then_rank_then_id_and_honors_limit() {
    let repo = Repo::new();
    // Z is highest priority (0) -> first. Among the p1 items, X depends on Y so
    // X (edge source) ranks before Y. W is p1 with no deps -> unranked? No: a
    // full reindex ranks every node; W has no edges so it gets some rank. To keep
    // the assertion deterministic we only check the documented (priority, rank,
    // id) ordering by giving the two ranked items a clear dep relationship and
    // checking Z is first.
    ItemSpec::new("proj-ZZZZZZZZ").priority(0).write_to(&repo);
    ItemSpec::new("proj-XXXXXXXX")
        .priority(1)
        .deps(&["proj-YYYYYYYY"])
        .write_to(&repo);
    ItemSpec::new("proj-YYYYYYYY").priority(1).write_to(&repo);
    reindex(&repo.issues, &repo.db).unwrap();
    let index = Index::open(&repo.db).unwrap();

    let rows = index.query_items(&Filter::default()).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["proj-ZZZZZZZZ", "proj-XXXXXXXX", "proj-YYYYYYYY"],
        "{ids:?}"
    );

    // limit honored.
    let limited = index
        .query_items(&Filter {
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].id, "proj-ZZZZZZZZ");
}

// ---------------------------------------------------------------------------
// 5. query_items(Ready)
// ---------------------------------------------------------------------------

#[test]
fn ready_excludes_closed_blocked_and_dangling() {
    let repo = Repo::new();
    // A closed; B deps A -> B ready (its only dep is closed).
    ItemSpec::new("proj-AAAAAAAA")
        .status("closed")
        .write_to(&repo);
    ItemSpec::new("proj-BBBBBBBB")
        .deps(&["proj-AAAAAAAA"])
        .write_to(&repo);
    // C open; D deps C -> D not ready (open hard dep).
    ItemSpec::new("proj-CCCCCCCC").write_to(&repo);
    ItemSpec::new("proj-DDDDDDDD")
        .deps(&["proj-CCCCCCCC"])
        .write_to(&repo);
    // E deps a missing id -> dangling -> not ready.
    ItemSpec::new("proj-EEEEEEEE")
        .deps(&["proj-MISSING1"])
        .write_to(&repo);
    reindex(&repo.issues, &repo.db).unwrap();
    let index = Index::open(&repo.db).unwrap();

    let ready = index
        .query_items(&Filter {
            mode: QueryMode::Ready,
            ..Default::default()
        })
        .unwrap();
    let ids = ready_id_set(&ready);
    assert!(
        ids.contains("proj-BBBBBBBB"),
        "B (dep closed) ready: {ids:?}"
    );
    assert!(ids.contains("proj-CCCCCCCC"), "C (no deps) ready: {ids:?}");
    assert!(
        !ids.contains("proj-AAAAAAAA"),
        "A closed not ready: {ids:?}"
    );
    assert!(
        !ids.contains("proj-DDDDDDDD"),
        "D blocked not ready: {ids:?}"
    );
    assert!(
        !ids.contains("proj-EEEEEEEE"),
        "E dangling not ready: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. staleness lifecycle
// ---------------------------------------------------------------------------

#[test]
fn staleness_clean_after_reindex_with_backdated_mtimes() {
    let repo = Repo::new();
    for id in ["proj-AAAAAAAA", "proj-BBBBBBBB", "proj-CCCCCCCC"] {
        ItemSpec::new(id).write_to(&repo);
    }
    reindex(&repo.issues, &repo.db).unwrap();
    backdate(&repo.issues);
    // Re-sync meta's dir_mtime to the backdated value so the fast path applies.
    reindex(&repo.issues, &repo.db).unwrap();

    let index = Index::open(&repo.db).unwrap();
    let report = index.check_staleness(&repo.issues).unwrap();
    assert!(report.is_clean(), "expected clean tree: {report:?}");
    assert_eq!(report.change_count(), 0);
}

#[test]
fn staleness_detects_new_deleted_modified_and_apply_resyncs() {
    let repo = Repo::new();
    for id in ["proj-AAAAAAAA", "proj-BBBBBBBB", "proj-CCCCCCCC"] {
        ItemSpec::new(id).body("initial body").write_to(&repo);
    }
    reindex(&repo.issues, &repo.db).unwrap();

    // New file, deleted file, and a content modification (the freshly written
    // file lands inside the recent-window guard, forcing the content-hash check).
    ItemSpec::new("proj-DDDDDDDD")
        .body("brand new searchterm")
        .write_to(&repo);
    repo.remove("proj-CCCCCCCC");
    ItemSpec::new("proj-AAAAAAAA")
        .body("mutated body")
        .write_to(&repo);

    let mut index = Index::open(&repo.db).unwrap();
    let report = index.check_staleness(&repo.issues).unwrap();
    let new_ids: Vec<&str> = report.new_ids.iter().map(|i| i.as_str()).collect();
    let del_ids: Vec<&str> = report.deleted_ids.iter().map(|i| i.as_str()).collect();
    let stale_ids: Vec<&str> = report.stale_ids.iter().map(|i| i.as_str()).collect();
    assert_eq!(new_ids, vec!["proj-DDDDDDDD"], "{report:?}");
    assert_eq!(del_ids, vec!["proj-CCCCCCCC"], "{report:?}");
    assert_eq!(stale_ids, vec!["proj-AAAAAAAA"], "{report:?}");
    assert_eq!(report.change_count(), 3);

    // Apply resyncs the index in one shot.
    index.apply_staleness(&report, &repo.issues).unwrap();
    assert_eq!(index.item_count().unwrap(), 3, "C removed, D added");

    // The new item is searchable; the deleted one is gone.
    assert_eq!(index.search("searchterm", None).unwrap().len(), 1);
    // A subsequent check no longer reports the resynced rows as stale/deleted.
    let after = index.check_staleness(&repo.issues).unwrap();
    assert!(
        after.stale_ids.is_empty() && after.deleted_ids.is_empty(),
        "still dirty after apply: {after:?}"
    );
}

// ---------------------------------------------------------------------------
// 7. file <-> index consistency (the key M1 guarantee)
// ---------------------------------------------------------------------------

#[test]
fn ready_set_matches_clove_core_graph() {
    let repo = Repo::new();
    // A non-trivial fixture mixing: closed deps, open deps, dangling deps,
    // a chain, a closed item, and an in_progress item, across priorities.
    ItemSpec::new("proj-AAAAAAAA")
        .status("closed")
        .write_to(&repo); // closed leaf
    ItemSpec::new("proj-BBBBBBBB")
        .priority(1)
        .deps(&["proj-AAAAAAAA"])
        .write_to(&repo); // ready: dep closed
    ItemSpec::new("proj-CCCCCCCC").priority(0).write_to(&repo); // ready: no deps
    ItemSpec::new("proj-DDDDDDDD")
        .deps(&["proj-CCCCCCCC"])
        .write_to(&repo); // blocked: dep open
    ItemSpec::new("proj-EEEEEEEE")
        .deps(&["proj-MISSING1"])
        .write_to(&repo); // dangling
    ItemSpec::new("proj-FFFFFFFF")
        .status("in_progress")
        .write_to(&repo); // active, ready
    ItemSpec::new("proj-GGGGGGGG")
        .status("closed")
        .deps(&["proj-CCCCCCCC"])
        .write_to(&repo); // closed -> not ready
    reindex(&repo.issues, &repo.db).unwrap();

    // Index path.
    let index = Index::open(&repo.db).unwrap();
    let index_ready = ready_id_set(
        &index
            .query_items(&Filter {
                mode: QueryMode::Ready,
                ..Default::default()
            })
            .unwrap(),
    );

    // File / graph path: build the graph from the same parsed frontmatters and
    // ask clove-core directly.
    let frontmatters = parse_frontmatters(&repo.issues);
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let graph_ready: HashSet<String> = graph
        .ready_items()
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect();

    assert_eq!(
        index_ready, graph_ready,
        "index ready set must equal clove-core graph ready set"
    );
    // Sanity: the fixture actually exercises readiness (non-empty, non-all).
    assert!(!graph_ready.is_empty() && graph_ready.len() < frontmatters.len());
}

// ---------------------------------------------------------------------------
// 8. search: limit, punctuation-safety, empty result
// ---------------------------------------------------------------------------

#[test]
fn search_limit_is_honored() {
    let repo = Repo::new();
    let mut index = Index::open(&repo.db).unwrap();
    for id in [
        "proj-AAAAAAAA",
        "proj-BBBBBBBB",
        "proj-CCCCCCCC",
        "proj-DDDDDDDD",
    ] {
        index
            .upsert_item(&item_from_spec(
                &ItemSpec::new(id).body("shared common token"),
            ))
            .unwrap();
    }
    assert_eq!(index.search("shared", None).unwrap().len(), 4);
    assert_eq!(index.search("shared", Some(2)).unwrap().len(), 2);
    assert_eq!(index.search("shared", Some(0)).unwrap().len(), 0);
}

#[test]
fn search_is_quoting_safe_and_empty_on_no_match() {
    let repo = Repo::new();
    let mut index = Index::open(&repo.db).unwrap();
    index
        .upsert_item(&item_from_spec(
            &ItemSpec::new("proj-AAAAAAAA").body("normal content here"),
        ))
        .unwrap();

    // Input full of FTS metacharacters / punctuation must not error or inject
    // query syntax — it's treated as a literal phrase that simply matches nothing.
    for tricky in [
        "\"quote injection\" OR 1",
        "foo* AND bar",
        "(unbalanced",
        "co-lon: semi; comma,",
    ] {
        let res = index.search(tricky, None).unwrap();
        assert!(
            res.is_empty(),
            "tricky input matched unexpectedly: {tricky:?}"
        );
    }

    // A clean non-matching term is also empty.
    assert!(index.search("absentword", None).unwrap().is_empty());
}
