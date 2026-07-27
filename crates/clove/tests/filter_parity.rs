//! Read-path §2: one filter set, every surface.
//!
//! The load-bearing test for `clove_core::view::Filters` is a *triple*
//! comparison, the same shape `sort_order.rs` uses for `Order`: the file path
//! (`--no-index`), the index path (a SQL `WHERE`), and the daemon path (that SQL
//! behind an RPC) must return **identical id sequences** for every filter
//! combination. They are three independent implementations of one predicate — a
//! Rust `matches()`, a generated `WHERE` clause, and a wire field — and any
//! divergence means `--no-index` silently changes results.
//!
//! The interesting half is the *residue*. `q` cannot be pushed into SQL (SQLite
//! case-folds ASCII only; see `view::q_matches`), so the index and daemon paths
//! answer it in memory after the query. That changes three things at once —
//! which columns are selected, whether `LIMIT` may be pushed down, and whether
//! `COUNT(*)` is the total — so `q` gets its own pagination and `_meta.total`
//! checks on top of the id-sequence comparison.
//!
//! The fixture is built so each specific way of getting a filter wrong changes
//! the answer — a dropped filter, OR-ed labels, a multi-value field truncated to
//! its first element, a `q` that reaches the body. See
//! `the_fixture_discriminates_the_ways_a_filter_can_be_wrong`, which is the
//! guard on that claim.

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;

fn clove() -> Command {
    Command::new(cargo_bin("clove"))
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    clove().current_dir(dir).args(args).output().unwrap()
}

const A: &str = "proj-AAAAAAAA";
const B: &str = "proj-BBBBBBBB";
const C: &str = "proj-CCCCCCCC";
const D: &str = "proj-DDDDDDDD";
const E: &str = "proj-EEEEEEEE";

/// One fixture item, spelled out field by field.
#[derive(Clone, Copy)]
struct Fixture {
    id: &'static str,
    title: &'static str,
    status: &'static str,
    item_type: &'static str,
    priority: u8,
    labels: &'static [&'static str],
    assignee: Option<&'static str>,
    deps: &'static [&'static str],
}

/// Write one item file directly (not `clove new`: the ids, labels, and titles
/// all have to be controlled for the filter cases below to discriminate).
fn write_item(root: &Path, f: Fixture) {
    let Fixture {
        id,
        title,
        status,
        item_type,
        priority,
        labels,
        assignee,
        deps,
    } = f;
    // Timestamps vary per item, and `created` runs opposite to `updated`, so a
    // `--sort created|updated` case cannot pass by degenerating into the id
    // tiebreak. Every fixture item shared one constant here, which made the
    // `--sort updated` entry below prove nothing: pointing `SortField::Updated`
    // at the wrong column left this suite green.
    let nth = id.as_bytes().get(5).copied().unwrap_or(b'A') - b'A';
    let created_day = 2 + u32::from(nth);
    let updated_day = 28 - u32::from(nth);
    let mut s = format!(
        "---\nschema: 1\nid: {id}\ntitle: {title}\nstatus: {status}\ntype: {item_type}\n\
         priority: {priority}\ncreated: 2026-06-{created_day:02}T10:00:00Z\n\
         updated: 2026-06-{updated_day:02}T10:00:00Z\n"
    );
    if status == "closed" {
        s.push_str("closed: 2026-06-09T11:00:00Z\n");
    }
    if let Some(a) = assignee {
        s.push_str(&format!("assignee: {a}\n"));
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
    s.push_str("---\nthe body says gadget, which no filter may ever see\n");
    std::fs::write(root.join(".clove/issues").join(format!("{id}.md")), s).unwrap();
}

/// The five fixture items (see [`build_fixture`] for the table).
const ITEMS: [Fixture; 5] = [
    Fixture {
        id: A,
        title: "alpha widget",
        status: "open",
        item_type: "bug",
        priority: 0,
        labels: &["area:core", "area:ios"],
        assignee: Some("alice"),
        deps: &[],
    },
    Fixture {
        id: B,
        title: "beta gizmo",
        status: "in_progress",
        item_type: "feature",
        priority: 1,
        labels: &["area:core"],
        assignee: Some("bob"),
        deps: &[],
    },
    Fixture {
        id: C,
        title: "gamma widget",
        status: "closed",
        item_type: "chore",
        priority: 2,
        labels: &["area:ios"],
        assignee: None,
        deps: &[],
    },
    Fixture {
        id: D,
        title: "delta thing",
        status: "open",
        item_type: "docs",
        priority: 3,
        labels: &["area:core", "area:ios"],
        assignee: None,
        deps: &[],
    },
    Fixture {
        id: E,
        title: "epsilon widget",
        status: "closed",
        item_type: "epic",
        priority: 4,
        labels: &[],
        assignee: Some("alice"),
        deps: &[],
    },
];

/// The fixture item with this id.
fn item(id: &str) -> Fixture {
    *ITEMS.iter().find(|f| f.id == id).unwrap()
}

/// A five-item store in which every filter dimension cuts differently.
///
/// | id | status      | type    | pri | labels               | assignee | title          |
/// |----|-------------|---------|-----|----------------------|----------|----------------|
/// | A  | open        | bug     | 0   | area:core, area:ios  | alice    | alpha widget   |
/// | B  | in_progress | feature | 1   | area:core            | bob      | beta gizmo     |
/// | C  | closed      | chore   | 2   | area:ios             | —        | gamma widget   |
/// | D  | open        | docs    | 3   | area:core, area:ios  | —        | delta thing    |
/// | E  | closed      | epic    | 4   | —                    | alice    | epsilon widget |
///
/// A and D carry *both* labels while B and C carry one each — that is what makes
/// AND-ed labels distinguishable from OR-ed ones. Every body says `gadget`,
/// which nothing matches: `q` is a filter, not a search, and a `q` that reached
/// the body would return all five.
fn build_fixture(root: &Path) {
    assert!(run_in(root, &["init", "--prefix", "proj"]).status.success());
    for f in ITEMS {
        write_item(root, f);
    }
}

/// Every filter case: the flags, and the ids they must select (in `--sort id`
/// order, so the expectation is about the *set* and the order is pinned
/// separately by `sort_order.rs`).
///
/// Spelled out rather than derived — a bug that moves all three paths together
/// is still caught.
fn cases() -> Vec<(&'static str, Vec<&'static str>, Vec<&'static str>)> {
    vec![
        // --- single value: the pre-existing spelling, unchanged -------------
        ("status=open", vec!["--status", "open"], vec![A, D]),
        ("type=bug", vec!["--type", "bug"], vec![A]),
        ("priority=1", vec!["--priority", "1"], vec![B]),
        ("label=area:ios", vec!["--label", "area:ios"], vec![A, C, D]),
        ("assignee=alice", vec!["--assignee", "alice"], vec![A, E]),
        // --- any-of within a field ------------------------------------------
        (
            "status=open|in_progress",
            vec!["--status", "open", "--status", "in_progress"],
            vec![A, B, D],
        ),
        (
            "type=bug|docs",
            vec!["--type", "bug", "--type", "docs"],
            vec![A, D],
        ),
        (
            "priority=0|4",
            vec!["--priority", "0", "--priority", "4"],
            vec![A, E],
        ),
        // --- all-of within `label`: the filter that was browser-only ---------
        (
            "label=area:core&area:ios",
            vec!["--label", "area:core", "--label", "area:ios"],
            vec![A, D],
        ),
        // Canonicalization applies per element, so the same filter shouted.
        (
            "label=AREA:CORE&Area:iOS",
            vec!["--label", "AREA:CORE", "--label", "Area:iOS"],
            vec![A, D],
        ),
        // --- q: the index residue -------------------------------------------
        ("q=widget (titles)", vec!["--q", "widget"], vec![A, C, E]),
        ("q=WIDGET (case)", vec!["--q", "WIDGET"], vec![A, C, E]),
        (
            "q=area:ios (labels)",
            vec!["--q", "area:ios"],
            vec![A, C, D],
        ),
        // Ids are `proj-AAAAAAAA`…; `bbbb` hits exactly one of them.
        ("q=bbbb (id)", vec!["--q", "bbbb"], vec![B]),
        // The bodies all contain `gadget`. `q` must not see them.
        ("q=gadget (body)", vec!["--q", "gadget"], vec![]),
        // --- across fields: all-of, including a residue beside SQL ----------
        (
            "status+label",
            vec!["--status", "open", "--label", "area:ios"],
            vec![A, D],
        ),
        (
            "status+q",
            vec!["--status", "open", "--status", "closed", "--q", "widget"],
            vec![A, C, E],
        ),
        (
            "status+label+q",
            vec![
                "--status", "open", "--status", "closed", "--label", "area:ios", "--q", "widget",
            ],
            vec![A, C],
        ),
        (
            "type+priority+q",
            vec![
                "--type",
                "bug",
                "--type",
                "epic",
                "--priority",
                "0",
                "--priority",
                "4",
                "--q",
                "widget",
            ],
            vec![A, E],
        ),
        // An empty intersection is an empty list, not an ignored filter.
        (
            "type+priority (disjoint)",
            vec!["--type", "bug", "--priority", "4"],
            vec![],
        ),
        // --- unconstrained ---------------------------------------------------
        ("none", vec![], vec![A, B, C, D, E]),
    ]
}

/// `(ids, _meta)` from a `--format json` list response.
fn ids_and_meta(out: &[u8]) -> (Vec<String>, serde_json::Value) {
    let v: serde_json::Value = serde_json::from_slice(out)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(out)));
    let ids = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("not a list response: {v}"))
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_owned())
        .collect();
    (ids, v["_meta"].clone())
}

/// `clove ls --sort id -f json <flags> [extra]`.
fn ls_args<'a>(flags: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["ls", "--sort", "id", "-f", "json"];
    args.extend_from_slice(flags);
    args.extend_from_slice(extra);
    args
}

/// The fixture must discriminate against the *failure modes this file is for*.
///
/// Pairwise distinctness across all cases is the wrong bar — five items cannot
/// give twenty-one different subsets, and `--status open` legitimately selecting
/// the same two ids as `--type bug --type docs` proves nothing either way. What
/// must hold is that each specific way of getting a filter wrong changes the
/// answer:
///
/// - **dropping the filter** (a lost wire field, a missing `WHERE` term) → every
///   case must differ from the unfiltered set;
/// - **OR-ing labels instead of AND-ing them** → the AND result must differ from
///   the union of its parts;
/// - **keeping only the first value of a multi-value field** → the multi result
///   must differ from each of its single-value halves;
/// - **letting `q` read the body** → `q=gadget` must be empty while every body
///   contains it.

#[test]
fn the_fixture_discriminates_the_ways_a_filter_can_be_wrong() {
    let by_name = |name: &str| -> Vec<&'static str> {
        cases()
            .into_iter()
            .find(|(n, _, _)| *n == name)
            .unwrap_or_else(|| panic!("no case named {name}"))
            .2
    };
    let everything = by_name("none");
    assert_eq!(everything.len(), 5);

    for (name, _, ids) in cases() {
        if name == "none" {
            continue;
        }
        assert_ne!(
            ids, everything,
            "`{name}` selects the whole store — ignoring it entirely would pass"
        );
    }

    // AND vs OR on labels.
    let and = by_name("label=area:core&area:ios");
    let ios = by_name("label=area:ios");
    assert_ne!(and, ios, "AND-ed labels must differ from one of them alone");
    assert!(
        and.len() < ios.len(),
        "AND must be strictly narrower than either part: {and:?} vs {ios:?}"
    );

    // Multi-value vs each of its halves.
    assert_ne!(by_name("status=open|in_progress"), by_name("status=open"));
    assert_ne!(by_name("type=bug|docs"), by_name("type=bug"));
    assert_ne!(by_name("priority=0|4"), by_name("priority=1"));

    // A residue stacked on SQL must narrow further than the SQL alone.
    assert_ne!(by_name("status+label+q"), by_name("status+label"));

    // `q` must never reach the body, and every body carries the needle.
    assert!(by_name("q=gadget (body)").is_empty());
}

/// The file path and the index path agree, for every filter combination — on
/// `ls` (a plain scan vs. a `WHERE`) and on `ready` (a different predicate on
/// both sides: `GraphStore::ready_items` vs. the ready SQL).
///
/// Runs everywhere, no daemon needed: this is the half `--no-index` exposes
/// directly to users.
#[test]
fn file_and_index_paths_agree_for_every_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);
    assert!(run_in(root, &["reindex"]).status.success());

    for (name, flags, want) in cases() {
        let want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();

        let (files, meta) = ids_and_meta(&run_in(root, &ls_args(&flags, &["--no-index"])).stdout);
        assert_eq!(
            meta["source"], "files",
            "--no-index must take the file path"
        );
        assert_eq!(files, want, "file path, {name}");
        assert_eq!(meta["total"], want.len(), "file path total, {name}");

        let (index, meta) = ids_and_meta(&run_in(root, &ls_args(&flags, &[])).stdout);
        assert_eq!(
            meta["source"], "index",
            "the index must answer, or this comparison is vacuous ({name})"
        );
        assert_eq!(index, files, "index path, {name}");
        assert_eq!(
            meta["total"],
            want.len(),
            "index path total, {name} — `COUNT(*)` is not the total once a \
             residue applies"
        );

        // `ready` runs a different predicate on both sides. Only the three
        // active items can be ready here (nothing depends on anything), so the
        // expectation is the case's set intersected with {A, B, D}.
        let ready_args = |extra: &[&'static str]| -> Vec<&str> {
            let mut a = vec!["ready", "--sort", "id", "-f", "json"];
            a.extend_from_slice(&flags);
            a.extend_from_slice(extra);
            a
        };
        let (ready_files, meta) = ids_and_meta(&run_in(root, &ready_args(&["--no-index"])).stdout);
        assert_eq!(meta["source"], "files");
        let (ready_index, meta) = ids_and_meta(&run_in(root, &ready_args(&[])).stdout);
        assert_eq!(meta["source"], "index");
        assert_eq!(ready_index, ready_files, "ready, {name}");
        let expect_ready: Vec<String> = want
            .iter()
            .filter(|id| [A, B, D].contains(&id.as_str()))
            .cloned()
            .collect();
        assert_eq!(ready_files, expect_ready, "ready set, {name}");
    }
}

/// `blocked` has no index tier, but it does have a daemon tier and a file tier,
/// and both apply the filters locally. Checked separately because its file path
/// is a third call site of `matches()`.
#[test]
fn blocked_applies_the_same_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);
    // Make A and B blocked on the open D.
    write_item(
        root,
        Fixture {
            deps: &[D],
            ..item(A)
        },
    );
    write_item(
        root,
        Fixture {
            deps: &[D],
            ..item(B)
        },
    );

    let ids = |flags: &[&str]| -> Vec<String> {
        let mut a = vec!["--no-index", "blocked", "--sort", "id", "-f", "json"];
        a.extend_from_slice(flags);
        ids_and_meta(&run_in(root, &a).stdout).0
    };
    assert_eq!(ids(&[]), vec![A.to_owned(), B.to_owned()]);
    assert_eq!(ids(&["--type", "bug"]), vec![A.to_owned()]);
    assert_eq!(
        ids(&["--status", "open", "--status", "in_progress"]),
        vec![A.to_owned(), B.to_owned()],
    );
    assert_eq!(
        ids(&["--label", "area:core", "--label", "area:ios"]),
        vec![A.to_owned()],
        "labels are AND-ed on `blocked` too",
    );
    assert_eq!(ids(&["--q", "gizmo"]), vec![B.to_owned()]);
    assert!(ids(&["--q", "gadget"]).is_empty(), "`q` never reads bodies");
}

/// Paging is correct under a residue: consecutive windows tile the result set
/// exactly once, and `_meta.total` is the post-residue count.
///
/// This is the case the residue exists to get right. With `q` pushed into the
/// SQL `LIMIT offset+limit`, the window would slice *before* the residue removed
/// rows, so a page would come back short and the last page would be missing
/// items — while `COUNT(*)` reported a total that included the rows `q` rejects.
#[test]
fn a_residue_pages_and_counts_correctly_on_the_index_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);
    assert!(run_in(root, &["reindex"]).status.success());

    // `widget` selects A, C, E — three rows drawn from five, interleaved with
    // rejects, so an off-by-one in the window shows up as a wrong id.
    let (all, meta) = ids_and_meta(&run_in(root, &ls_args(&["--q", "widget"], &[])).stdout);
    assert_eq!(meta["source"], "index");
    assert_eq!(all, vec![A.to_owned(), C.to_owned(), E.to_owned()]);
    assert_eq!(meta["total"], 3, "the total counts post-residue rows");

    let mut paged: Vec<String> = Vec::new();
    for offset in ["0", "2"] {
        let args = ls_args(&["--q", "widget"], &["--limit", "2", "--offset", offset]);
        let (ids, meta) = ids_and_meta(&run_in(root, &args).stdout);
        assert_eq!(meta["source"], "index");
        assert_eq!(meta["total"], 3, "every page reports the same full total");
        paged.extend(ids);
    }
    assert_eq!(
        paged, all,
        "windows must tile the residue-filtered set once"
    );

    // ...and the file path answers the same windows identically.
    for offset in ["0", "2"] {
        let args = ls_args(
            &["--q", "widget"],
            &["--limit", "2", "--offset", offset, "--no-index"],
        );
        let (files, _) = ids_and_meta(&run_in(root, &args).stdout);
        let args = ls_args(&["--q", "widget"], &["--limit", "2", "--offset", offset]);
        let (index, _) = ids_and_meta(&run_in(root, &args).stdout);
        assert_eq!(files, index, "offset {offset}");
    }
}

/// `_meta.filters` echoes the **parsed** filter set, so a client can confirm
/// what was applied rather than assume its input survived canonicalization.
#[test]
fn meta_echoes_the_parsed_filter_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);

    let meta = |args: &[&str]| -> serde_json::Value { ids_and_meta(&run_in(root, args).stdout).1 };

    // The unconstrained case is echoed too — an absent key would be
    // indistinguishable from a surface that does not report filters.
    let m = meta(&["--no-index", "ls", "-f", "json"]);
    assert_eq!(m["filters"]["status"], serde_json::json!([]));
    assert_eq!(m["filters"]["type"], serde_json::json!([]));
    assert_eq!(m["filters"]["priority"], serde_json::json!([]));
    assert_eq!(m["filters"]["labels"], serde_json::json!([]));
    assert_eq!(m["filters"]["assignee"], serde_json::Value::Null);
    assert_eq!(m["filters"]["q"], serde_json::Value::Null);

    let m = meta(&[
        "--no-index",
        "ls",
        "-f",
        "json",
        "--status",
        "open",
        "--status",
        "started", // an alias — it must echo canonicalized
        "--type",
        "bug",
        "--label",
        "AREA:Core",
        "--priority",
        "1",
        "--assignee",
        "alice",
        "--q",
        "Widget",
    ]);
    assert_eq!(
        m["filters"]["status"],
        serde_json::json!(["open", "in_progress"]),
        "the `started` alias is echoed as the canonical word"
    );
    assert_eq!(m["filters"]["type"], serde_json::json!(["bug"]));
    assert_eq!(
        m["filters"]["labels"],
        serde_json::json!(["area:core"]),
        "labels are echoed canonicalized, not as typed"
    );
    assert_eq!(m["filters"]["priority"], serde_json::json!([1]));
    assert_eq!(m["filters"]["assignee"], "alice");
    assert_eq!(m["filters"]["q"], "Widget");

    // The index path echoes the same thing (it is the same parsed value).
    assert!(run_in(root, &["reindex"]).status.success());
    let m = meta(&["ls", "-f", "json", "--label", "Area:IOS"]);
    assert_eq!(m["source"], "index");
    assert_eq!(m["filters"]["labels"], serde_json::json!(["area:ios"]));

    // `search` takes no field filters, so it advertises none.
    let m = meta(&["--no-index", "search", "widget", "-f", "json"]);
    assert!(
        m.get("filters").is_none(),
        "search must not claim a filter surface it does not have: {m}"
    );
}

/// An invalid filter value is a validation error on every list command, not a
/// filter that silently matches nothing.
/// A query SQLite cannot *shape* falls back to the files instead of failing.
///
/// Each AND-ed label is its own `EXISTS` subquery and SQLite caps expression
/// depth at 1000, so around 997 repeated `--label` values raised
/// `Expression tree is too large` — reported as `IO_ERROR`/exit 5, a broken
/// store — while `--no-index` answered the same query normally. The index is a
/// cache; the shape of a query is not a reason to refuse it.
#[test]
fn an_over_deep_filter_falls_back_to_the_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_fixture(root);
    assert!(run_in(root, &["reindex"]).status.success());

    // Well past SQLite's limit, and deliberately satisfiable: every repetition
    // is the same label, so the answer is the same as asking once.
    let mut args: Vec<String> = vec!["ls".into(), "-f".into(), "json".into()];
    for _ in 0..1200 {
        args.push("--label".into());
        args.push("area:core".into());
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_in(root, &argv);
    assert!(
        out.status.success(),
        "an over-deep filter must not fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (deep_ids, meta) = ids_and_meta(&out.stdout);
    assert_eq!(
        meta["source"], "files",
        "it must have fallen back off the index"
    );

    // ...and the fallback is the *right* answer, not merely a successful one.
    let mut once = argv.clone();
    once.truncate(3);
    once.extend_from_slice(&["--label", "area:core"]);
    let (want, _) = ids_and_meta(&run_in(root, &once).stdout);
    assert_eq!(
        deep_ids, want,
        "the fallback must agree with a single label"
    );
    assert!(
        !want.is_empty(),
        "the fixture must actually match something"
    );
}

#[test]
fn invalid_filter_values_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);

    for args in [
        vec!["ls", "--status", "open", "--status", "paused", "-f", "json"],
        vec!["ready", "--type", "saga", "-f", "json"],
        vec!["blocked", "--label", "   ", "-f", "json"],
        vec![
            "query",
            "--filter",
            r#"{"status":["open","paused"]}"#,
            "-f",
            "json",
        ],
    ] {
        let out = run_in(root, &args);
        assert!(!out.status.success(), "{args:?} should have failed");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], false, "{args:?}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{args:?}");
    }
    // clap still owns the numeric parse for `--priority`, so a non-number is
    // its error (exit 2) rather than ours — unchanged from before multi-value.
    let out = run_in(root, &["ls", "--priority", "abc", "-f", "json"]);
    assert!(!out.status.success());
}

/// `clove query`'s JSON filter accepts one value or a list on every filter
/// field, and the single-value spelling parses exactly as it did before.
#[test]
fn query_json_filter_takes_one_value_or_many() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);

    let ids = |filter: &str| -> Vec<String> {
        let args = [
            "--no-index",
            "query",
            "--filter",
            filter,
            "--sort",
            "id",
            "-f",
            "json",
        ];
        ids_and_meta(&run_in(root, &args).stdout).0
    };

    // The pre-existing scalar spellings, byte for byte.
    assert_eq!(
        ids(r#"{"status":"open"}"#),
        vec![A.to_owned(), D.to_owned()]
    );
    assert_eq!(ids(r#"{"priority":1}"#), vec![B.to_owned()]);
    assert_eq!(ids(r#"{"type":"bug"}"#), vec![A.to_owned()]);
    assert_eq!(
        ids(r#"{"label":"area:ios"}"#),
        vec![A.to_owned(), C.to_owned(), D.to_owned()]
    );
    // ...and the list forms.
    assert_eq!(
        ids(r#"{"status":["open","in_progress"]}"#),
        vec![A.to_owned(), B.to_owned(), D.to_owned()]
    );
    assert_eq!(
        ids(r#"{"priority":[0,4]}"#),
        vec![A.to_owned(), E.to_owned()]
    );
    assert_eq!(
        ids(r#"{"label":["area:core","area:ios"]}"#),
        vec![A.to_owned(), D.to_owned()],
        "labels AND in the JSON filter too"
    );
    assert_eq!(
        ids(r#"{"q":"widget"}"#),
        vec![A.to_owned(), C.to_owned(), E.to_owned()]
    );
}

/// The third path: a live `cloved`. The filters ride `QueryRequest.filters`, so
/// a dropped wire field shows up here as the *unfiltered* list.
#[cfg(unix)]
// The daemon is reaped by `sigterm` + `wait` at the end of each test; clippy
// cannot see across the SIGTERM, so the lint is suppressed here as it is in
// `sort_order.rs` and `daemon_routing.rs`.
#[allow(clippy::zombie_processes)]
mod daemon {
    use super::*;
    use std::path::PathBuf;
    use std::process::Child;
    use std::time::{Duration, Instant};

    /// Build `cloved` on demand rather than hoping it is already in `target/`.
    ///
    /// `cargo test -p clove-cli` does not rebuild a sibling binary, so a test
    /// that merely *skipped* when `target/debug/cloved` was absent would report
    /// **ok** while never comparing the third path at all. That happened once on
    /// this branch already (see `sort_order.rs`), so this test guarantees its
    /// own precondition instead.
    fn cloved_bin() -> PathBuf {
        escargot::CargoBuild::new()
            .package("cloved")
            .bin("cloved")
            .run()
            .expect("build cloved for the daemon filter comparison")
            .path()
            .to_path_buf()
    }

    fn spawn_daemon(clove_dir: &Path, bin: &Path) -> Child {
        let child = Command::new(bin)
            .arg("run")
            .arg("--clove-dir")
            .arg(clove_dir)
            .spawn()
            .expect("spawn cloved");
        // Readiness is the socket, not the pid — see the note in `sort_order.rs`.
        let pid = clove_dir.join("daemon.pid");
        let sock = clove_dir.join("daemon.sock");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if pid.exists() && sock.exists() {
                return child;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not ready");
    }

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
    fn sigterm(pid: u32) {
        unsafe {
            libc_kill(pid as i32, 15);
        }
    }

    #[test]
    fn daemon_path_agrees_with_the_file_path_for_every_filter() {
        let bin = cloved_bin();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        build_fixture(root);
        assert!(run_in(root, &["reindex"]).status.success());

        // Ground truth from the file path, gathered before the daemon exists.
        let truth: Vec<(&str, Vec<&str>, Vec<String>, usize)> = cases()
            .into_iter()
            .flat_map(|(_name, flags, _)| {
                ["ls", "ready"].into_iter().map(move |cmd| {
                    let mut a = vec!["--no-index", cmd, "--sort", "id", "-f", "json"];
                    a.extend_from_slice(&flags);
                    let (ids, meta) = ids_and_meta(&run_in(root, &a).stdout);
                    assert_eq!(meta["source"], "files");
                    let total = meta["total"].as_u64().unwrap() as usize;
                    (cmd, flags.clone(), ids, total)
                })
            })
            .collect();

        let clove_dir = root.join(".clove");
        let mut daemon = spawn_daemon(&clove_dir, &bin);

        let mut failures = Vec::new();
        for (cmd, flags, want, want_total) in &truth {
            let mut a = vec![*cmd, "--sort", "id", "-f", "json"];
            a.extend(flags.iter().copied());
            let (ids, meta) = ids_and_meta(&run_in(root, &a).stdout);
            if meta["source"] != "daemon" {
                failures.push(format!(
                    "{cmd} {flags:?}: served by `{}`, not the daemon",
                    meta["source"]
                ));
            } else if ids != *want {
                failures.push(format!("{cmd} {flags:?}: daemon {ids:?} != files {want:?}"));
            } else if meta["total"] != *want_total {
                failures.push(format!(
                    "{cmd} {flags:?}: daemon total {} != files {want_total}",
                    meta["total"]
                ));
            }
        }

        sigterm(daemon.id());
        let _ = daemon.wait();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// `blocked`'s daemon RPC now carries the sort, and the CLI no longer
    /// re-sorts locally — so an order the daemon fails to apply is visible here
    /// as a difference from the file path rather than being papered over.
    ///
    /// The companion of `sort_order.rs::blocked_daemon_path_agrees_with_the_file_path`,
    /// kept there for ordering; this one adds the filters on top.
    #[test]
    fn blocked_daemon_path_agrees_with_the_file_path_for_every_filter() {
        let bin = cloved_bin();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        build_fixture(root);
        // A and B block on the open D; C blocks on an id that does not exist.
        //
        // The dangling case is not decoration: it is the one the removed
        // `include_warnings` flag used to gate, and `cmd/blocked.rs` still
        // reasons about it. The comment claimed C was dangling long before C
        // actually was — it was never rewritten — so the daemon path was never
        // exercised with a blocked item whose dependency is missing.
        write_item(
            root,
            Fixture {
                deps: &[D],
                ..item(A)
            },
        );
        write_item(
            root,
            Fixture {
                deps: &[D],
                ..item(B)
            },
        );
        write_item(
            root,
            Fixture {
                deps: &["proj-MISSING1"],
                ..item(C)
            },
        );
        assert!(run_in(root, &["reindex"]).status.success());

        let flag_sets: Vec<Vec<&str>> = vec![
            vec![],
            vec!["--status", "open", "--status", "in_progress"],
            vec!["--type", "bug", "--type", "feature"],
            vec!["--label", "area:core", "--label", "area:ios"],
            vec!["--q", "gizmo"],
            vec!["--q", "gadget"],
            vec!["--priority", "0", "--priority", "1"],
        ];
        let fields = [
            "rank", "priority", "id", "created", "updated", "status", "type",
        ];

        let truth: Vec<(Vec<&str>, &str, bool, Vec<String>)> = flag_sets
            .iter()
            .flat_map(|flags| {
                fields.iter().flat_map(move |field| {
                    [false, true].map(move |desc| (flags.clone(), *field, desc))
                })
            })
            .map(|(flags, field, desc)| {
                let mut a = vec!["--no-index", "blocked", "--sort", field, "-f", "json"];
                if desc {
                    a.push("--desc");
                }
                a.extend(flags.iter().copied());
                let (ids, meta) = ids_and_meta(&run_in(root, &a).stdout);
                assert_eq!(meta["source"], "files");
                (flags, field, desc, ids)
            })
            .collect();
        // The fixture must be non-trivial, or "the daemon ignored everything"
        // would pass.
        assert!(
            truth.iter().any(|(_, _, _, ids)| ids.len() == 2)
                && truth.iter().any(|(_, _, _, ids)| ids.len() == 1),
            "blocked fixture is weak"
        );

        let clove_dir = root.join(".clove");
        let mut daemon = spawn_daemon(&clove_dir, &bin);
        let mut failures = Vec::new();
        for (flags, field, desc, want) in &truth {
            let mut a = vec!["blocked", "--sort", field, "-f", "json"];
            if *desc {
                a.push("--desc");
            }
            a.extend(flags.iter().copied());
            let (ids, meta) = ids_and_meta(&run_in(root, &a).stdout);
            if meta["source"] != "daemon" {
                failures.push(format!(
                    "{flags:?} --sort {field} desc={desc}: served by `{}`",
                    meta["source"]
                ));
            } else if ids != *want {
                failures.push(format!(
                    "{flags:?} --sort {field} desc={desc}: daemon {ids:?} != files {want:?}"
                ));
            }
        }
        sigterm(daemon.id());
        let _ = daemon.wait();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
