//! End-to-end tests for `clove mcp`: drive the real binary's MCP stdio server
//! with newline-delimited JSON-RPC and assert the handshake, tool listing, and a
//! create→read round-trip over the direct-core fallback path (no daemon).
//!
//! The server handles requests concurrently (per the MCP spec), so the test
//! talks to it **sequentially** — one request, await its reply, then the next —
//! exactly as a real client would when a later call depends on an earlier write.
#![cfg(feature = "mcp")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;

fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// A live MCP stdio conversation with `clove mcp`.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// A `clove mcp` command with the daemon auto-start opted out (the default for
/// tests that exercise the direct-core fallback path — hermetic, no spawned daemon).
fn fallback_cmd(dir: &Path) -> Command {
    let mut cmd = clove(dir);
    cmd.env("CLOVE_MCP_NO_DAEMON", "1");
    cmd
}

impl Session {
    /// Spawn the server (fallback / no-daemon) and complete the handshake.
    fn start(dir: &Path) -> Session {
        Session::start_cmd(fallback_cmd(dir))
    }

    /// Spawn the server from a pre-configured command and complete the
    /// `initialize` / `initialized` handshake.
    fn start_cmd(mut cmd: Command) -> Session {
        let mut child = cmd
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn clove mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut s = Session {
            child,
            stdin,
            stdout,
        };

        let init = s.request(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.0" }
            }
        }));
        assert_eq!(init["result"]["serverInfo"]["name"], "clove");
        assert!(init["result"]["protocolVersion"].is_string());
        // The server advertises the resources capability with subscribe + listChanged
        // (gh-21: it pushes resources/updated when the work graph changes).
        let caps = &init["result"]["capabilities"];
        assert_eq!(caps["resources"]["subscribe"], true, "caps: {caps}");
        assert_eq!(caps["resources"]["listChanged"], true, "caps: {caps}");
        s.notify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        s
    }

    /// Send a request and read exactly one response line.
    fn request(&mut self, req: Value) -> Value {
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "server closed before replying to {req}");
        serde_json::from_str(&line).expect("response is valid JSON")
    }

    /// Send a notification (no reply expected).
    fn notify(&mut self, note: Value) {
        writeln!(self.stdin, "{note}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Call a tool and return its `result`.
    fn call(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let resp = self.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }));
        resp["result"].clone()
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

#[test]
fn handshake_and_tools_list() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    let resp = s.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = resp["result"]["tools"].as_array().unwrap();
    // The wire contract, maintained by hand on purpose: this is what a client
    // sees, so a tool appearing or vanishing should be a deliberate edit here.
    const EXPECTED: &[&str] = &[
        "clove_ready",
        "clove_blocked",
        "clove_list",
        "clove_show",
        "clove_search",
        "clove_comments",
        "clove_dep_tree",
        "clove_stats",
        "clove_new",
        "clove_status",
        "clove_edit",
        "clove_comment",
        "clove_dep_add",
        "clove_dep_remove",
        "clove_set_parent",
    ];
    assert_eq!(
        tools.len(),
        EXPECTED.len(),
        "advertised tool count drifted from the expected set"
    );
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in EXPECTED {
        assert!(names.contains(expected), "missing tool {expected}");
    }
    // Each tool publishes an input schema object.
    let ready = tools.iter().find(|t| t["name"] == "clove_ready").unwrap();
    assert!(ready["inputSchema"]["properties"].is_object());

    s.shutdown();
}

#[test]
fn create_then_read_round_trip() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    // clove_new returns the created id; not an error; the file lands on disk.
    let created = s.call(
        2,
        "clove_new",
        json!({ "title": "wire up MCP", "priority": 1 }),
    );
    assert_eq!(created["isError"], false);
    let id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("proj-"), "got id {id}");
    assert!(dir
        .path()
        .join(".clove/issues")
        .join(format!("{id}.md"))
        .exists());

    // clove_ready (no daemon → direct-core fallback) now lists the new item.
    let ready = s.call(3, "clove_ready", json!({}));
    assert_eq!(ready["isError"], false);
    let items = ready["structuredContent"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id);
    assert_eq!(items[0]["title"], "wire up MCP");

    // A mutation round-trip: close it, then it is no longer ready.
    let closed = s.call(4, "clove_status", json!({ "id": id, "status": "closed" }));
    assert_eq!(closed["structuredContent"]["status"], "closed");
    let ready2 = s.call(5, "clove_ready", json!({}));
    assert_eq!(ready2["structuredContent"]["total"], 0);

    // clove_stats sees one (closed) item.
    let stats = s.call(6, "clove_stats", json!({}));
    assert_eq!(stats["structuredContent"]["total"], 1);
    assert_eq!(stats["structuredContent"]["by_status"]["closed"], 1);

    s.shutdown();
}

#[test]
fn edit_body_dep_remove_and_set_parent_round_trip() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    let main = s.call(2, "clove_new", json!({ "title": "main" }));
    let main_id = main["structuredContent"]["id"].as_str().unwrap().to_owned();
    let dep = s.call(3, "clove_new", json!({ "title": "dep" }));
    let dep_id = dep["structuredContent"]["id"].as_str().unwrap().to_owned();

    // clove_edit now carries a body (the new capability) alongside scalar fields.
    let edited = s.call(
        4,
        "clove_edit",
        json!({ "id": main_id, "title": "renamed", "body": "a fresh body" }),
    );
    assert_eq!(edited["isError"], false);
    assert_eq!(edited["structuredContent"]["title"], "renamed");
    let shown = s.call(5, "clove_show", json!({ "id": main_id }));
    assert_eq!(shown["structuredContent"]["body"], "a fresh body\n");

    // dep_add then the new clove_dep_remove.
    let added = s.call(
        6,
        "clove_dep_add",
        json!({ "id": main_id, "dep_id": dep_id }),
    );
    assert_eq!(added["structuredContent"]["deps"], json!([dep_id]));
    let removed = s.call(
        7,
        "clove_dep_remove",
        json!({ "id": main_id, "dep_id": dep_id }),
    );
    assert_eq!(removed["isError"], false);
    assert_eq!(removed["structuredContent"]["deps"], json!([]));

    // The new clove_set_parent: set, then clear (omit `parent`).
    let parented = s.call(
        8,
        "clove_set_parent",
        json!({ "id": main_id, "parent": dep_id }),
    );
    assert_eq!(parented["structuredContent"]["parent"], dep_id);
    let cleared = s.call(9, "clove_set_parent", json!({ "id": main_id }));
    assert!(cleared["structuredContent"]["parent"].is_null());

    s.shutdown();
}

/// The plugin spawns `clove mcp` per session, possibly before `clove init`. The
/// server must still **start and complete the handshake** in a directory with no
/// `.clove/` repository (rather than the process failing to launch), and its
/// tools surface a "no repository" error until the repo exists.
#[test]
fn starts_without_a_repository() {
    let dir = tempfile::tempdir().unwrap(); // NOT init'd — no .clove/
    let mut s = Session::start(dir.path());

    // Handshake already succeeded inside Session::start; tools are still listed.
    let resp = s.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 15);

    // A read tool returns a tool error (not a protocol error / crash) because
    // there is no repository yet.
    let ready = s.call(3, "clove_ready", json!({}));
    assert_eq!(ready["isError"], true, "no repo → tool reports an error");

    // No stray `.clove/` was materialized just by starting the server.
    assert!(
        !dir.path().join(".clove").exists(),
        "starting the server must not create a repository"
    );

    s.shutdown();
}

/// With the daemon **enabled** (the default) but no `.clove/` repository, the
/// server must NOT auto-start `cloved` or materialize a `.clove/` — the
/// `clove_dir.exists()` guard skips coordination when there is nothing to
/// coordinate. (The `no_daemon` variant above can't cover this: it disables the
/// daemon outright, so it would pass even if the guard were removed.)
#[cfg(unix)]
#[test]
fn no_repo_does_not_spawn_daemon_or_create_clove_dir() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap(); // NOT init'd — no .clove/

    // Build `cloved` and point `CLOVED_PATH` at it, so that if the guard were
    // broken the server WOULD find a daemon to spawn (and create `.clove/`).
    let cloved = escargot::CargoBuild::new()
        .package("cloved")
        .bin("cloved")
        .run()
        .expect("build cloved");

    let mut cmd = clove(dir.path()); // daemon enabled (no CLOVE_MCP_NO_DAEMON)
    cmd.env("CLOVED_PATH", cloved.path())
        .env("CLOVED_DISABLE_WEB", "1")
        .env("CLOVE_MCP_HEARTBEAT_MS", "100");
    let s = Session::start_cmd(cmd); // handshake still succeeds

    // Give a broken guard time to spawn cloved / create the dir, then assert it did not.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(600) {
        assert!(
            !dir.path().join(".clove").exists(),
            "no repo → the server must not create a .clove/ directory"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    s.shutdown();
}

#[test]
fn resources_are_listed_and_readable() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    // resources/list advertises the two live resources.
    let list = s.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }));
    let resources = list["result"]["resources"]
        .as_array()
        .expect("resources array");
    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"clove://ready"), "resources: {resources:?}");
    assert!(uris.contains(&"clove://stats"), "resources: {resources:?}");

    // Create an item, then read clove://ready — its JSON reflects the new item and
    // is byte-identical to what the clove_ready tool returns.
    let created = s.call(3, "clove_new", json!({ "title": "resource read me" }));
    let id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let read = s.request(json!({
        "jsonrpc": "2.0", "id": 4, "method": "resources/read",
        "params": { "uri": "clove://ready" }
    }));
    let contents = read["result"]["contents"]
        .as_array()
        .expect("contents array");
    assert_eq!(contents[0]["mimeType"], "application/json");
    let text = contents[0]["text"].as_str().expect("text contents");
    // Valid JSON that mentions the just-created ready item.
    let _: Value = serde_json::from_str(text).expect("resource text is JSON");
    assert!(
        text.contains(&id),
        "clove://ready should include the new item {id}: {text}"
    );

    // An unknown resource is a protocol-level error.
    let bad = s.request(json!({
        "jsonrpc": "2.0", "id": 5, "method": "resources/read",
        "params": { "uri": "clove://nope" }
    }));
    assert!(bad.get("error").is_some(), "unknown uri → error: {bad}");

    s.shutdown();
}

/// An agent can read back what it wrote. Before `clove_comments`, `clove_comment`
/// was write-only over MCP — `clove_show` reported a `comment_count` and nothing
/// else, so findings an agent recorded were unreachable in a later session.
#[test]
fn comments_round_trip_through_mcp() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    let created = s.call(2, "clove_new", json!({ "title": "investigate" }));
    let id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for note in ["first finding", "second finding", "third finding"] {
        let r = s.call(3, "clove_comment", json!({ "id": id, "message": note }));
        assert_ne!(r["isError"], true, "comment failed: {r}");
    }

    let all = s.call(4, "clove_comments", json!({ "id": id }));
    let page = &all["structuredContent"];
    assert_eq!(page["total"], 3);
    assert_eq!(page["returned"], 3);
    let bodies: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        vec!["first finding", "second finding", "third finding"],
        "comments come back oldest-first"
    );
    // Authorship is attributed, but lossily: comments carry the author only in
    // their file name, so it is stored as a filename-safe slug
    // (`tester@example.com` -> `tester-example-com`) and the original address is
    // not recoverable. Pinned here so the MCP surface's fidelity is explicit.
    assert_eq!(page["items"][0]["author"], "tester-example-com");

    // `limit` keeps the most recent, matching `clove comments --limit`.
    let last = s.call(5, "clove_comments", json!({ "id": id, "limit": 1 }));
    assert_eq!(
        last["structuredContent"]["items"][0]["body"],
        "third finding"
    );
    assert_eq!(
        last["structuredContent"]["total"], 3,
        "total is the unpaginated count"
    );

    // A missing item is a tool error, not an empty page.
    let missing = s.call(6, "clove_comments", json!({ "id": "proj-ZZZZZZZZ" }));
    assert_eq!(missing["isError"], true);

    s.shutdown();
}

/// Every list-shaped read tool honours the *same* `offset`/`limit` contract:
/// absent → the surface default, `0` → unlimited, `n` → at most `n`; `total` is
/// always the pre-pagination match count and `limit` is echoed back.
///
/// Before the shared `Page` existed only `clove_list` had an `offset` at all —
/// `clove_ready` / `clove_blocked` / `clove_search` hard-coded it to zero, so an
/// agent that hit the limit had no way to reach the rest of the matches.
#[test]
fn every_list_tool_pages_the_same_way() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    // One blocker plus five items that depend on it: `blocked` sees the five,
    // `ready` sees the blocker alone once the five are wired up.
    let blocker = s.call(2, "clove_new", json!({ "title": "page blocker" }));
    let blocker = blocker["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for n in 0..5 {
        let r = s.call(
            3,
            "clove_new",
            json!({ "title": format!("page item {n}"), "deps": [blocker] }),
        );
        assert_ne!(r["isError"], true, "create failed: {r}");
    }

    // Ids in the tool's own order, so the paging assertions below compare
    // against ground truth rather than a guess about the sort.
    let ids = |s: &mut Session, tool: &str, args: Value| -> Vec<String> {
        let r = s.call(9, tool, args);
        assert_ne!(r["isError"], true, "{tool} failed: {r}");
        r["structuredContent"]["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    };

    for (tool, args, expected_total) in [
        ("clove_blocked", json!({}), 5),
        ("clove_list", json!({}), 6),
        ("clove_search", json!({ "text": "page" }), 6),
    ] {
        let with = |extra: Value| -> Value {
            let mut merged = args.as_object().unwrap().clone();
            for (k, v) in extra.as_object().unwrap() {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        };

        // The unpaginated order is the reference for every window below.
        let all = ids(&mut s, tool, with(json!({ "limit": 0 })));
        assert_eq!(all.len(), expected_total, "{tool}: limit 0 is unlimited");

        let page = s.call(9, tool, with(json!({ "offset": 2, "limit": 2 })));
        let meta = &page["structuredContent"];
        assert_eq!(meta["total"], expected_total, "{tool}: total is pre-window");
        assert_eq!(meta["returned"], 2, "{tool}: returned is post-window");
        assert_eq!(meta["offset"], 2, "{tool}: offset is echoed");
        assert_eq!(meta["limit"], 2, "{tool}: limit is echoed");
        let windowed: Vec<String> = meta["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            windowed,
            all[2..4],
            "{tool}: offset skips, it does not clamp"
        );

        // Walking past the end is an empty page, not an error or a wrap-around.
        let past = s.call(9, tool, with(json!({ "offset": 99 })));
        assert_eq!(past["structuredContent"]["returned"], 0, "{tool}: past end");
        assert_eq!(
            past["structuredContent"]["total"], expected_total,
            "{tool}: total survives an out-of-range offset"
        );
    }

    // `clove_ready` shares the contract even though only the blocker is ready.
    let ready = s.call(9, "clove_ready", json!({ "offset": 1 }));
    assert_eq!(ready["structuredContent"]["total"], 1);
    assert_eq!(ready["structuredContent"]["returned"], 0);
    assert_eq!(ids(&mut s, "clove_ready", json!({})), vec![blocker]);

    // The default is the MCP default, echoed back rather than left to folklore.
    assert_eq!(
        s.call(9, "clove_list", json!({}))["structuredContent"]["limit"],
        50
    );

    s.shutdown();
}

/// Read results are compacted by default and can be projected to a field
/// subset. Both cut what an agent has to read; neither changes what the CLI,
/// the web API, or `export json` produce.
#[test]
fn read_results_are_compact_and_projectable() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());
    s.call(2, "clove_new", json!({ "title": "shape me" }));

    // Default: keys that are null or empty on a plain item are gone, but a
    // definite `false` is not.
    let default = s.call(3, "clove_list", json!({}));
    let row = &default["structuredContent"]["items"][0];
    for absent in [
        "assignee",
        "parent",
        "closed",
        "labels",
        "deps",
        "relates",
        "duplicates",
        "supersedes",
        "source_system",
        "external_ref",
        "schema",
    ] {
        assert!(row.get(absent).is_none(), "`{absent}` should be compacted");
    }
    assert!(row.get("id").is_some());
    assert!(row.get("title").is_some());
    // The page envelope is never shaped away.
    assert_eq!(default["structuredContent"]["total"], 1);
    assert_eq!(default["structuredContent"]["returned"], 1);

    // `fields` projects, and is honoured literally.
    let projected = s.call(4, "clove_list", json!({ "fields": ["id", "title"] }));
    let row = &projected["structuredContent"]["items"][0];
    assert_eq!(
        row.as_object().unwrap().len(),
        2,
        "exactly the two asked for"
    );
    assert!(row.get("id").is_some() && row.get("title").is_some());

    // `compact: false` restores the pre-existing full shape for any client that
    // depended on it.
    let full = s.call(5, "clove_list", json!({ "compact": false }));
    let row = &full["structuredContent"]["items"][0];
    assert!(row["assignee"].is_null(), "opt-out returns the null keys");
    assert_eq!(row["labels"], json!([]));
    assert_eq!(row["schema"], 1);

    // `clove_show` shapes too, and `ready: false` survives compaction — it is an
    // answer, not an absence.
    let id = default["structuredContent"]["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = s.call(6, "clove_show", json!({ "id": id }));
    assert_eq!(shown["structuredContent"]["ready"], true);
    assert!(shown["structuredContent"].get("assignee").is_none());

    s.shutdown();
}

/// A tool error reads the same whether or not a daemon is running.
///
/// Read tools never reach the daemon and write tools fall back to local ops
/// when none is up, so the classification being shared was not enough: the
/// daemon rendered `CODE: message` and the local path rendered just `message`.
/// Within one session `clove_show` and `clove_status` disagreed about the same
/// missing id, and a script updated to match the documented `ITEM_NOT_FOUND:`
/// spelling broke again the moment the daemon stopped.
#[test]
fn error_text_carries_the_code_without_a_daemon() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());

    for tool in ["clove_show", "clove_comments", "clove_dep_tree"] {
        let r = s.call(2, tool, json!({ "id": "proj-ZZZZZZZZ" }));
        assert_eq!(r["isError"], true, "{tool} should error");
        let text = r["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("ITEM_NOT_FOUND"),
            "{tool} must name the code like the daemon does: {text}"
        );
    }

    // A write tool on the local fallback path renders the same way.
    let r = s.call(
        3,
        "clove_status",
        json!({ "id": "proj-ZZZZZZZZ", "status": "closed" }),
    );
    assert_eq!(r["isError"], true);
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("ITEM_NOT_FOUND"), "got {text}");

    // A validation failure carries its own code, not a generic one.
    let created = s.call(4, "clove_new", json!({ "title": "real" }));
    let id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = s.call(5, "clove_edit", json!({ "id": id, "priority": 9 }));
    assert_eq!(r["isError"], true);
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("VALIDATION_ERROR"),
        "a bad priority is a validation error: {text}"
    );

    s.shutdown();
}

/// `clove_dep_tree {depth: 0}` means unlimited, as `--depth 0` does on the CLI
/// and as DESIGN §7.8 says. It used to pass 0 through, returning the root with
/// no children — a client asking for the whole tree got one node and no error.
#[test]
fn dep_tree_depth_zero_is_unlimited() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());
    let mut previous: Option<String> = None;
    for i in 0..4 {
        let args = match &previous {
            Some(dep) => json!({ "title": format!("level {i}"), "deps": [dep] }),
            None => json!({ "title": format!("level {i}") }),
        };
        let created = s.call(2, "clove_new", args);
        previous = Some(
            created["structuredContent"]["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let root = previous.unwrap();

    let depth = |v: &Value| -> usize {
        let mut n = 0;
        let mut node = v;
        while node["children"].as_array().map(|c| !c.is_empty()) == Some(true) {
            node = &node["children"][0];
            n += 1;
        }
        n
    };

    let full = s.call(3, "clove_dep_tree", json!({ "id": root, "depth": 0 }));
    assert_eq!(
        depth(&full["structuredContent"]),
        3,
        "depth 0 must return the whole chain, not the root alone"
    );
    // A real depth still bounds it.
    let bounded = s.call(4, "clove_dep_tree", json!({ "id": root, "depth": 1 }));
    assert_eq!(depth(&bounded["structuredContent"]), 1);

    s.shutdown();
}

#[test]
fn tool_error_is_reported_as_is_error() {
    let dir = init_repo();
    let mut s = Session::start(dir.path());
    // A malformed id → the tool returns an error result (not a protocol error).
    let result = s.call(2, "clove_show", json!({ "id": "not-a-valid-id" }));
    assert_eq!(result["isError"], true, "bad id surfaces as a tool error");
    s.shutdown();
}

/// `clove mcp` auto-starts the daemon (topology B) and the heartbeat keeps it
/// alive + accrues ping stats. Unix-only (kills the spawned daemon at the end).
#[cfg(unix)]
#[test]
fn auto_starts_daemon_and_heartbeats() {
    use clove_ipc::DaemonClient;
    use std::time::{Duration, Instant};

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }

    let dir = init_repo();
    let clove_dir = camino::Utf8PathBuf::from_path_buf(dir.path().join(".clove")).unwrap();

    // `clove mcp` auto-starts the daemon by locating `cloved` next to its own
    // binary — but `cargo test -p clove` builds only `clove`, not the separate
    // `cloved` crate, so under a scoped run the daemon could never spawn (a 6s
    // timeout). Build `cloved` on demand and point `CLOVED_PATH` at it so the
    // test is self-sufficient under any invocation (`-p clove` or `--workspace`).
    let cloved = escargot::CargoBuild::new()
        .package("cloved")
        .bin("cloved")
        .run()
        .expect("build cloved for the daemon auto-start test");

    // Daemon auto-start enabled; web disabled (avoid port contention) and a fast
    // heartbeat so the test observes pings accruing without a long wait.
    let mut cmd = clove(dir.path());
    cmd.env("CLOVED_PATH", cloved.path())
        .env("CLOVED_DISABLE_WEB", "1")
        .env("CLOVE_MCP_HEARTBEAT_MS", "150");
    let mut s = Session::start_cmd(cmd);

    // The MCP server should have brought a daemon up. Wait briefly for readiness.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(6) && !clove_dir.join("daemon.pid").exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        clove_dir.join("daemon.pid").exists(),
        "clove mcp should have auto-started the daemon"
    );

    // A write tool routes through the daemon and lands on disk.
    let created = s.call(2, "clove_new", json!({ "title": "via daemon" }));
    let id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(dir
        .path()
        .join(".clove/issues")
        .join(format!("{id}.md"))
        .exists());

    // Ping stats accrue: query the daemon directly and confirm the count climbs
    // as the heartbeat fires.
    let mut client = DaemonClient::probe(&clove_dir).expect("daemon alive");
    let first = client.status().unwrap().ping_count;
    assert!(first >= 1, "startup ensure + probe should have pinged");
    std::thread::sleep(Duration::from_millis(450)); // ~3 heartbeat ticks
    let later = client.status().unwrap().ping_count;
    assert!(
        later > first,
        "heartbeat must keep pinging (first={first}, later={later})"
    );

    s.shutdown();

    // Tear down the spawned daemon so the test leaves nothing running.
    if let Ok(pid) = std::fs::read_to_string(clove_dir.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            unsafe {
                libc_kill(pid, 15);
            }
        }
    }
}

/// A write the *daemon* rejects must carry the same error classification the
/// same failure produces locally. `cloved` used to emit a private code set
/// (`not_found`, `op_failed`, …) that collapsed distinct failure classes into
/// one bucket; it now emits `clove_types::error_code`, so the wire code matches
/// the local one and the numeric exit rides along with it.
///
/// Unix-only + escargot-built `cloved`, matching the other daemon tests here.
#[cfg(unix)]
#[test]
fn daemon_rejected_write_carries_the_shared_error_code() {
    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }

    let dir = init_repo();
    let clove_dir = camino::Utf8PathBuf::from_path_buf(dir.path().join(".clove")).unwrap();
    let cloved = escargot::CargoBuild::new()
        .package("cloved")
        .bin("cloved")
        .run()
        .expect("build cloved for the error-classification test");

    let mut cmd = clove(dir.path());
    cmd.env("CLOVED_PATH", cloved.path())
        .env("CLOVED_DISABLE_WEB", "1");
    let mut s = Session::start_cmd(cmd);

    // A well-formed id with no backing item: the write routes to the daemon,
    // which rejects it. (A malformed id would fail client-side and never reach
    // the daemon, so it would not exercise the wire at all.)
    let result = s.call(
        2,
        "clove_status",
        json!({ "id": "proj-ZZZZZZZZ", "status": "closed" }),
    );
    assert_eq!(result["isError"], true, "missing item must be an error");
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("ITEM_NOT_FOUND"),
        "daemon error must carry the shared code, got: {text:?}"
    );

    s.shutdown();

    if let Ok(pid) = std::fs::read_to_string(clove_dir.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            unsafe {
                libc_kill(pid, 15);
            }
        }
    }
}

/// gh-21: after subscribing to `clove://ready`, a mutation that bumps the daemon's
/// change-generation makes the server push a `notifications/resources/updated` for
/// that URI. Needs the daemon (the change signal), so Unix-only + escargot-built
/// `cloved`. Uses a reader thread + channel so the notification wait is bounded.
#[cfg(unix)]
#[test]
fn subscribed_resource_updated_on_mutation() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }

    let dir = init_repo();
    let clove_dir = camino::Utf8PathBuf::from_path_buf(dir.path().join(".clove")).unwrap();
    let cloved = escargot::CargoBuild::new()
        .package("cloved")
        .bin("cloved")
        .run()
        .expect("build cloved for the push-notification test");

    let mut child = clove(dir.path())
        .arg("mcp")
        .env("CLOVED_PATH", cloved.path())
        .env("CLOVED_DISABLE_WEB", "1")
        .env("CLOVE_MCP_NOTIFY_MS", "50") // poll fast so the test doesn't wait
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn clove mcp");
    let mut stdin = child.stdin.take().unwrap();

    // Reader thread: every stdout line → channel. Bounds the notification wait
    // (recv_timeout) and never blocks the main thread on a missing message.
    let (tx, rx) = mpsc::channel::<Value>();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });

    let send = |stdin: &mut ChildStdin, msg: Value| {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    };
    // Read messages until one satisfies `pred`, or fail after `deadline`.
    let wait_for =
        |rx: &mpsc::Receiver<Value>, deadline: Instant, pred: &dyn Fn(&Value) -> bool| loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            match rx.recv_timeout(remaining.max(Duration::from_millis(1))) {
                Ok(v) => {
                    if pred(&v) {
                        return v;
                    }
                }
                Err(_) => panic!("timed out waiting for the expected message"),
            }
        };

    // Handshake.
    send(
        &mut stdin,
        json!({ "jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"test","version":"0.0.0"}}}),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    wait_for(&rx, deadline, &|v| v["id"] == 1);
    send(
        &mut stdin,
        json!({ "jsonrpc":"2.0","method":"notifications/initialized" }),
    );

    // Subscribe to clove://ready BEFORE mutating, so the change is pushed.
    send(
        &mut stdin,
        json!({ "jsonrpc":"2.0","id":2,"method":"resources/subscribe","params":{"uri":"clove://ready"}}),
    );
    wait_for(&rx, deadline, &|v| v["id"] == 2);

    // A write routes through the daemon → mark_dirty → change-generation bump →
    // the notifier polls (50ms) and pushes resources/updated for the subscription.
    send(
        &mut stdin,
        json!({ "jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"clove_new","arguments":{"title":"trigger a push"}}}),
    );

    // Wait for the resources/updated notification for clove://ready (tolerating the
    // interleaved tool-call response and the coarse resources/list_changed frame).
    let note = wait_for(&rx, deadline, &|v| {
        v["method"] == "notifications/resources/updated" && v["params"]["uri"] == "clove://ready"
    });
    assert_eq!(note["params"]["uri"], "clove://ready");

    drop(stdin);
    let _ = child.wait();
    if let Ok(pid) = std::fs::read_to_string(clove_dir.join("daemon.pid")) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            unsafe {
                libc_kill(pid, 15);
            }
        }
    }
}
