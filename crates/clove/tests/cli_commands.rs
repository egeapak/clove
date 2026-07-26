//! End-to-end CLI tests for the M0 command surface and index wiring.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir`, with a clean environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// Initialize a repo in a fresh temp dir and return it.
fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// Run a command expecting JSON success and return the parsed envelope.
fn json_ok(cmd: &mut Command) -> Value {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["ok"], true, "envelope not ok: {v}");
    v
}

/// Create an item and return its id.
fn new_item(dir: &Path, title: &str, extra: &[&str]) -> String {
    let mut cmd = clove(dir);
    cmd.arg("new").arg(title).args(extra);
    let v = json_ok(&mut cmd);
    v["data"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn init_is_idempotent_and_writes_gitignore() {
    let dir = init_repo();
    // Second init does not fail and does not overwrite config.
    clove(dir.path()).arg("init").assert().success();

    let gitignore = std::fs::read_to_string(dir.path().join(".clove/.gitignore")).unwrap();
    for entry in [
        "index.db",
        "*.db-shm",
        "*.db-wal",
        "daemon.sock",
        "daemon.pid",
        "reindex.lock",
        "daemon.lock",
        "index.db.tmp",
    ] {
        assert!(gitignore.contains(entry), "missing {entry}");
    }
    assert!(!gitignore.contains('\r'), "gitignore must use LF endings");
    assert!(dir.path().join(".clove/config.toml").exists());
}

#[test]
fn new_show_round_trip() {
    let dir = init_repo();
    let id = new_item(dir.path(), "A task", &["--type", "bug", "-p", "1"]);

    let v = json_ok(clove(dir.path()).arg("show").arg(&id));
    assert_eq!(v["data"]["id"], id);
    assert_eq!(v["data"]["type"], "bug");
    assert_eq!(v["data"]["priority"], 1);
    assert_eq!(v["data"]["status"], "open");
}

#[test]
fn ready_and_blocked_partition_by_dependency() {
    let dir = init_repo();
    let dep = new_item(dir.path(), "Dependency", &[]);
    let blocked = new_item(dir.path(), "Dependent", &["--dep", &dep]);

    // The dependent is blocked; the dependency is ready.
    let ready = json_ok(clove(dir.path()).arg("ready"));
    let ready_ids: Vec<&str> = ready["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ready_ids.contains(&dep.as_str()));
    assert!(!ready_ids.contains(&blocked.as_str()));

    let blk = json_ok(clove(dir.path()).arg("blocked"));
    let blk_ids: Vec<&str> = blk["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(blk_ids.contains(&blocked.as_str()));

    // Closing the dependency makes the dependent ready.
    clove(dir.path()).arg("close").arg(&dep).assert().success();
    let ready2 = json_ok(clove(dir.path()).arg("ready"));
    let ready2_ids: Vec<&str> = ready2["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ready2_ids.contains(&blocked.as_str()));
}

#[test]
fn close_sets_then_clears_closed_timestamp() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Closable", &[]);

    let closed = json_ok(clove(dir.path()).arg("close").arg(&id));
    assert_eq!(closed["data"]["status"], "closed");
    assert!(closed["data"]["closed"].is_string());
    // The transition samples the clock once and uses that single timestamp for
    // both the `closed` field and the write's `updated`. Sampling twice (as the
    // pre-`update_with` code did) let them land a second apart.
    assert_eq!(
        closed["data"]["closed"], closed["data"]["updated"],
        "closed and updated must come from one clock sample"
    );

    let reopened = json_ok(clove(dir.path()).args(["status", &id, "open"]));
    assert_eq!(reopened["data"]["status"], "open");
    assert!(reopened["data"]["closed"].is_null());
}

#[test]
fn labels_are_canonicalized_and_filterable() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Labeled", &[]);

    clove(dir.path())
        .args(["label", &id, "add", "Area:iOS"])
        .assert()
        .success();
    // Adding the canonical form again is a no-op (single label).
    let v = json_ok(clove(dir.path()).args(["label", &id, "add", "area:ios"]));
    assert_eq!(v["data"]["labels"], serde_json::json!(["area:ios"]));

    // Filter matches regardless of input case.
    let ls = json_ok(clove(dir.path()).args(["ls", "--label", "AREA:IOS"]));
    assert_eq!(ls["data"].as_array().unwrap().len(), 1);

    // Remove with a non-canonical argument.
    let removed = json_ok(clove(dir.path()).args(["label", &id, "rm", "AREA:IOS"]));
    assert_eq!(removed["data"]["labels"], serde_json::json!([]));
}

#[test]
fn priority_out_of_range_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "P", &[]);
    clove(dir.path())
        .args(["priority", &id, "5"])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn dep_validation_exit_codes() {
    let dir = init_repo();
    let a = new_item(dir.path(), "A", &[]);
    let b = new_item(dir.path(), "B", &[]);

    // self-dependency → exit 4
    clove(dir.path())
        .args(["dep", "add", &a, &a])
        .assert()
        .failure()
        .code(4);

    // missing dependency target → exit 2
    clove(dir.path())
        .args(["dep", "add", &a, "proj-ZZZZZZZZ"])
        .assert()
        .failure()
        .code(2);

    // a → b, then b → a would cycle → exit 3
    clove(dir.path())
        .args(["dep", "add", &a, &b])
        .assert()
        .success();
    clove(dir.path())
        .args(["dep", "add", &b, &a])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn dep_rm_absent_dependency_errors_but_present_one_removes() {
    let dir = init_repo();
    let a = new_item(dir.path(), "A", &[]);
    let b = new_item(dir.path(), "B", &[]);

    // Removing a dependency that doesn't exist must fail (same as web/MCP/daemon,
    // which route through ops::dep_remove) rather than silently no-op → exit 4.
    let out = clove(dir.path())
        .args(["dep", "rm", &a, &b, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["exit"], 4);

    // Happy path: add then remove an existing dependency succeeds and clears it.
    clove(dir.path())
        .args(["dep", "add", &a, &b])
        .assert()
        .success();
    let added = json_ok(clove(dir.path()).arg("show").arg(&a));
    assert!(added["data"]["deps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d == &b));

    let removed = json_ok(clove(dir.path()).args(["dep", "rm", &a, &b]));
    assert!(removed["data"]["deps"].as_array().unwrap().is_empty());
}

#[test]
fn show_missing_item_json_error_envelope() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["show", "proj-ZZZZZZZZ", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ITEM_NOT_FOUND");
    assert_eq!(v["error"]["exit"], 2);
}

/// `clove search` scans files whether or not an index exists — and reports so.
///
/// This test used to assert the opposite (`source == "index"` after a reindex).
/// The index tier is gone: it ran an FTS5 phrase query, which matches whole
/// ASCII-folded tokens, so it could not answer the substring question the file
/// path answers, and `clove search` gave different results depending on whether
/// `.clove/index.db` existed (read-path roadmap §6.1). `--no-index` is still
/// accepted on `search`; it is now a no-op.
#[test]
fn search_scans_files_even_with_an_index_present() {
    let dir = init_repo();
    new_item(
        dir.path(),
        "Findable widget",
        &["-b", "the body mentions sprockets"],
    );
    new_item(dir.path(), "Other", &[]);

    clove(dir.path()).arg("reindex").assert().success();
    // The index has to be real, or "search used files" says nothing.
    assert_eq!(
        json_ok(clove(dir.path()).args(["ls"]))["_meta"]["source"],
        "index"
    );

    let v = json_ok(clove(dir.path()).args(["search", "sprockets"]));
    assert_eq!(v["_meta"]["source"], "files");
    assert_eq!(v["data"].as_array().unwrap().len(), 1);

    // `--no-index` changes nothing, because there was nothing to opt out of.
    let v2 = json_ok(clove(dir.path()).args(["search", "sprockets", "--no-index"]));
    assert_eq!(v2["_meta"]["source"], "files");
    assert_eq!(v2["data"], v["data"]);

    let v3 = json_ok(clove(dir.path()).args(["search", "widget", "--no-index"]));
    assert_eq!(v3["_meta"]["source"], "files");
    assert_eq!(v3["data"].as_array().unwrap().len(), 1);
}

#[test]
fn comment_add_then_list() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Discussed", &[]);
    clove(dir.path())
        .args(["comment", &id, "first note"])
        .assert()
        .success();
    let v = json_ok(clove(dir.path()).args(["comments", &id]));
    let comments = v["data"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "first note");
    // The author is stored as a filename-safe slug derived from the email.
    assert!(comments[0]["author"].as_str().unwrap().contains("tester"));
}

#[test]
fn agent_doc_is_idempotent_and_checks_schema() {
    let dir = init_repo();
    let a = clove(dir.path()).arg("agent-doc").output().unwrap();
    let b = clove(dir.path()).arg("agent-doc").output().unwrap();
    assert_eq!(a.stdout, b.stdout, "agent-doc must be byte-identical");

    let doc_path = dir.path().join("AGENTS.md");
    std::fs::write(&doc_path, &a.stdout).unwrap();
    clove(dir.path())
        .args(["agent-doc", "--check", "--file"])
        .arg(&doc_path)
        .assert()
        .success();
}

#[test]
fn doctor_reports_and_fixes_safe_issues() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Has issues", &[]);

    // Seed a non-canonical label by editing the file directly, and an orphan
    // comment directory.
    let item_path = dir.path().join(format!(".clove/issues/{id}.md"));
    let contents = std::fs::read_to_string(&item_path).unwrap();
    let contents = contents.replace("labels: []", "labels:\n  - Area:iOS");
    std::fs::write(&item_path, contents).unwrap();
    std::fs::create_dir_all(dir.path().join(".clove/issues/proj-ORPHAN00/comments")).unwrap();

    // doctor reports two fixable warnings.
    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(report["data"]["summary"]["warnings"].as_u64().unwrap() >= 1);

    // --fix resolves them; a subsequent run is clean.
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let after = json_ok(clove(dir.path()).arg("doctor"));
    assert_eq!(after["data"]["summary"]["warnings"], 0);
    assert_eq!(after["data"]["summary"]["errors"], 0);
    assert!(!dir.path().join(".clove/issues/proj-ORPHAN00").exists());
}

#[test]
fn doctor_strict_exits_4_on_errors() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Dangling", &[]);
    // Introduce a dangling dependency by hand-editing.
    let item_path = dir.path().join(format!(".clove/issues/{id}.md"));
    let contents = std::fs::read_to_string(&item_path).unwrap();
    let contents = contents.replace("deps: []", "deps:\n  - proj-MISSING0");
    std::fs::write(&item_path, contents).unwrap();

    clove(dir.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn ls_index_serves_lean_rows_in_same_order_as_files() {
    let dir = init_repo();
    let dep = new_item(dir.path(), "Dep", &[]);
    new_item(dir.path(), "Other", &["--dep", &dep, "--type", "bug"]);

    // Before indexing, the file scan is used (full frontmatter objects).
    let files = json_ok(clove(dir.path()).arg("ls"));
    assert_eq!(files["_meta"]["source"], "files");
    let file_ids: Vec<&str> = files["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    // The file path includes full fields like `deps`.
    assert!(files["data"][0].get("deps").is_some());

    clove(dir.path()).arg("reindex").assert().success();

    // After indexing, ls uses the index and serves the lean projection
    // (id/status/type/priority/title) in the SAME id order.
    let indexed = json_ok(clove(dir.path()).arg("ls"));
    assert_eq!(indexed["_meta"]["source"], "index");
    let index_ids: Vec<&str> = indexed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(index_ids, file_ids, "index and file ls must agree on order");

    let mut keys: Vec<&str> = indexed["data"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["id", "priority", "status", "title", "type"]);

    // --no-index forces the (full) file path.
    let forced = json_ok(clove(dir.path()).args(["ls", "--no-index"]));
    assert_eq!(forced["_meta"]["source"], "files");
    assert!(forced["data"][0].get("deps").is_some());
}

#[test]
fn search_follows_the_shared_limit_contract() {
    let dir = init_repo();
    let issues = dir.path().join(".clove/issues");
    for i in 0..120u32 {
        let id = format!("proj-{i:08}");
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: Needle {i}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    // Default: capped at 100 like every other list command.
    let v = json_ok(clove(dir.path()).args(["search", "needle"]));
    assert_eq!(v["_meta"]["returned"], 100);
    assert_eq!(v["_meta"]["total"], 120);

    // `--limit 0` means unlimited, not zero rows.
    let all = json_ok(clove(dir.path()).args(["search", "needle", "--limit", "0"]));
    assert_eq!(all["_meta"]["returned"], 120);

    // An explicit limit is honored (index path too).
    clove(dir.path()).arg("reindex").assert().success();
    let idx = json_ok(clove(dir.path()).args(["search", "needle", "--limit", "5"]));
    assert_eq!(idx["_meta"]["returned"], 5);
    assert_eq!(idx["_meta"]["total"], 120);
}

/// `clove search` returns identical ids in identical order whether or not
/// `.clove/index.db` exists.
///
/// Scoped to the CLI, which is what this drives; the MCP tool shares the same
/// `rank_search_hits`, and the daemon leg is
/// `search_does_not_route_through_the_daemon` in `daemon_routing.rs`.
///
/// **The needles are the point.** This test used whole-token needles only for
/// most of its life, and that is exactly why it could not see the bug roadmap
/// §6.1 recorded: the index path ran an FTS5 phrase query, which matches whole
/// tokens with ASCII-only case folding, while `view::match_class` is
/// `str::contains` over a full-Unicode lowercase. Three divergent rows, all
/// covered below:
///
/// | needle | fixture | before: `--no-index` | before: index |
/// |---|---|---|---|
/// | `core` | label `area:core`, body `the corepart word` | 2 | 1 |
/// | `icode` | label `ünicode-tag` | 1 | 0 |
/// | `Ünicode` | label `ünicode-tag` | 1 | 0 |
///
/// The last row is a second axis: `tokenize='ascii'` folds ASCII only, so a
/// non-ASCII *needle* differing in case from the stored text was found by the
/// file path and missed by the index. Labels are lowercased on write, so the
/// case difference has to come from the query — `ünicode` against `ünicode-tag`
/// matched on both paths, and a fixture written that way round proves nothing.
///
/// Resolved by dropping the FTS entirely (index schema 6): search is a file scan
/// on every surface. So the assertion that an index is *present* and search
/// still reports `source: "files"` is load-bearing — it is what fails the moment
/// anyone reintroduces an index tier, before the id sets even get compared.
///
/// The `gateway` rows are the older half of this test: the CLI once matched
/// title and body only, so a label-only hit was invisible to it while
/// `ops::search` returned it, and ranking differed (two match classes vs three).
#[test]
fn search_agrees_across_the_file_and_index_paths() {
    let dir = init_repo();
    let issues = dir.path().join(".clove/issues");
    // One item per match class, so ranking is observable, plus a non-match.
    // Match class runs **counter to id order** on purpose: every item is
    // priority 2, so class is the only thing that can produce the expected
    // sequence, and a ranker that fell back to the id tiebreak alone would
    // return the exact reverse. An earlier version of this fixture had the
    // title hit on the lowest id, so sorting by id passed it.
    let rows = [
        ("AAAAAAAA", "Unrelated title", "the gateway times out", ""),
        (
            "BBBBBBBB",
            "Another title",
            "unrelated prose",
            "area:gateway",
        ),
        ("CCCCCCCC", "Payments gateway", "unrelated prose", ""),
        // Mid-token in the body: `core` lives inside `corepart`, which no FTS
        // query can reach (a prefix match `"core"*` would not help either).
        ("DDDDDDDD", "Nothing here", "the corepart word", ""),
        ("EEEEEEEE", "Plain title", "nothing at all", "area:core"),
        // Non-ASCII label, for the `icode` (mid-token) and `Ünicode`
        // (non-ASCII case-folded needle) rows.
        ("FFFFFFFF", "Plain title two", "plain body", "ünicode-tag"),
    ];
    for (suffix, title, body, label) in rows {
        let id = format!("proj-{suffix}");
        let labels = if label.is_empty() {
            String::new()
        } else {
            format!("labels:\n  - {label}\n")
        };
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: {title}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
                 {labels}---\n{body}\n"
            ),
        )
        .unwrap();
    }

    let ids_of = |v: &Value| -> Vec<String> {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    };

    // (needle, expected ids in relevance order, what it exercises)
    let cases: [(&str, Vec<&str>, &str); 6] = [
        (
            "gateway",
            vec!["proj-CCCCCCCC", "proj-BBBBBBBB", "proj-AAAAAAAA"],
            "whole token: title, then label, then body — the reverse of id order",
        ),
        (
            "core",
            vec!["proj-EEEEEEEE", "proj-DDDDDDDD"],
            "mid-token in a body (`corepart`) beside a whole-token label hit",
        ),
        (
            "icode",
            vec!["proj-FFFFFFFF"],
            "mid-token inside a non-ASCII label",
        ),
        (
            "Ünicode",
            vec!["proj-FFFFFFFF"],
            "non-ASCII needle, case-folded against a lowercase label",
        ),
        (
            "COREPART",
            vec!["proj-DDDDDDDD"],
            "ASCII needle case-folded against a body",
        ),
        (
            "\"quote injection\" OR gateway*",
            Vec::new(),
            "the needle is a literal substring, not a query language",
        ),
    ];

    // Leg 1: no index on disk at all.
    let mut want: Vec<(&str, Vec<String>)> = Vec::new();
    for (needle, expected, why) in &cases {
        let v = json_ok(clove(dir.path()).args(["search", needle, "--no-index"]));
        assert_eq!(v["_meta"]["source"], "files");
        assert_eq!(ids_of(&v), *expected, "file path, {needle:?}: {why}");
        want.push((needle, ids_of(&v)));
    }

    // Leg 2: a fresh, complete index on disk. `search` must not consult it — and
    // the index has to genuinely exist, or this leg proves nothing.
    clove(dir.path()).arg("reindex").assert().success();
    assert!(
        dir.path().join(".clove/index.db").exists(),
        "the index must exist for this leg to mean anything"
    );
    let listed = json_ok(clove(dir.path()).args(["ls", "--limit", "0"]));
    assert_eq!(
        listed["_meta"]["source"], "index",
        "the index must be live enough to answer a list, or `search` \
         falling back to files would be trivially true"
    );

    for (needle, expected) in &want {
        let v = json_ok(clove(dir.path()).args(["search", needle]));
        assert_eq!(
            v["_meta"]["source"], "files",
            "search has no index tier ({needle:?}); reintroducing one \
             reintroduces roadmap §6.1"
        );
        assert_eq!(
            &ids_of(&v),
            expected,
            "with an index present, {needle:?} must answer identically"
        );
    }
}

/// An index left over from an older clove is rebuilt, and stays rebuilt, no
/// matter which command opens it first.
///
/// The first version of the rebuild discarded the old database inside the
/// *detection* step, so whichever command opened the index first consumed the
/// signal. `clove stats` runs a telemetry open on every invocation — documented
/// in its own comment as "side-effect-free" — and used the non-rebuilding
/// variant, so a single `clove stats` replaced the index with an empty one
/// carrying the *current* version. No later open could tell it was empty rather
/// than up to date, so every subsequent query silently fell back to scanning
/// files, permanently, until someone ran `clove reindex` by hand.
///
/// The store here is deliberately larger than `STALE_REFRESH_LIMIT` so the
/// staleness path cannot paper over an empty index by refreshing it inline.
#[test]
fn a_stale_schema_index_is_rebuilt_whichever_command_opens_it_first() {
    let dir = init_repo();
    let issues = dir.path().join(".clove/issues");
    for i in 0..30u32 {
        let id = format!("proj-S{i:07}");
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: Widget {i}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n"
            ),
        )
        .unwrap();
    }
    clove(dir.path()).arg("reindex").assert().success();

    let db = dir.path().join(".clove/index.db");
    let downgrade = || {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", clove_index::SCHEMA_VERSION - 1)
            .unwrap();
    };

    // `clove stats` first: it must not consume the rebuild.
    downgrade();
    clove(dir.path()).arg("stats").assert().success();
    let v = json_ok(clove(dir.path()).args(["ls", "--limit", "0"]));
    assert_eq!(
        v["_meta"]["source"], "index",
        "the index must still answer after `stats` opened it"
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 30);

    // And directly, with a list command opening it first.
    downgrade();
    let v = json_ok(clove(dir.path()).args(["ls", "--limit", "0"]));
    assert_eq!(v["_meta"]["source"], "index");
    assert_eq!(v["data"].as_array().unwrap().len(), 30);
}

/// `--fields` returns the same keys whether or not an index exists.
///
/// The index and daemon fast paths select a lean five-column row, so any field
/// outside `{id,status,type,priority,title}` was silently absent from the
/// result — `clove ls --fields id,created` gave `[{"id": …}]` on a machine with
/// an index and both keys on one without. The answer depended on whether
/// `.clove/index.db` happened to exist.
#[test]
fn fields_outside_the_lean_row_fall_back_to_the_files() {
    let dir = init_repo();
    new_item(dir.path(), "Widget", &[]);
    clove(dir.path()).arg("reindex").assert().success();

    // Outside the lean set: must fall back and still answer in full.
    let v = json_ok(clove(dir.path()).args(["ls", "--fields", "id,created"]));
    let mut keys: Vec<&str> = v["data"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["created", "id"],
        "requested fields must be present"
    );
    assert_eq!(
        v["_meta"]["source"], "files",
        "the lean row cannot serve these"
    );

    // Inside the lean set: the index still serves it.
    let v = json_ok(clove(dir.path()).args(["ls", "--fields", "id,title"]));
    assert_eq!(
        v["_meta"]["source"], "index",
        "no need to give up the index"
    );
    let mut keys: Vec<&str> = v["data"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["id", "title"]);

    // `ready` shares the tiering and the same guard.
    let v = json_ok(clove(dir.path()).args(["ready", "--fields", "id,updated"]));
    assert!(
        v["data"][0].get("updated").is_some(),
        "ready must honour it too: {v}"
    );
}

/// `--compact` produces the same key set as the MCP tools' `compact`.
///
/// It did not: the MCP path additionally drops `schema`, so the two surfaces
/// disagreed by exactly one silent key while the changelog claimed they shaped
/// identically.
#[test]
fn compact_drops_the_same_keys_as_the_mcp_tools() {
    let dir = init_repo();
    new_item(dir.path(), "Widget", &[]);
    let v = json_ok(clove(dir.path()).args(["ls", "--no-index", "--compact"]));
    let obj = v["data"][0].as_object().unwrap();
    assert!(obj.get("schema").is_none(), "schema is dropped: {v}");
    for absent in ["assignee", "parent", "closed", "labels", "deps"] {
        assert!(obj.get(absent).is_none(), "`{absent}` should be compacted");
    }
    // Without --compact the full shape, `schema` included, is unchanged.
    let full = json_ok(clove(dir.path()).args(["ls", "--no-index"]));
    assert_eq!(full["data"][0]["schema"], 1);
}

#[test]
fn ls_default_limit_caps_at_100_with_full_total() {
    let dir = init_repo();
    let issues = dir.path().join(".clove/issues");
    for i in 0..120u32 {
        let id = format!("proj-{i:08}");
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: Item {i}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    // Default: capped at 100, but _meta.total reports all 120 (file path).
    let v = json_ok(clove(dir.path()).arg("ls"));
    assert_eq!(v["_meta"]["source"], "files");
    assert_eq!(v["_meta"]["returned"], 100);
    assert_eq!(v["_meta"]["total"], 120);

    // --limit 0 returns everything.
    let all = json_ok(clove(dir.path()).args(["ls", "--limit", "0"]));
    assert_eq!(all["_meta"]["returned"], 120);

    // Same caps via the index path, with an accurate total.
    clove(dir.path()).arg("reindex").assert().success();
    let idx = json_ok(clove(dir.path()).arg("ls"));
    assert_eq!(idx["_meta"]["source"], "index");
    assert_eq!(idx["_meta"]["returned"], 100);
    assert_eq!(idx["_meta"]["total"], 120);
}

#[test]
fn ls_deep_flag_still_uses_index() {
    let dir = init_repo();
    new_item(dir.path(), "One", &[]);
    clove(dir.path()).arg("reindex").assert().success();
    // --deep selects the thorough staleness check but still serves from the index.
    let v = json_ok(clove(dir.path()).args(["ls", "--deep"]));
    assert_eq!(v["_meta"]["source"], "index");
}

#[test]
fn ls_index_auto_refreshes_after_edit() {
    let dir = init_repo();
    new_item(dir.path(), "One", &[]);
    clove(dir.path()).arg("reindex").assert().success();

    // Add an item after indexing; the index auto-refreshes (<= threshold).
    new_item(dir.path(), "Two", &[]);
    let v = json_ok(clove(dir.path()).arg("ls"));
    assert_eq!(v["_meta"]["source"], "index");
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
}

#[test]
fn doctor_detects_and_fixes_index_divergence() {
    let dir = init_repo();
    new_item(dir.path(), "Indexed", &[]);
    clove(dir.path()).arg("reindex").assert().success();

    // Diverge the index by deleting the item file directly.
    for entry in std::fs::read_dir(dir.path().join(".clove/issues")).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("md") {
            std::fs::remove_file(p).unwrap();
        }
    }

    let report = json_ok(clove(dir.path()).arg("doctor"));
    let codes: Vec<&str> = report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"INDEX_DIVERGENCE"), "codes: {codes:?}");

    // --fix rebuilds the index; a subsequent run is clean of divergence.
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let after = json_ok(clove(dir.path()).arg("doctor"));
    let after_codes: Vec<&str> = after["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(
        !after_codes.contains(&"INDEX_DIVERGENCE"),
        "codes: {after_codes:?}"
    );
}

#[test]
fn doctor_no_index_skips_divergence_check() {
    let dir = init_repo();
    new_item(dir.path(), "X", &[]);
    clove(dir.path()).arg("reindex").assert().success();
    std::fs::remove_dir_all(dir.path().join(".clove/issues")).unwrap();
    std::fs::create_dir_all(dir.path().join(".clove/issues")).unwrap();

    // With --no-index the divergence check does not run.
    let report = json_ok(clove(dir.path()).args(["doctor", "--no-index"]));
    let codes: Vec<&str> = report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(!codes.contains(&"INDEX_DIVERGENCE"));
}

/// Collect the `code` of every issue in a doctor JSON report.
fn doctor_codes(report: &Value) -> Vec<String> {
    report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn doctor_detects_and_fixes_gitignore_drift() {
    let dir = init_repo();
    new_item(dir.path(), "X", &[]);
    let gitignore = dir.path().join(".clove/.gitignore");

    // A missing required entry (the file exists but is incomplete).
    std::fs::write(&gitignore, "index.db\n").unwrap();
    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(
        doctor_codes(&report).contains(&"GITIGNORE_DRIFT".to_owned()),
        "codes: {:?}",
        doctor_codes(&report)
    );

    // --fix appends the missing canonical entries (keeping the existing line).
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let fixed = std::fs::read_to_string(&gitignore).unwrap();
    for entry in ["index.db", "daemon.sock", "index.db.tmp"] {
        assert!(fixed.contains(entry), "missing {entry} after fix");
    }
    let after = json_ok(clove(dir.path()).arg("doctor"));
    assert!(!doctor_codes(&after).contains(&"GITIGNORE_DRIFT".to_owned()));

    // A wholly absent file is also detected and recreated.
    std::fs::remove_file(&gitignore).unwrap();
    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(doctor_codes(&report).contains(&"GITIGNORE_DRIFT".to_owned()));
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(gitignore.exists());
}

#[test]
fn doctor_detects_timestamp_incoherence() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Backwards", &[]);

    // Hand-edit the file so `updated` precedes `created`.
    let item_path = dir.path().join(format!(".clove/issues/{id}.md"));
    let rewritten = std::fs::read_to_string(&item_path)
        .unwrap()
        .lines()
        .map(|l| {
            if l.starts_with("created:") {
                "created: 2026-06-05T10:00:00Z".to_owned()
            } else if l.starts_with("updated:") {
                "updated: 2026-06-01T10:00:00Z".to_owned()
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&item_path, format!("{rewritten}\n")).unwrap();

    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(
        doctor_codes(&report).contains(&"TIMESTAMP_INCOHERENT".to_owned()),
        "codes: {:?}",
        doctor_codes(&report)
    );
    // It is a warning, not an error: a plain run still exits 0.
    clove(dir.path()).arg("doctor").assert().success();
}

#[test]
fn doctor_detects_and_fixes_index_corruption() {
    let dir = init_repo();
    new_item(dir.path(), "Indexed", &[]);
    clove(dir.path()).arg("reindex").assert().success();

    // Overwrite the index with bytes that are not a SQLite database.
    let db = dir.path().join(".clove/index.db");
    std::fs::write(&db, b"this is definitely not sqlite").unwrap();

    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(
        doctor_codes(&report).contains(&"INDEX_CORRUPT".to_owned()),
        "codes: {:?}",
        doctor_codes(&report)
    );
    // It is an error: --strict exits 4.
    clove(dir.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .code(4);

    // --fix rebuilds the index from the files; a subsequent run is clean.
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let after = json_ok(clove(dir.path()).arg("doctor"));
    assert!(!doctor_codes(&after).contains(&"INDEX_CORRUPT".to_owned()));
}

#[test]
fn doctor_detects_and_fixes_index_schema_mismatch() {
    let dir = init_repo();
    new_item(dir.path(), "Indexed", &[]);
    clove(dir.path()).arg("reindex").assert().success();

    // Stamp an older schema version onto the (otherwise valid) index.
    let db = dir.path().join(".clove/index.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", 1_i64).unwrap();
    }

    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(
        doctor_codes(&report).contains(&"INDEX_SCHEMA_MISMATCH".to_owned()),
        "codes: {:?}",
        doctor_codes(&report)
    );

    // --fix rebuilds at the current schema version; clean afterwards.
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let after = json_ok(clove(dir.path()).arg("doctor"));
    assert!(!doctor_codes(&after).contains(&"INDEX_SCHEMA_MISMATCH".to_owned()));
}

#[test]
fn env_clove_format_json_without_flag() {
    let dir = init_repo();
    new_item(dir.path(), "Item", &[]);
    let out = clove(dir.path())
        .env("CLOVE_FORMAT", "json")
        .arg("ls")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
}

/// `clove blocked` answers from the index, and answers *identically*.
///
/// Until read-path §4 it was the one list with no index tier at all: with a hot
/// `.clove/index.db` and no daemon it still scanned and parsed every file in the
/// store. `clove_index` now has a `Blocked` query — the exact complement of the
/// `ready` clause within the active, non-excluded set — so the SQL and the
/// in-memory `GraphStore` partition the store the same way.
///
/// The comparison is against `--no-index`, and the `source` assertions are what
/// keep it from being vacuous: without them this would pass with both runs on
/// the file path.
#[test]
fn blocked_answers_from_the_index_and_agrees_with_the_files() {
    let dir = init_repo();
    let a = new_item(dir.path(), "alpha", &[]);
    let b = new_item(dir.path(), "beta", &[]);
    let c = new_item(dir.path(), "gamma", &[]);
    // a is blocked by the open b; c is blocked by a missing id.
    clove(dir.path())
        .args(["dep", "add", &a, &b])
        .assert()
        .success();
    clove(dir.path())
        .args(["dep", "add", &c, &b])
        .assert()
        .success();
    clove(dir.path()).arg("reindex").assert().success();

    let files = json_ok(clove(dir.path()).args(["--no-index", "blocked"]));
    assert_eq!(files["_meta"]["source"], "files");
    let indexed = json_ok(clove(dir.path()).args(["blocked"]));
    assert_eq!(
        indexed["_meta"]["source"], "index",
        "blocked must have an index tier, or the comparison below is vacuous"
    );

    assert_eq!(indexed["data"], files["data"], "row for row, tier for tier");
    assert_eq!(indexed["_meta"]["total"], files["_meta"]["total"]);
    // `blocked_by` is the point of the list and no lean row carries it, so the
    // index tier has to hydrate the page rather than serve the projection.
    assert_eq!(
        indexed["data"][0]["blocked_by"],
        serde_json::json!([b]),
        "the index tier still reports what blocks each item: {indexed}"
    );
    // A hydrated row is a *full* row: the lean five columns would not have this.
    assert!(
        indexed["data"][0].get("created").is_some(),
        "blocked rows are full items, not the lean projection: {indexed}"
    );
}

/// `clove ready` warns about the items it silently left out.
///
/// An item whose `deps` name a missing id is excluded from `ready` — correctly,
/// since nothing can say the dependency is done — and the only signal a user
/// gets is this warning: `_meta.warnings` in JSON, stderr in human format. It is
/// also the one warning the accelerator tiers cannot produce, because the SQL
/// `ready` query never builds the dangling set, so the message must survive the
/// hand-off from `clove_core::ops::ready_rows` all the way to `_meta`.
///
/// The `--no-index` run is what makes the check meaningful: the file tier is the
/// only one that has the warning to give.
#[test]
fn ready_warns_about_items_excluded_for_dangling_deps() {
    let dir = init_repo();
    let ok = new_item(dir.path(), "unblocked", &[]);
    let broken = new_item(dir.path(), "names a ghost", &[]);
    // Hand-write a reference to an id that does not exist (`dep add` refuses).
    let path = dir.path().join(format!(".clove/issues/{broken}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("deps: []", "deps: [proj-ZZZZZZZZ]")).unwrap();

    let v = json_ok(clove(dir.path()).args(["--no-index", "ready"]));
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![ok.as_str()], "the dangling item is not ready");

    let warnings = v["_meta"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "exactly one warning: {v}");
    let msg = warnings[0].as_str().unwrap();
    assert!(
        msg.contains("dangling") && msg.contains(&broken),
        "the warning must name the excluded item: {msg}"
    );

    // Human format puts it on stderr, so an interactive run cannot miss it —
    // and `--quiet` is what turns it off.
    let out = clove(dir.path())
        .args(["--no-index", "ready"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dangling"), "stderr: {stderr}");
    let quiet = clove(dir.path())
        .args(["--no-index", "--quiet", "ready"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("dangling"),
        "--quiet silences it"
    );

    // It is not lost: `blocked` is where the item went, with the broken id named.
    let blocked = json_ok(clove(dir.path()).args(["--no-index", "blocked"]));
    assert_eq!(blocked["data"][0]["id"], serde_json::json!(broken));
    assert_eq!(
        blocked["data"][0]["blocked_by"],
        serde_json::json!(["proj-ZZZZZZZZ"])
    );
}
