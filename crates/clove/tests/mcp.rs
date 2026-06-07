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
    assert_eq!(tools.len(), 12, "all 12 tools advertised");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "clove_ready",
        "clove_blocked",
        "clove_list",
        "clove_show",
        "clove_search",
        "clove_dep_tree",
        "clove_stats",
        "clove_new",
        "clove_status",
        "clove_edit",
        "clove_comment",
        "clove_dep_add",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
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

    // Daemon auto-start enabled; web disabled (avoid port contention) and a fast
    // heartbeat so the test observes pings accruing without a long wait.
    let mut cmd = clove(dir.path());
    cmd.env("CLOVED_DISABLE_WEB", "1")
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
