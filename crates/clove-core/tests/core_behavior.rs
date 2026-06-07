//! Integration tests for the public `clove_core` API.
//!
//! These exercise the crate strictly through its public surface (the re-exports
//! from `clove_core`): the file store CRUD, id generation, label
//! canonicalization, field validation, the dependency-graph engine, the parser,
//! config loading, comments, and `doctor`.
//!
//! Conventions:
//! - Every test gets a fresh temp repo via [`repo`], which creates the required
//!   `.clove/issues/` directory.
//! - Timestamps are fixed (via [`ts`]) so assertions are deterministic.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

use clove_core::comments::add_comment_at;
use clove_core::graph::EdgeKind;
use clove_core::{
    diagnose, doctor_fix, list_comments, load_config, normalize_label, parse_frontmatter_file,
    parse_item_bytes, parse_item_file, validate_item, CloveConfig, CloveError, CloveId, GraphStore,
    Item, ItemFrontmatter, ItemStatus, ItemStore, ItemType, NewItem, OutputFormat, Priority,
    Severity, ValidationError,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A fresh temp repo with `.clove/issues/` created, plus its `ItemStore`.
///
/// Returns the `TempDir` too so the directory lives for the test's duration.
fn repo() -> (tempfile::TempDir, ItemStore) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
    // A properly-initialized repo carries `.clove/.gitignore` (as `clove init`
    // writes it); without it the doctor `GITIGNORE_DRIFT` check would fire.
    std::fs::write(
        root.join(".clove").join(".gitignore"),
        format!("{}\n", clove_core::GITIGNORE_ENTRIES.join("\n")),
    )
    .unwrap();
    (tmp, ItemStore::new(root))
}

/// A simple `NewItem` with sensible defaults.
fn new_item(title: &str) -> NewItem {
    NewItem {
        title: title.to_owned(),
        item_type: ItemType::Feature,
        priority: Priority::DEFAULT,
        labels: Vec::new(),
        deps: Vec::new(),
        parent: None,
        assignee: None,
        body: "Body.\n".to_owned(),
    }
}

/// Create an item in `store`, returning the created `Item`.
fn mk(store: &ItemStore, title: &str) -> Item {
    store
        .create("proj", new_item(title), ts("2026-06-02T10:00:00Z"))
        .unwrap()
}

/// Parse an RFC3339 timestamp.
fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// Parse an id (panics on a bad literal — test-only).
fn id(s: &str) -> CloveId {
    CloveId::new(s).unwrap()
}

/// Build a frontmatter for graph tests: given id, status, hard deps.
fn fm(id_str: &str, status: ItemStatus, deps: &[&str]) -> ItemFrontmatter {
    ItemFrontmatter {
        schema: 1,
        id: id(id_str),
        title: format!("Item {id_str}"),
        status,
        item_type: ItemType::Feature,
        priority: Priority::DEFAULT,
        created: ts("2026-06-02T10:00:00Z"),
        updated: ts("2026-06-02T10:00:00Z"),
        closed: if status == ItemStatus::Closed {
            Some(ts("2026-06-02T10:00:00Z"))
        } else {
            None
        },
        assignee: None,
        parent: None,
        labels: Vec::new(),
        deps: deps.iter().map(|d| id(d)).collect(),
        relates: Vec::new(),
        duplicates: Vec::new(),
        supersedes: Vec::new(),
        source_system: None,
        external_ref: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Store CRUD
// ---------------------------------------------------------------------------

#[test]
fn create_get_roundtrip_matches_file_path() {
    let (_tmp, store) = repo();
    let created = mk(&store, "First");

    // The generated id maps to a file path whose stem is the id.
    let path = store.path_for(&created.frontmatter.id);
    assert!(path.exists(), "create wrote the file");
    assert_eq!(path.file_stem(), Some(created.frontmatter.id.as_str()));

    // get returns an identical item.
    let fetched = store.get(&created.frontmatter.id).unwrap();
    assert_eq!(fetched, created);

    // The written file parses back to the same item.
    let parsed = parse_item_file(&path).unwrap();
    assert_eq!(parsed, created);

    // New items start Open with created == updated.
    assert_eq!(created.frontmatter.status, ItemStatus::Open);
    assert_eq!(created.frontmatter.created, created.frontmatter.updated);
    assert_eq!(created.frontmatter.schema, 1);
}

#[test]
fn update_stamps_updated_and_persists_changes() {
    let (_tmp, store) = repo();
    let created = mk(&store, "Original");

    let mut edited = created.clone();
    edited.frontmatter.title = "Edited".to_owned();
    edited.frontmatter.status = ItemStatus::InProgress;

    let updated = store.update(&edited, ts("2026-06-02T15:30:00Z")).unwrap();
    assert_eq!(updated.frontmatter.updated, ts("2026-06-02T15:30:00Z"));
    assert_ne!(updated.frontmatter.updated, created.frontmatter.updated);
    // `created` is preserved across updates.
    assert_eq!(updated.frontmatter.created, created.frontmatter.created);

    // Changes are persisted to disk.
    let reread = store.get(&created.frontmatter.id).unwrap();
    assert_eq!(reread.frontmatter.title, "Edited");
    assert_eq!(reread.frontmatter.status, ItemStatus::InProgress);
    assert_eq!(reread.frontmatter.updated, ts("2026-06-02T15:30:00Z"));
}

#[test]
fn update_truncates_subsecond_precision() {
    let (_tmp, store) = repo();
    let created = mk(&store, "Sub-second");
    let updated = store
        .update(&created, ts("2026-06-02T15:30:00.123456789Z"))
        .unwrap();
    // On-disk precision is whole seconds.
    assert_eq!(updated.frontmatter.updated, ts("2026-06-02T15:30:00Z"));
}

#[test]
fn delete_removes_file_and_comment_dir() {
    let (_tmp, store) = repo();
    let item = mk(&store, "With comments");
    let cid = item.frontmatter.id.clone();

    add_comment_at(
        store.issues_dir(),
        &cid,
        "ege@example.com",
        "A comment.",
        ts("2026-06-02T10:00:00Z"),
    )
    .unwrap();
    let comment_dir = store.issues_dir().join(cid.as_str());
    assert!(comment_dir.is_dir(), "comment dir created");

    store.delete(&cid, false).unwrap();
    assert!(!store.exists(&cid), "item file removed");
    assert!(!store.path_for(&cid).exists());
    assert!(!comment_dir.exists(), "sibling comment dir removed");
}

#[test]
fn delete_refuses_with_dependents_then_force_succeeds() {
    let (_tmp, store) = repo();
    let dep = mk(&store, "Dependency");

    let mut dependent_spec = new_item("Dependent");
    dependent_spec.deps = vec![dep.frontmatter.id.clone()];
    store
        .create("proj", dependent_spec, ts("2026-06-02T10:00:01Z"))
        .unwrap();

    // Plain delete is refused.
    let err = store.delete(&dep.frontmatter.id, false).unwrap_err();
    match err {
        CloveError::HasDependents { dependents, .. } => {
            assert_eq!(dependents.len(), 1);
        }
        other => panic!("expected HasDependents, got {other:?}"),
    }
    assert!(
        store.exists(&dep.frontmatter.id),
        "still present after refusal"
    );

    // force=true deletes anyway.
    store.delete(&dep.frontmatter.id, true).unwrap();
    assert!(!store.exists(&dep.frontmatter.id));
}

#[test]
fn get_and_update_and_delete_missing_are_not_found() {
    let (_tmp, store) = repo();
    let missing = id("proj-00000000");

    assert!(matches!(
        store.get(&missing).unwrap_err(),
        CloveError::NotFound { .. }
    ));
    assert!(matches!(
        store.delete(&missing, false).unwrap_err(),
        CloveError::NotFound { .. }
    ));
    assert!(!store.exists(&missing));

    // update of a non-existent item is NotFound.
    let mut ghost = mk(&store, "ghost");
    ghost.frontmatter.id = missing.clone();
    assert!(matches!(
        store
            .update(&ghost, ts("2026-06-02T10:00:00Z"))
            .unwrap_err(),
        CloveError::NotFound { .. }
    ));
}

#[test]
fn scan_and_scan_frontmatter_agree_on_ids() {
    let (_tmp, store) = repo();
    let a = mk(&store, "A");
    let b = mk(&store, "B");

    let (items, errs) = store.scan().unwrap();
    assert!(errs.is_empty());
    let item_ids: HashSet<CloveId> = items.into_iter().map(|i| i.frontmatter.id).collect();

    let (fms, ferrs) = store.scan_frontmatter().unwrap();
    assert!(ferrs.is_empty());
    let fm_ids: HashSet<CloveId> = fms.into_iter().map(|f| f.id).collect();

    assert_eq!(item_ids, fm_ids);
    assert!(item_ids.contains(&a.frontmatter.id));
    assert!(item_ids.contains(&b.frontmatter.id));
}

// ---------------------------------------------------------------------------
// 2. ID generation
// ---------------------------------------------------------------------------

#[test]
fn created_ids_are_unique_and_valid() {
    let (_tmp, store) = repo();
    let mut ids = HashSet::new();
    for n in 0..25 {
        let item = mk(&store, &format!("Item {n}"));
        let s = item.frontmatter.id.as_str();
        // Each generated id is itself a valid CloveId.
        assert!(CloveId::new(s).is_ok());
        assert!(s.starts_with("proj-"));
        assert!(ids.insert(item.frontmatter.id), "ids must be unique");
    }
    assert_eq!(ids.len(), 25);
}

#[test]
fn path_for_filename_stem_matches_id() {
    let (_tmp, store) = repo();
    let item = mk(&store, "Path test");
    let path = store.path_for(&item.frontmatter.id);
    assert_eq!(
        path.file_name(),
        Some(format!("{}.md", item.frontmatter.id).as_str())
    );
    assert_eq!(path.file_stem(), Some(item.frontmatter.id.as_str()));
    // path_for lives directly under issues_dir.
    assert_eq!(path.parent(), Some(store.issues_dir()));
}

// ---------------------------------------------------------------------------
// 3. Labels
// ---------------------------------------------------------------------------

#[test]
fn normalize_label_canonicalizes() {
    // Case folding + trim + internal-whitespace collapse all map to one form.
    for raw in ["Area:iOS", "  AREA:IOS  ", "area:ios", "area:IOS"] {
        assert_eq!(normalize_label(raw).unwrap(), "area:ios");
    }
    assert_eq!(
        normalize_label("multi   word\ttag").unwrap(),
        "multi word tag"
    );
}

#[test]
fn normalize_label_rejects_empty() {
    for raw in ["", "   ", "\t\n"] {
        assert!(matches!(
            normalize_label(raw).unwrap_err(),
            CloveError::EmptyLabel { .. }
        ));
    }
}

#[test]
fn created_item_with_canonical_labels_persists_them() {
    let (_tmp, store) = repo();
    let label = normalize_label("Area:Core").unwrap();
    let mut spec = new_item("Labeled");
    spec.labels = vec![label.clone()];
    let created = store
        .create("proj", spec, ts("2026-06-02T10:00:00Z"))
        .unwrap();
    assert_eq!(created.frontmatter.labels, vec!["area:core".to_owned()]);

    // The label survives a round-trip through disk unchanged.
    let reread = store.get(&created.frontmatter.id).unwrap();
    assert_eq!(reread.frontmatter.labels, vec![label]);
}

// ---------------------------------------------------------------------------
// 4. validate_item
// ---------------------------------------------------------------------------

#[test]
fn validate_clean_item_is_empty() {
    let clean = fm("proj-AAAAAAAA", ItemStatus::Open, &[]);
    assert!(validate_item(&clean).is_empty());
}

#[test]
fn validate_priority_out_of_range() {
    let mut bad = fm("proj-AAAAAAAA", ItemStatus::Open, &[]);
    bad.priority = Priority(7);
    let errors = validate_item(&bad);
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::PriorityOutOfRange(7))));
}

#[test]
fn validate_closed_without_timestamp() {
    let mut bad = fm("proj-AAAAAAAA", ItemStatus::Closed, &[]);
    bad.closed = None;
    let errors = validate_item(&bad);
    assert!(errors.contains(&ValidationError::ClosedWithoutTimestamp));
}

#[test]
fn validate_closed_timestamp_on_open() {
    let mut bad = fm("proj-AAAAAAAA", ItemStatus::Open, &[]);
    bad.closed = Some(ts("2026-06-02T10:00:00Z"));
    let errors = validate_item(&bad);
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::ClosedTimestampOnNonClosed("open"))));
}

// ---------------------------------------------------------------------------
// 5. Graph
// ---------------------------------------------------------------------------

const A: &str = "proj-AAAAAAAA";
const B: &str = "proj-BBBBBBBB";
const C: &str = "proj-CCCCCCCC";

#[test]
fn ready_excludes_blocked_and_blocked_reports_deps() {
    // A depends on B; B is open → A is blocked, B is ready.
    let items = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[])];
    let (graph, dangling) = GraphStore::build(&items);
    assert!(dangling.is_empty());

    assert_eq!(graph.ready_items(), vec![id(B)]);

    let blocked = graph.blocked_items();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].id, id(A));
    assert_eq!(blocked[0].blocking_deps, vec![id(B)]);
    assert!(blocked[0].dangling_deps.is_empty());
}

#[test]
fn closing_a_dependency_makes_dependent_ready() {
    let open = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[])];
    let (graph, _) = GraphStore::build(&open);
    assert!(!graph.ready_items().contains(&id(A)));

    // Close B → A becomes ready, B drops out (closed items are not "ready").
    let closed = [
        fm(A, ItemStatus::Open, &[B]),
        fm(B, ItemStatus::Closed, &[]),
    ];
    let (graph, _) = GraphStore::build(&closed);
    assert_eq!(graph.ready_items(), vec![id(A)]);
    assert!(graph.blocked_items().is_empty());
}

#[test]
fn cycles_detected_for_two_node_and_self() {
    // Two-node cycle.
    let two = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[A])];
    let (g, _) = GraphStore::build(&two);
    assert!(g.has_any_cycle());
    assert_eq!(g.all_cycles(), vec![vec![id(A), id(B)]]);
    // Cycle members are excluded from both ready and blocked.
    assert!(g.ready_items().is_empty());
    assert!(g.blocked_items().is_empty());

    // Self cycle.
    let selfloop = [fm(A, ItemStatus::Open, &[A])];
    let (g, _) = GraphStore::build(&selfloop);
    assert!(g.has_any_cycle());
    assert_eq!(g.all_cycles(), vec![vec![id(A)]]);

    // Acyclic graph reports no cycle.
    let linear = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[])];
    let (g, _) = GraphStore::build(&linear);
    assert!(!g.has_any_cycle());
    assert!(g.all_cycles().is_empty());
}

#[test]
fn check_would_cycle_true_and_false() {
    // A→B→C linear.
    let items = [
        fm(A, ItemStatus::Open, &[B]),
        fm(B, ItemStatus::Open, &[C]),
        fm(C, ItemStatus::Open, &[]),
    ];
    let (graph, _) = GraphStore::build(&items);
    // Adding C→A (C depends on A) closes the loop → would cycle.
    assert!(graph.check_would_cycle(&id(C), &id(A)));
    // Adding A→C is fine (A already transitively reaches C; no back path).
    assert!(!graph.check_would_cycle(&id(A), &id(C)));
}

#[test]
fn dep_tree_depth_limit_and_cycle_ref() {
    // Depth limiting on a linear chain A→B→C.
    let chain = [
        fm(A, ItemStatus::Open, &[B]),
        fm(B, ItemStatus::Open, &[C]),
        fm(C, ItemStatus::Open, &[]),
    ];
    let (graph, _) = GraphStore::build(&chain);

    // max_depth 1 → root + one level of children (B), no C.
    let tree = graph.dep_tree(&id(A), 1).unwrap();
    assert_eq!(tree.id, id(A));
    assert_eq!(tree.children.len(), 1);
    assert_eq!(tree.children[0].id, id(B));
    assert!(tree.children[0].children.is_empty(), "C is beyond depth 1");

    // max_depth 2 → reaches C.
    let tree = graph.dep_tree(&id(A), 2).unwrap();
    assert_eq!(tree.children[0].children[0].id, id(C));

    // Unknown root → None.
    assert!(graph.dep_tree(&id("proj-ZZZZZZZZ"), 5).is_none());

    // Cyclic graph: the tree terminates and marks a cycle_ref node.
    let cyclic = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[A])];
    let (graph, _) = GraphStore::build(&cyclic);
    let tree = graph.dep_tree(&id(A), 100).unwrap();
    assert!(tree_has_cycle_ref(&tree), "a node must be marked cycle_ref");
}

fn tree_has_cycle_ref(node: &clove_core::graph::DepTreeNode) -> bool {
    node.cycle_ref || node.children.iter().any(tree_has_cycle_ref)
}

#[test]
fn topological_ranks_order_dependent_before_dependency() {
    // A depends on B. A's rank must come before B's in the toposort.
    let items = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[])];
    let (graph, _) = GraphStore::build(&items);
    let ranks = graph.topological_ranks();
    let ra = ranks[&id(A)];
    let rb = ranks[&id(B)];
    assert!(
        ra < rb,
        "dependent A (rank {ra}) should precede dependency B (rank {rb})"
    );

    // A cyclic graph yields no ranks (empty map).
    let cyclic = [fm(A, ItemStatus::Open, &[B]), fm(B, ItemStatus::Open, &[A])];
    let (graph, _) = GraphStore::build(&cyclic);
    assert!(graph.topological_ranks().is_empty());
}

#[test]
fn dangling_refs_surface_for_missing_targets() {
    let missing = "proj-MISSING0";
    let items = [fm(A, ItemStatus::Open, &[missing])];
    let (graph, dangling) = GraphStore::build(&items);

    // The dangling target is recorded both in the returned list and on the graph.
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].from, id(A));
    assert_eq!(dangling[0].to, id(missing));
    assert_eq!(dangling[0].kind, EdgeKind::DependsOn);
    assert!(graph.dangling_ids().contains(&id(missing)));

    // The referencing item is reported as blocked-by-dangling, and its meta knows.
    let meta = graph.meta(&id(A)).unwrap();
    assert!(meta.has_dangling_deps());
    let blocked = graph.blocked_items();
    let entry = blocked.iter().find(|b| b.id == id(A)).unwrap();
    assert_eq!(entry.dangling_deps, vec![id(missing)]);
    assert!(!graph.ready_items().contains(&id(A)));
}

#[test]
fn malformed_parent_flagged_in_meta() {
    let mut item = fm(A, ItemStatus::Open, &[]);
    item.parent = Some(id(A)); // self-parent
    let (graph, _) = GraphStore::build(&[item]);
    let meta = graph.meta(&id(A)).unwrap();
    assert!(meta.malformed_parent);
    // Excluded from ready.
    assert!(!graph.ready_items().contains(&id(A)));
}

// ---------------------------------------------------------------------------
// 6. Parser
// ---------------------------------------------------------------------------

#[test]
fn parse_item_file_roundtrips_written_item() {
    let (_tmp, store) = repo();
    let mut spec = new_item("Round trip");
    spec.assignee = Some("ege".to_owned());
    spec.body = "Line one.\nLine two.\n".to_owned();
    let created = store
        .create("proj", spec, ts("2026-06-02T10:00:00Z"))
        .unwrap();

    let parsed = parse_item_file(&store.path_for(&created.frontmatter.id)).unwrap();
    assert_eq!(parsed, created);
    assert_eq!(parsed.body, "Line one.\nLine two.\n");
}

#[test]
fn parse_frontmatter_file_ignores_body() {
    let (_tmp, store) = repo();
    let mut spec = new_item("FM only");
    spec.body = "A long body that should be ignored by the frontmatter parser.\n".to_owned();
    let created = store
        .create("proj", spec, ts("2026-06-02T10:00:00Z"))
        .unwrap();

    let frontmatter = parse_frontmatter_file(&store.path_for(&created.frontmatter.id)).unwrap();
    assert_eq!(frontmatter, created.frontmatter);
}

#[test]
fn parse_item_bytes_detects_id_mismatch() {
    let (_tmp, store) = repo();
    let created = mk(&store, "Mismatch");
    let bytes = std::fs::read(store.path_for(&created.frontmatter.id)).unwrap();
    let path = store.path_for(&created.frontmatter.id);

    // Correct expected id parses.
    let ok = parse_item_bytes(&bytes, &path, &created.frontmatter.id).unwrap();
    assert_eq!(ok, created);

    // A different expected id → IdMismatch.
    let other = id("proj-ZZZZZZZZ");
    let err = parse_item_bytes(&bytes, &path, &other).unwrap_err();
    assert!(matches!(err, CloveError::IdMismatch { .. }), "got {err:?}");
}

#[test]
fn parse_item_file_filename_mismatch_errors() {
    // Write an item whose embedded id does not match its file name stem.
    let (_tmp, store) = repo();
    let contents = "---\nschema: 1\nid: proj-AAAAAAAA\ntitle: x\nstatus: open\ntype: bug\n\
priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n";
    let path = store.issues_dir().join("proj-BBBBBBBB.md");
    std::fs::write(&path, contents).unwrap();

    let err = parse_item_file(&path).unwrap_err();
    assert!(matches!(err, CloveError::IdMismatch { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// 7. Config
// ---------------------------------------------------------------------------

#[test]
fn config_defaults_are_valid() {
    let config = CloveConfig::default();
    let p = Utf8Path::new("/repo/.clove/config.toml");
    assert!(config.validate(p).is_ok());
    assert_eq!(config.id_length, 8);
    assert_eq!(config.default_format, OutputFormat::Human);
    assert_eq!(config.default_type, ItemType::Feature);
    assert!(config.index.auto_refresh);
    assert!(!config.daemon.git_sync);
}

#[test]
fn config_validate_rejects_bad_fields() {
    let p = Utf8Path::new("/repo/.clove/config.toml");

    // Bad id_prefix.
    let bad_prefix = CloveConfig {
        id_prefix: "Has-Dash".to_owned(),
        ..Default::default()
    };
    assert!(bad_prefix.validate(p).is_err());

    // Out-of-range id_length.
    for bad in [0u8, 3, 13] {
        let cfg = CloveConfig {
            id_length: bad,
            ..Default::default()
        };
        assert!(
            cfg.validate(p).is_err(),
            "id_length {bad} should be rejected"
        );
    }

    // Wrong config_schema.
    let bad_schema = CloveConfig {
        config_schema: 99,
        ..Default::default()
    };
    assert!(bad_schema.validate(p).is_err());
}

#[test]
fn config_from_toml_str_parses_and_validates() {
    let p = Utf8Path::new("/repo/.clove/config.toml");
    let text = "config_schema = 1\nid_prefix = \"clove\"\nid_length = 6\n\
default_type = \"bug\"\ndefault_format = \"json\"\n";
    let config = CloveConfig::from_toml_str(text, p).unwrap();
    config.validate(p).unwrap();
    assert_eq!(config.id_prefix, "clove");
    assert_eq!(config.id_length, 6);
    assert_eq!(config.default_type, ItemType::Bug);
    assert_eq!(config.default_format, OutputFormat::Json);

    // Unknown field is rejected at parse time.
    assert!(CloveConfig::from_toml_str("bogus = 1\n", p).is_err());
}

#[test]
fn load_config_reads_written_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".clove")).unwrap();
    std::fs::write(
        CloveConfig::path_in(&root),
        "id_prefix = \"abcd\"\nid_length = 5\n",
    )
    .unwrap();

    let config = load_config(&root).unwrap();
    assert_eq!(config.id_prefix, "abcd");
    assert_eq!(config.id_length, 5);
}

#[test]
fn output_format_parse_roundtrip() {
    for f in [OutputFormat::Human, OutputFormat::Json, OutputFormat::Jsonl] {
        assert_eq!(OutputFormat::parse(f.as_str()), Some(f));
    }
    assert_eq!(OutputFormat::parse("  JSON "), Some(OutputFormat::Json));
    assert_eq!(OutputFormat::parse("nonsense"), None);
}

// ---------------------------------------------------------------------------
// 8. Comments
// ---------------------------------------------------------------------------

#[test]
fn comments_add_then_list_ordered_by_timestamp() {
    let (_tmp, store) = repo();
    let item = mk(&store, "Discussed");
    let cid = item.frontmatter.id.clone();

    // Add out of chronological order; listing must sort.
    add_comment_at(
        store.issues_dir(),
        &cid,
        "ege@example.com",
        "Second.",
        ts("2026-06-02T12:00:00Z"),
    )
    .unwrap();
    add_comment_at(
        store.issues_dir(),
        &cid,
        "alice@example.com",
        "First.",
        ts("2026-06-02T11:00:00Z"),
    )
    .unwrap();

    let comments = list_comments(store.issues_dir(), &cid).unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].body, "First.");
    assert_eq!(comments[1].body, "Second.");
    assert_eq!(comments[0].timestamp, ts("2026-06-02T11:00:00Z"));
    assert_eq!(comments[0].author, "alice-example-com");
}

#[test]
fn comments_distinct_even_at_same_timestamp() {
    let (_tmp, store) = repo();
    let item = mk(&store, "Same instant");
    let cid = item.frontmatter.id.clone();
    let when = ts("2026-06-02T10:00:00Z");

    let a = add_comment_at(store.issues_dir(), &cid, "ege@example.com", "A", when).unwrap();
    let b = add_comment_at(store.issues_dir(), &cid, "ege@example.com", "B", when).unwrap();
    assert_ne!(a, b, "distinct files despite identical timestamp");
    assert_eq!(list_comments(store.issues_dir(), &cid).unwrap().len(), 2);
}

#[test]
fn comments_empty_when_none_present() {
    let (_tmp, store) = repo();
    let item = mk(&store, "No comments");
    assert!(list_comments(store.issues_dir(), &item.frontmatter.id)
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// 9. Doctor
// ---------------------------------------------------------------------------

#[test]
fn doctor_clean_store_is_healthy() {
    let (_tmp, store) = repo();
    // Need a config so the config check passes cleanly.
    std::fs::write(
        CloveConfig::path_in(store.repo_root()),
        "id_prefix = \"proj\"\n",
    )
    .unwrap();
    mk(&store, "Healthy A");
    mk(&store, "Healthy B");

    let report = diagnose(&store);
    assert_eq!(report.errors(), 0, "issues: {:?}", report.issues);
    assert_eq!(report.warnings(), 0, "issues: {:?}", report.issues);
    assert_eq!(report.checked, 2);
}

#[test]
fn doctor_fixes_noncanonical_labels_and_orphan_comments() {
    let (_tmp, store) = repo();
    std::fs::write(
        CloveConfig::path_in(store.repo_root()),
        "id_prefix = \"proj\"\n",
    )
    .unwrap();

    // Write an item file directly with NON-canonical labels (uppercase) — the
    // store would normalize, so we bypass it by writing raw YAML.
    let bad_id = "proj-LABELBAD";
    let contents = "---\nschema: 1\nid: proj-LABELBAD\ntitle: Bad labels\nstatus: open\n\
type: feature\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
labels:\n- Area:Core\n---\nbody\n";
    std::fs::write(store.issues_dir().join(format!("{bad_id}.md")), contents).unwrap();

    // An orphan comment directory: a comments dir with no matching item file.
    let orphan_dir = store.issues_dir().join("proj-ORPHAN00").join("comments");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(
        orphan_dir.join("20260602T100000.000000000Z-ege-example-com-abcd.md"),
        "orphan comment",
    )
    .unwrap();

    // Diagnose: both are fixable warnings, no errors.
    let report = diagnose(&store);
    assert_eq!(report.errors(), 0, "issues: {:?}", report.issues);
    let warning_codes: HashSet<&str> = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .map(|i| i.code)
        .collect();
    assert!(
        warning_codes.contains("NONCANONICAL_LABELS"),
        "{warning_codes:?}"
    );
    assert!(
        warning_codes.contains("ORPHAN_COMMENTS"),
        "{warning_codes:?}"
    );
    assert!(
        report
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .all(|i| i.fixable),
        "warnings should be fixable"
    );

    // Fix resolves them.
    let fixed = doctor_fix(&store).unwrap();
    assert!(fixed >= 2, "expected at least 2 fixes, got {fixed}");

    // After fix: labels canonical on disk, orphan dir gone, no warnings remain.
    let reread = store.get(&id(bad_id)).unwrap();
    assert_eq!(reread.frontmatter.labels, vec!["area:core".to_owned()]);
    assert!(!store.issues_dir().join("proj-ORPHAN00").exists());

    let after = diagnose(&store);
    assert_eq!(after.warnings(), 0, "issues: {:?}", after.issues);
}

#[test]
fn doctor_reports_dangling_dep_and_cycle_as_unfixed_errors() {
    let (_tmp, store) = repo();
    std::fs::write(
        CloveConfig::path_in(store.repo_root()),
        "id_prefix = \"proj\"\n",
    )
    .unwrap();

    // Item with a dangling dependency (target has no file).
    let dangling_src = "proj-DANGSRC0";
    let dangling_contents = "---\nschema: 1\nid: proj-DANGSRC0\ntitle: Dangler\nstatus: open\n\
type: feature\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
deps:\n- proj-NOEXIST0\n---\nbody\n";
    std::fs::write(
        store.issues_dir().join(format!("{dangling_src}.md")),
        dangling_contents,
    )
    .unwrap();

    // A two-node hard-dependency cycle: X→Y and Y→X.
    let x = "---\nschema: 1\nid: proj-CYCLEXXX\ntitle: X\nstatus: open\ntype: feature\n\
priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
deps:\n- proj-CYCLEYYY\n---\nbody\n";
    let y = "---\nschema: 1\nid: proj-CYCLEYYY\ntitle: Y\nstatus: open\ntype: feature\n\
priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
deps:\n- proj-CYCLEXXX\n---\nbody\n";
    std::fs::write(store.issues_dir().join("proj-CYCLEXXX.md"), x).unwrap();
    std::fs::write(store.issues_dir().join("proj-CYCLEYYY.md"), y).unwrap();

    let report = diagnose(&store);
    let error_codes: HashSet<&str> = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect();
    assert!(error_codes.contains("DANGLING_REF"), "{error_codes:?}");
    assert!(error_codes.contains("CYCLE_DETECTED"), "{error_codes:?}");
    // These structural errors are NOT marked fixable.
    assert!(report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .all(|i| !i.fixable));

    // doctor_fix does not silence them (no safe repair applies).
    doctor_fix(&store).unwrap();
    let after = diagnose(&store);
    let after_codes: HashSet<&str> = after
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect();
    assert!(after_codes.contains("DANGLING_REF"));
    assert!(after_codes.contains("CYCLE_DETECTED"));
}
