//! M4: `clove stats` end-to-end tests — analytics shape, schema, persistence.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use jsonschema::Validator;
use serde_json::Value;
use tempfile::TempDir;

fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

fn schema(name: &str) -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/json-schema/v1")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    jsonschema::validator_for(&value).expect("valid schema")
}

fn json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

fn init_with_items() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    clove(dir.path())
        .args([
            "new",
            "First",
            "--type",
            "feature",
            "-p",
            "1",
            "-a",
            "alice",
            "-l",
            "area:core",
        ])
        .assert()
        .success();
    clove(dir.path())
        .args([
            "new",
            "Second",
            "--type",
            "bug",
            "-a",
            "alice",
            "-l",
            "area:core",
        ])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "Third", "--type", "docs"])
        .assert()
        .success();

    // First blocks Second (Second depends on First, which is open).
    let ids = item_ids(dir.path());
    clove(dir.path())
        .args(["dep", "add", &ids[1], &ids[0]])
        .assert()
        .success();
    dir
}

fn item_ids(dir: &Path) -> Vec<String> {
    let v = json(clove(dir).args(["ls", "--format", "json", "--limit", "0"]));
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn stats_json_validates_against_schema() {
    let dir = init_with_items();
    let stats = schema("stats.json");

    let v = json(clove(dir.path()).args(["stats", "--format", "json"]));
    if let Err(e) = stats.validate(&v) {
        panic!("stats schema violation: {e}");
    }

    let data = &v["data"];
    assert_eq!(data["total"], 3);
    assert_eq!(data["by_status"]["open"], 3);
    assert_eq!(data["by_type"]["bug"], 1);
    assert_eq!(data["by_type"]["feature"], 1);
    assert_eq!(data["by_type"]["docs"], 1);
    // One open dep (First) blocks Second; First and Third are ready.
    assert_eq!(data["ready"], 2, "{data}");
    assert_eq!(data["blocked"], 1, "{data}");
    assert_eq!(data["unassigned"], 1);
    assert_eq!(data["daemon"]["running"], false);
}

#[test]
fn stats_on_empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    let v = json(clove(dir.path()).args(["stats", "--format", "json"]));
    assert_eq!(v["data"]["total"], 0);
    assert_eq!(v["data"]["ready"], 0);
    assert!(v["data"]["epics"].as_array().unwrap().is_empty());
}

#[test]
fn snapshot_persists_and_history_reads_back() {
    let dir = init_with_items();

    // No history yet.
    let empty = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    assert_eq!(empty["data"].as_array().unwrap().len(), 0);

    // Record a snapshot; history now lives in the index database.
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();
    assert!(dir.path().join(".clove/index.db").exists());

    let hist = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    let rows = hist["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["stats"]["total"], 3);
    assert!(rows[0]["captured_at"].is_string());

    // A second snapshot accumulates.
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();
    let hist2 = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    assert_eq!(hist2["data"].as_array().unwrap().len(), 2);

    // --limit caps the series, and `_meta` reports the window honestly: `total`
    // is the count *before* the cap, so a truncated series is distinguishable
    // from an exhausted one. It used to report the post-window count (`1` here),
    // leaving a client no way to tell.
    let limited =
        json(clove(dir.path()).args(["stats", "--history", "--limit", "1", "--format", "json"]));
    assert_eq!(limited["data"].as_array().unwrap().len(), 1);
    assert_eq!(limited["_meta"]["total"], 2, "total is pre-window");
    assert_eq!(limited["_meta"]["returned"], 1);
    assert_eq!(limited["_meta"]["limit"], 1);

    // `--offset` pages into the series, and `--limit 0` is unlimited.
    let skipped =
        json(clove(dir.path()).args(["stats", "--history", "--offset", "1", "--format", "json"]));
    assert_eq!(skipped["data"].as_array().unwrap().len(), 1);
    assert_eq!(skipped["_meta"]["total"], 2);
    let all =
        json(clove(dir.path()).args(["stats", "--history", "--limit", "0", "--format", "json"]));
    assert_eq!(all["data"].as_array().unwrap().len(), 2);
    assert_eq!(all["_meta"]["limit"], 0);
}

#[test]
fn history_since_filters_by_timestamp() {
    let dir = init_with_items();
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();

    // A far-future `--since` excludes the just-recorded snapshot.
    let future = json(clove(dir.path()).args([
        "stats",
        "--history",
        "--since",
        "2999-01-01T00:00:00+00:00",
        "--format",
        "json",
    ]));
    assert_eq!(future["data"].as_array().unwrap().len(), 0);

    // A past `--since` includes it.
    let past = json(clove(dir.path()).args([
        "stats",
        "--history",
        "--since",
        "2000-01-01T00:00:00+00:00",
        "--format",
        "json",
    ]));
    assert_eq!(past["data"].as_array().unwrap().len(), 1);
}

#[test]
fn top_caps_breakdowns() {
    let dir = init_with_items();
    // Both labeled items share area:core; cap doesn't drop it, but `--top 1`
    // limits the assignee list to one row.
    let v = json(clove(dir.path()).args(["stats", "--top", "1", "--format", "json"]));
    assert!(v["data"]["by_assignee"].as_array().unwrap().len() <= 1);
}

// ---------------------------------------------------------------------------
// Canonical timestamps (READ_PATH_ROADMAP §3)
// ---------------------------------------------------------------------------

/// Whether `s` is in clove's one canonical spelling: RFC 3339, UTC, whole
/// seconds, `Z`.
fn is_canonical(s: &str) -> bool {
    s.len() == 20
        && s.ends_with('Z')
        && !s.contains('.')
        && s.parse::<chrono::DateTime<chrono::Utc>>().is_ok()
}

#[test]
fn snapshot_timestamps_are_canonical_end_to_end() {
    let dir = init_with_items();
    let taken = json(clove(dir.path()).args(["stats", "--snapshot", "--format", "json"]));
    let generated_at = taken["_meta"]["generated_at"].as_str().unwrap();
    assert!(
        is_canonical(generated_at),
        "_meta.generated_at `{generated_at}` is not canonical"
    );

    let hist = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    let captured_at = hist["data"][0]["captured_at"].as_str().unwrap();
    assert!(
        is_canonical(captured_at),
        "captured_at `{captured_at}` is not canonical"
    );
}

#[test]
fn history_orders_by_instant_across_stored_spellings() {
    // `--history` sorts on `captured_at` as a string, and the snapshots table is
    // durable — it is carried verbatim across every reindex and index-schema
    // rebuild, so rows an older clove wrote in a different spelling outlive any
    // bump and sit next to canonical ones. Rows are written straight into the
    // database here for exactly that reason: routing them through `--snapshot`
    // would canonicalize them and the fixture would prove nothing.
    let dir = init_with_items();
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();

    // Reuse the real report blob the snapshot just wrote, so these rows differ
    // from a genuine one only in the spelling of `captured_at`.
    let db = dir.path().join(".clove").join("index.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let detail: String = conn
        .query_row("SELECT detail_json FROM snapshots", [], |r| r.get(0))
        .unwrap();
    conn.execute("DELETE FROM snapshots", []).unwrap();
    for captured_at in [
        "2026-06-01T10:00:02Z",                // canonical
        "2026-06-01T10:00:00.904816670+00:00", // as an older clove wrote it
        "2026-06-01T10:00:01+00:00",
    ] {
        conn.execute(
            "INSERT INTO snapshots (captured_at, total, open, in_progress, closed, ready, \
             blocked, dangling, cycles, detail_json) VALUES (?1, 0, 0, 0, 0, 0, 0, 0, 0, ?2)",
            rusqlite::params![captured_at, detail],
        )
        .unwrap();
    }
    drop(conn);

    let hist = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    let series: Vec<String> = hist["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["captured_at"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        series,
        vec![
            "2026-06-01T10:00:02Z",
            "2026-06-01T10:00:01Z",
            "2026-06-01T10:00:00Z"
        ],
        "newest first by instant, every row canonical"
    );

    // A `--since` bound at the boundary second is inclusive whatever spelling
    // either side of the comparison uses.
    for bound in ["2026-06-01T10:00:01Z", "2026-06-01T10:00:01+00:00"] {
        let since = json(clove(dir.path()).args([
            "stats",
            "--history",
            "--since",
            bound,
            "--format",
            "json",
        ]));
        assert_eq!(
            since["data"].as_array().unwrap().len(),
            2,
            "`{bound}` must include the boundary row"
        );
    }
}

/// `--since`/`--limit`/`--offset` are the `--history` window, and passing one
/// without `--history` is a usage error rather than a flag that does nothing
/// (read-path roadmap §7).
///
/// A live report is a single object: there is no series to filter or page, so
/// these three had nothing to apply and were silently dropped — `clove stats
/// --limit 5` exited 0 with the full report, indistinguishable from a build that
/// does not support the flag. Their help text already said "With `--history`:";
/// clap now enforces it and names the missing flag.
#[test]
fn the_history_window_flags_require_history() {
    let dir = init_with_items();

    for args in [
        vec!["stats", "--limit", "5"],
        vec!["stats", "--offset", "1"],
        vec!["stats", "--since", "2000-01-01T00:00:00+00:00"],
        vec!["stats", "--limit", "5", "--format", "json"],
    ] {
        let out = clove(dir.path()).args(&args).output().unwrap();
        assert!(
            !out.status.success(),
            "{args:?} silently ignored the flag: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--history"),
            "{args:?}: the error must name the flag that is missing: {stderr}"
        );
        // The report must not be printed alongside the error.
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "{args:?}: printed a report anyway"
        );
    }

    // With `--history` they are accepted, and a live report still needs none of
    // them — the rejection is about the *combination*, not the flags.
    clove(dir.path())
        .args(["stats", "--history", "--limit", "5", "--offset", "0"])
        .assert()
        .success();
    clove(dir.path()).args(["stats"]).assert().success();
    clove(dir.path())
        .args(["stats", "--top", "3", "--no-epics"])
        .assert()
        .success();
}
