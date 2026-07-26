//! The invariant the whole crate rests on: **every tier answers the same
//! question.**
//!
//! `clove-engine` exists so a surface can stop caring which of daemon / index /
//! files produced a row. That is only safe if the three agree, and the risky
//! half is the new one — a tier answering the *query* in SQL and the engine then
//! reading only the page's files back (`Projection::Full`). This file pins that
//! hydrated index answer against the file scan, row for row, across filters,
//! orders, and windows.
//!
//! The daemon tier is not exercised here (it needs a spawned `cloved`; that is
//! `crates/clove/tests/daemon_routing.rs` and `filter_parity.rs`), but it goes
//! through the same `finish_lean` hydration, so a break there breaks here too.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::view::{Filters, Order, Page, SortField};
use clove_core::{ItemStore, NewItem};
use clove_engine::{Engine, Projection, Rows, Source, Tiers};
use clove_types::{CloveId, ItemStatus, ItemType, Priority};

/// A store with a deliberately awkward shape: a blocked chain, a dangling
/// reference, a closed dependency, mixed priorities/types/labels/assignees, and
/// a `q`-matchable title (the one filter SQL cannot express, so it forces the
/// residue path).
struct Fixture {
    _dir: tempfile::TempDir,
    root: Utf8PathBuf,
    clove_dir: Utf8PathBuf,
    ids: Vec<CloveId>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
    let clove_dir = root.join(".clove");
    std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
    std::fs::write(
        clove_dir.join("config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();

    let store = ItemStore::new(root.clone());
    let mut ids = Vec::new();
    // (title, type, priority, labels, assignee)
    type Spec = (
        &'static str,
        ItemType,
        u8,
        &'static [&'static str],
        Option<&'static str>,
    );
    let specs: [Spec; 6] = [
        (
            "alpha widget",
            ItemType::Bug,
            0,
            &["area:core"],
            Some("ana"),
        ),
        ("beta gizmo", ItemType::Feature, 3, &["area:ios"], None),
        (
            "gamma widget",
            ItemType::Chore,
            1,
            &["area:core", "area:ios"],
            Some("bo"),
        ),
        ("delta thing", ItemType::Docs, 2, &[], Some("ana")),
        ("epsilon widget", ItemType::Epic, 4, &["area:core"], None),
        ("zeta gizmo", ItemType::Bug, 2, &[], None),
    ];
    for (title, item_type, priority, labels, assignee) in specs {
        let item = store
            .create(
                "proj",
                NewItem {
                    title: title.to_owned(),
                    item_type,
                    priority: Priority(priority),
                    labels: labels.iter().map(|s| (*s).to_owned()).collect(),
                    deps: Vec::new(),
                    parent: None,
                    assignee: assignee.map(str::to_owned),
                    body: format!("body of {title}"),
                },
                Utc::now(),
            )
            .unwrap();
        ids.push(item.frontmatter.id);
    }

    // a depends on b (b open -> a blocked); c depends on d, which we close
    // (so c stays ready); e names a missing id (dangling -> blocked).
    edit(&store, &ids[0], |fm| fm.deps = vec![ids[1].clone()]);
    edit(&store, &ids[2], |fm| fm.deps = vec![ids[3].clone()]);
    clove_core::ops::transition(&store, &ids[3], ItemStatus::Closed, Utc::now()).unwrap();
    edit(&store, &ids[4], |fm| {
        fm.deps = vec![CloveId::new("proj-ZZZZZZZZ").unwrap()]
    });

    clove_index::reindex(&clove_dir.join("issues"), &clove_dir.join("index.db")).unwrap();
    Fixture {
        _dir: dir,
        root,
        clove_dir,
        ids,
    }
}

fn edit(store: &ItemStore, id: &CloveId, f: impl FnOnce(&mut clove_types::ItemFrontmatter)) {
    let mut item = store.get(id).unwrap();
    f(&mut item.frontmatter);
    store.update(&item, Utc::now()).unwrap();
}

impl Fixture {
    fn engine(&self, tiers: Tiers) -> Engine {
        Engine::new(
            ItemStore::new(self.root.clone()),
            self.clove_dir.clone(),
            tiers,
        )
    }

    fn indexed(&self) -> Engine {
        // No daemon in this process, so `Tiers::default()` resolves to the index.
        self.engine(Tiers::default())
    }

    fn files(&self) -> Engine {
        self.engine(Tiers::files_only())
    }
}

/// One row as a caller observes it: the id, and `blocked_by` when the query
/// carries it.
type ObservedRow = (String, Option<Vec<String>>);

/// `(id, blocked_by)` for every row, which is the whole observable surface of a
/// `Rows::Full` answer that a caller renders.
fn rows_of(rows: &Rows) -> Vec<ObservedRow> {
    match rows {
        Rows::Full(rows) => rows
            .iter()
            .map(|r| (r.frontmatter.id.to_string(), r.blocked_by.clone()))
            .collect(),
        Rows::Lean(rows) => rows.iter().map(|r| (r.id.clone(), None)).collect(),
    }
}

fn cases() -> Vec<(&'static str, Filters)> {
    vec![
        ("unfiltered", Filters::default()),
        (
            "status",
            Filters::parse_multi(&["open".into()], &[], &[], None, &[], None).unwrap(),
        ),
        (
            "type+priority",
            Filters::parse_multi(
                &[],
                &["bug".into(), "chore".into()],
                &[],
                None,
                &["0".into(), "1".into()],
                None,
            )
            .unwrap(),
        ),
        (
            "labels all-of",
            Filters::parse_multi(
                &[],
                &[],
                &["area:core".into(), "area:ios".into()],
                None,
                &[],
                None,
            )
            .unwrap(),
        ),
        (
            "assignee",
            Filters::parse_multi(&[], &[], &[], Some("ana"), &[], None).unwrap(),
        ),
        // `q` is the residue: SQL cannot express it, so this exercises the
        // path where the limit is *not* pushed down and `COUNT(*)` is not the
        // total.
        (
            "q residue",
            Filters::parse_multi(&[], &[], &[], None, &[], Some("widget")).unwrap(),
        ),
        (
            "q residue + label",
            Filters::parse_multi(&[], &[], &["area:core".into()], None, &[], Some("widget"))
                .unwrap(),
        ),
    ]
}

fn orders() -> Vec<Order> {
    let fields = [
        SortField::Rank,
        SortField::Priority,
        SortField::Created,
        SortField::Updated,
        SortField::Id,
        SortField::Status,
        SortField::Type,
    ];
    fields
        .into_iter()
        .flat_map(|field| [false, true].map(|descending| Order { field, descending }))
        .collect()
}

/// A hydrated index answer is row-for-row the file answer — same ids, same
/// order, same `total` — for every filter, every sort, and a window that
/// actually slices.
#[test]
fn the_index_tier_hydrates_to_exactly_the_file_answer() {
    let fx = fixture();
    let indexed = fx.indexed();
    let files = fx.files();

    for (name, filters) in cases() {
        for order in orders() {
            for window in [
                Page::unlimited(),
                Page::new(1, Some(2), 0),
                Page::new(0, Some(3), 0),
            ] {
                for query in ["list", "ready", "blocked"] {
                    let run = |engine: &Engine| {
                        match query {
                            "ready" => engine.ready(&filters, order, window, Projection::Full),
                            "blocked" => engine.blocked(&filters, order, window, Projection::Full),
                            _ => engine.list(&filters, order, window, Projection::Full),
                        }
                        .unwrap()
                    };

                    let got = run(&indexed);
                    let want = run(&files);
                    let label = format!(
                        "{query} / {name} / {:?} desc={} / offset={} limit={:?}",
                        order.field, order.descending, window.offset, window.limit
                    );
                    assert_eq!(
                        got.source,
                        Source::Index,
                        "{label}: the index must answer, or this comparison is vacuous"
                    );
                    assert_eq!(want.source, Source::Files, "{label}");
                    assert_eq!(got.total, want.total, "{label}: total");
                    assert_eq!(rows_of(&got.rows), rows_of(&want.rows), "{label}: rows");
                }
            }
        }
    }
}

/// `blocked` gained an index tier in read-path §4 — it was the one list that
/// could not answer from SQL at all. The set it answers with must be exactly
/// `GraphStore::blocked_items`, including the dangling-only item, and every row
/// must still carry `blocked_by`.
#[test]
fn blocked_answers_from_the_index_with_blocked_by_intact() {
    let fx = fixture();
    let answer = fx
        .indexed()
        .blocked(
            &Filters::default(),
            Order::default(),
            Page::unlimited(),
            Projection::Full,
        )
        .unwrap();
    assert_eq!(answer.source, Source::Index, "blocked has an index tier");

    let mut ids: Vec<String> = rows_of(&answer.rows)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    let mut want = vec![fx.ids[0].to_string(), fx.ids[4].to_string()];
    want.sort();
    assert_eq!(
        ids, want,
        "the blocked set is the open-dep and dangling items"
    );

    // `blocked_by` is the point of the list, and a lean row cannot carry it.
    let rows = rows_of(&answer.rows);
    let dangling = rows
        .iter()
        .find(|(id, _)| id == &fx.ids[4].to_string())
        .expect("the dangling item is blocked, not invisible");
    assert_eq!(
        dangling.1.as_deref(),
        Some(["proj-ZZZZZZZZ".to_owned()].as_slice()),
        "a missing dependency is reported as what blocks the item"
    );
}

/// `Projection::Files` is a caller *policy* — `clove ls --fields id,created`
/// uses it — so it must pin the answer to the file scan even with a live index
/// sitting right there.
#[test]
fn projection_files_refuses_the_index_even_when_it_is_live() {
    let fx = fixture();
    let engine = fx.indexed();
    // Same engine, same store, same query: only the projection differs.
    let tiered = engine
        .list(
            &Filters::default(),
            Order::default(),
            Page::unlimited(),
            Projection::Full,
        )
        .unwrap();
    assert_eq!(tiered.source, Source::Index, "the index is live");

    let pinned = engine
        .list(
            &Filters::default(),
            Order::default(),
            Page::unlimited(),
            Projection::Files,
        )
        .unwrap();
    assert_eq!(pinned.source, Source::Files);
    assert_eq!(rows_of(&pinned.rows), rows_of(&tiered.rows));
}

/// The engine windows before it returns, so a caller that renders what it is
/// given cannot double-page. `total` stays the pre-window match count on every
/// tier — including the residue path, where `COUNT(*)` is *not* the total.
#[test]
fn every_tier_returns_the_page_and_the_prewindow_total() {
    let fx = fixture();
    let q = Filters::parse_multi(&[], &[], &[], None, &[], Some("widget")).unwrap();
    for (label, engine) in [("index", fx.indexed()), ("files", fx.files())] {
        let all = engine
            .list(&q, Order::default(), Page::unlimited(), Projection::Full)
            .unwrap();
        assert_eq!(all.total, 3, "{label}: three titles contain `widget`");
        assert_eq!(all.rows.len(), 3, "{label}");

        let page = engine
            .list(
                &q,
                Order::default(),
                Page::new(1, Some(1), 0),
                Projection::Full,
            )
            .unwrap();
        assert_eq!(page.total, 3, "{label}: total is the pre-window count");
        assert_eq!(page.rows.len(), 1, "{label}: the window is already applied");
        assert_eq!(
            rows_of(&page.rows)[0],
            rows_of(&all.rows)[1],
            "{label}: offset 1 is the second row of the unwindowed answer"
        );
    }
}

/// `search` has one tier by design (read-path §6.1: the FTS matched whole
/// ASCII-folded tokens where the file scan matches Unicode substrings). A live
/// index must not change its answer *or* its reported source.
#[test]
fn search_reports_files_even_with_a_live_index() {
    let fx = fixture();
    let order = clove_core::view::SearchOrder::parse(None, None).unwrap();
    let indexed = fx
        .indexed()
        .search("widget", order, Page::unlimited())
        .unwrap();
    assert_eq!(indexed.source, Source::Files);
    // A mid-token needle no FTS could match, to prove the file scan really ran.
    let mid = fx
        .indexed()
        .search("idget", order, Page::unlimited())
        .unwrap();
    assert_eq!(mid.total, 3, "substring matching, not whole tokens");
}
