//! End-to-end two-way GitHub sync coverage (T-M06) against a **deterministic
//! in-process mock GitHub server**.
//!
//! The pure reconciliation matrix is unit-tested offline in
//! `clove_import::sync`. This suite exercises the *whole* path — the real `clove
//! sync` binary, octocrab's real HTTP client, the create/update/list REST calls,
//! the local write-back of `external_ref`, and the persisted sync state — by
//! pointing octocrab's API base (`CLOVE_GITHUB_API_URL`) at a tiny HTTP server we
//! run in the test process and whose state we can seed and inspect.
//!
//! The server speaks just enough of the GitHub Issues REST API for clove:
//! `GET/POST /repos/{o}/{r}/issues` and `PATCH /repos/{o}/{r}/issues/{n}`,
//! returning octocrab-model-valid JSON. It is fully deterministic (a monotonic
//! clock for `updated_at`), so every scenario below is reproducible with no
//! network and no token.
//!
//! Since Phase 4b there is no in-process `github` feature: `clove sync github`
//! resolves the external `clove-sync-github` plugin (PLUGIN_SYSTEM.md §4.2/§8).
//! This suite therefore exercises the **full dispatch path** — the real `clove`
//! binary routes to the plugin, which runs octocrab against the mock. The plugin
//! is built once (via escargot) and its directory placed on `CLOVE_PLUGIN_PATH`
//! (see the `clove()` harness); the plugin emits byte-identical JSON to the old
//! built-in, so every assertion below holds unchanged.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Mock GitHub server
// ---------------------------------------------------------------------------

/// Shared, inspectable server state.
struct State {
    /// The octocrab-shaped issue JSON objects currently "on GitHub".
    issues: Vec<Value>,
    /// issue number → its comment JSON objects.
    comments: std::collections::HashMap<u64, Vec<Value>>,
    /// Next issue number to mint on create.
    next_number: u64,
    /// Next comment id to mint.
    next_comment_id: u64,
    /// Monotonic clock (seconds past a fixed far-future base) for `updated_at`,
    /// so a created/edited issue always looks strictly newer than seeded data.
    clock: u64,

    // --- Fault injection (retry / idempotency / partial-failure coverage) ---
    /// Number of upcoming issue-create POSTs that still create the issue but then
    /// respond 500 — simulating a write the server committed whose response was
    /// lost. A blind retry would create a duplicate.
    create_commit_then_fail: usize,
    /// While `Some`, every issue PATCH responds with this HTTP status and mutates
    /// nothing (used to test the retry error-class gating).
    patch_fail_status: Option<u16>,
    /// Number of upcoming issue PATCHes that fail 503 (transient) before behaving
    /// normally — used to confirm idempotent updates *are* retried on 5xx.
    patch_transient_fails: usize,
    /// While `Some(n)`, a comment POST to issue `n` responds 500 and adds nothing.
    fail_comment_post_for: Option<u64>,
    /// Count of issue-create POSTs actually received.
    issue_post_count: u64,
    /// Total GETs of any issue's comments endpoint (for skip-coverage asserts).
    comment_get_count: u64,
    /// Count of issue PATCHes actually received.
    patch_count: u64,
}

impl State {
    /// The next monotonic RFC3339 timestamp (base 2030 + clock seconds).
    fn tick(&mut self) -> String {
        self.clock += 1;
        let base = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap();
        (base + chrono::Duration::seconds(self.clock as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }
}

/// A running mock server; drop ends nothing (thread is detached, dies with proc).
struct MockGitHub {
    state: Arc<Mutex<State>>,
    addr: SocketAddr,
}

impl MockGitHub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            issues: Vec::new(),
            comments: std::collections::HashMap::new(),
            comment_get_count: 0,
            next_number: 1,
            next_comment_id: 1000,
            clock: 0,
            create_commit_then_fail: 0,
            patch_fail_status: None,
            patch_transient_fails: 0,
            fail_comment_post_for: None,
            issue_post_count: 0,
            patch_count: 0,
        }));
        let thread_state = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle_conn(stream, &thread_state);
            }
        });
        Self { state, addr }
    }

    /// Seed an issue directly (simulating one already on GitHub).
    fn seed(&self, number: u64, title: &str, body: &str, state: &str) {
        let mut s = self.state.lock().unwrap();
        let updated = s.tick();
        if number >= s.next_number {
            s.next_number = number + 1;
        }
        s.issues.push(make_issue(
            number,
            title,
            body,
            state,
            &[],
            &[],
            &updated,
            None,
        ));
    }

    /// Mutate an existing issue (simulating a GitHub-side edit) and bump its
    /// `updated_at` to a fresh monotonic tick.
    fn edit(&self, number: u64, f: impl FnOnce(&mut Value)) {
        let mut s = self.state.lock().unwrap();
        let updated = s.tick();
        let issue = s
            .issues
            .iter_mut()
            .find(|i| i["number"] == json!(number))
            .expect("seeded issue exists");
        f(issue);
        issue["updated_at"] = json!(updated);
    }

    /// Snapshot the issue with `number`, if present.
    fn issue(&self, number: u64) -> Option<Value> {
        let s = self.state.lock().unwrap();
        s.issues
            .iter()
            .find(|i| i["number"] == json!(number))
            .cloned()
    }

    fn issue_count(&self) -> usize {
        self.state.lock().unwrap().issues.len()
    }

    /// Arm the next `n` issue-create POSTs to commit the write but respond 500.
    fn arm_create_commit_then_fail(&self, n: usize) {
        self.state.lock().unwrap().create_commit_then_fail = n;
    }

    /// Make every issue PATCH respond with `status` (no mutation) until cleared.
    fn set_patch_fail_status(&self, status: Option<u16>) {
        self.state.lock().unwrap().patch_fail_status = status;
    }

    /// Arm the next `n` issue PATCHes to fail 503 (transient) before succeeding.
    fn arm_patch_transient_fails(&self, n: usize) {
        self.state.lock().unwrap().patch_transient_fails = n;
    }

    /// Make comment POSTs to `number` fail with 500 (no mutation) until cleared.
    fn set_fail_comment_post_for(&self, number: Option<u64>) {
        self.state.lock().unwrap().fail_comment_post_for = number;
    }

    fn issue_post_count(&self) -> u64 {
        self.state.lock().unwrap().issue_post_count
    }

    fn patch_count(&self) -> u64 {
        self.state.lock().unwrap().patch_count
    }

    fn comment_get_count(&self) -> u64 {
        self.state.lock().unwrap().comment_get_count
    }

    /// Seed a comment on an existing issue (simulating a GitHub-side comment).
    fn seed_comment(&self, number: u64, author: &str, body: &str) {
        let mut s = self.state.lock().unwrap();
        let id = s.next_comment_id;
        s.next_comment_id += 1;
        let created = s.tick();
        s.comments
            .entry(number)
            .or_default()
            .push(make_comment(id, author, body, &created));
    }

    /// Set an issue's assignees to `logins` (simulating GitHub-side edits).
    fn set_assignees(&self, number: u64, logins: &[&str]) {
        self.edit(number, |i| {
            i["assignees"] = Value::Array(logins.iter().map(|l| make_user(l)).collect());
            i["assignee"] = logins.first().map(|l| make_user(l)).unwrap_or(Value::Null);
        });
    }

    /// Set an issue's labels (simulating GitHub-side label edits).
    fn set_labels(&self, number: u64, names: &[&str]) {
        self.edit(number, |i| {
            i["labels"] = Value::Array(
                names
                    .iter()
                    .enumerate()
                    .map(|(idx, n)| make_label(idx as u64, n))
                    .collect(),
            );
        });
    }

    /// The label names currently on an issue (sorted).
    fn label_names(&self, number: u64) -> Vec<String> {
        let mut names: Vec<String> = self
            .issue(number)
            .and_then(|i| i["labels"].as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|l| l["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Whether an issue is currently closed on the mock.
    fn is_closed(&self, number: u64) -> bool {
        self.issue(number)
            .map(|i| i["state"] == json!("closed"))
            .unwrap_or(false)
    }

    /// Drop an issue (simulating a GitHub-side delete).
    fn remove_issue(&self, number: u64) {
        let mut s = self.state.lock().unwrap();
        s.issues.retain(|i| i["number"] != json!(number));
    }

    /// The assignee logins currently on an issue.
    fn assignee_logins(&self, number: u64) -> Vec<String> {
        self.issue(number)
            .and_then(|i| i["assignees"].as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|u| u["login"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The current `state_reason` of an issue, if any.
    fn state_reason(&self, number: u64) -> Option<String> {
        self.issue(number)
            .and_then(|i| i["state_reason"].as_str().map(str::to_owned))
    }

    /// The comment bodies currently on an issue.
    fn comment_bodies(&self, number: u64) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.comments
            .get(&number)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c["body"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Handle one connection: parse a single request, route it, respond, close.
fn handle_conn(mut stream: TcpStream, state: &Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    // Request line.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();

    // Headers.
    let mut content_length = 0usize;
    let mut expect_continue = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("expect:") && lower.contains("100-continue") {
            expect_continue = true;
        }
    }
    if expect_continue {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }

    // Body.
    let mut body = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let (status, payload) = route(&method, &target, &body_json, state);
    let body_str = payload.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
        body_str.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Route a request to its handler, returning `(status line, json body)`.
fn route(
    method: &str,
    target: &str,
    body: &Value,
    state: &Arc<Mutex<State>>,
) -> (&'static str, Value) {
    let path = target.split('?').next().unwrap_or("");
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    // Expect /repos/{owner}/{repo}/issues[/{number}].
    let is_issues = segments.len() >= 4 && segments[0] == "repos" && segments[3] == "issues";
    if !is_issues {
        return ("404 Not Found", json!({ "message": "not found" }));
    }
    let number = segments.get(4).and_then(|n| n.parse::<u64>().ok());
    let is_comments = segments.get(5) == Some(&"comments");

    let mut s = state.lock().unwrap();

    // /repos/{o}/{r}/issues/{n}/comments
    if is_comments {
        let Some(number) = number else {
            return ("404 Not Found", json!({ "message": "bad issue" }));
        };
        return match method {
            "GET" => {
                s.comment_get_count += 1;
                (
                    "200 OK",
                    Value::Array(s.comments.get(&number).cloned().unwrap_or_default()),
                )
            }
            "POST" => {
                if s.fail_comment_post_for == Some(number) {
                    return (
                        "500 Internal Server Error",
                        json!({ "message": "comment post failed" }),
                    );
                }
                let id = s.next_comment_id;
                s.next_comment_id += 1;
                let created = s.tick();
                let body_text = body["body"].as_str().unwrap_or("").to_owned();
                let comment = make_comment(id, "tester", &body_text, &created);
                s.comments.entry(number).or_default().push(comment.clone());
                // Real GitHub bumps the issue's `updated_at` when a comment is
                // posted; mirror that so the sync's comment-fingerprint handling is
                // exercised faithfully (a stale fingerprint would flag a spurious
                // remote change on the next run).
                if let Some(issue) = s.issues.iter_mut().find(|i| i["number"] == json!(number)) {
                    issue["updated_at"] = json!(created);
                }
                ("201 Created", comment)
            }
            _ => ("404 Not Found", json!({ "message": "unsupported" })),
        };
    }

    match (method, number) {
        ("GET", None) => {
            // Real GitHub reports each issue's live comment count in the list;
            // the sync's comment-fetch skip relies on it.
            let mut issues = s.issues.clone();
            for issue in &mut issues {
                let number = issue["number"].as_u64().unwrap_or(0);
                issue["comments"] = json!(s.comments.get(&number).map(Vec::len).unwrap_or(0));
            }
            ("200 OK", Value::Array(issues))
        }
        ("POST", None) => {
            s.issue_post_count += 1;
            let number = s.next_number;
            s.next_number += 1;
            let updated = s.tick();
            let title = body["title"].as_str().unwrap_or("").to_owned();
            let issue_body = body["body"].as_str().unwrap_or("").to_owned();
            let labels = str_array(&body["labels"]);
            let assignees = str_array(&body["assignees"]);
            let issue = make_issue(
                number,
                &title,
                &issue_body,
                "open",
                &labels,
                &assignees,
                &updated,
                None,
            );
            s.issues.push(issue.clone());
            // Simulate a committed-write-with-lost-response: the issue exists on
            // the server, but the client sees an error. A blind retry would create
            // a duplicate.
            if s.create_commit_then_fail > 0 {
                s.create_commit_then_fail -= 1;
                return (
                    "500 Internal Server Error",
                    json!({ "message": "created but response lost" }),
                );
            }
            ("201 Created", issue)
        }
        ("PATCH", Some(number)) => {
            s.patch_count += 1;
            if s.patch_transient_fails > 0 {
                s.patch_transient_fails -= 1;
                return (status_line(503), json!({ "message": "try again later" }));
            }
            if let Some(code) = s.patch_fail_status {
                return (status_line(code), json!({ "message": "patch failed" }));
            }
            let updated = s.tick();
            let Some(issue) = s.issues.iter_mut().find(|i| i["number"] == json!(number)) else {
                return ("404 Not Found", json!({ "message": "no such issue" }));
            };
            if let Some(t) = body.get("title").and_then(Value::as_str) {
                issue["title"] = json!(t);
            }
            if let Some(b) = body.get("body").and_then(Value::as_str) {
                issue["body"] = json!(b);
            }
            if let Some(st) = body.get("state").and_then(Value::as_str) {
                issue["state"] = json!(st);
                if st == "closed" {
                    issue["closed_at"] = json!(updated);
                    // GitHub stamps a reason on close: the caller's, else
                    // `completed` (so a bare close resets a prior `not_planned`).
                    issue["state_reason"] = match body.get("state_reason").and_then(Value::as_str) {
                        Some(reason) => json!(reason),
                        None => json!("completed"),
                    };
                } else {
                    issue["closed_at"] = Value::Null;
                    issue["state_reason"] = Value::Null;
                }
            } else if let Some(reason) = body.get("state_reason").and_then(Value::as_str) {
                issue["state_reason"] = json!(reason);
            }
            if let Some(labels) = body.get("labels") {
                issue["labels"] = Value::Array(
                    str_array(labels)
                        .iter()
                        .enumerate()
                        .map(|(i, name)| make_label(i as u64, name))
                        .collect(),
                );
            }
            if let Some(assignees) = body.get("assignees") {
                issue["assignees"] =
                    Value::Array(str_array(assignees).iter().map(|a| make_user(a)).collect());
            }
            issue["updated_at"] = json!(updated);
            ("200 OK", issue.clone())
        }
        _ => ("404 Not Found", json!({ "message": "unsupported" })),
    }
}

/// Map an HTTP status code to a status line for the fault-injection responses.
fn status_line(code: u16) -> &'static str {
    match code {
        422 => "422 Unprocessable Entity",
        429 => "429 Too Many Requests",
        500 => "500 Internal Server Error",
        503 => "503 Service Unavailable",
        _ => "400 Bad Request",
    }
}

/// Extract a `Vec<String>` from a JSON value that may be an array of strings.
fn str_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Build an octocrab-`Author`-valid user object.
fn make_user(login: &str) -> Value {
    let u = |p: &str| json!(format!("https://example.test/{login}/{p}"));
    json!({
        "login": login,
        "id": 1,
        "node_id": "U_1",
        "avatar_url": u("avatar"),
        "gravatar_id": "",
        "url": u("url"),
        "html_url": u("html"),
        "followers_url": u("followers"),
        "following_url": u("following"),
        "gists_url": u("gists"),
        "starred_url": u("starred"),
        "subscriptions_url": u("subscriptions"),
        "organizations_url": u("orgs"),
        "repos_url": u("repos"),
        "events_url": u("events"),
        "received_events_url": u("received"),
        "type": "User",
        "site_admin": false,
        "name": null,
        "patch_url": null,
        "email": null
    })
}

/// Build an octocrab-`Label`-valid label object.
fn make_label(id: u64, name: &str) -> Value {
    json!({
        "id": id,
        "node_id": format!("L_{id}"),
        "url": format!("https://example.test/labels/{name}"),
        "name": name,
        "description": null,
        "color": "ededed",
        "default": false
    })
}

/// Build an octocrab-`Comment`-valid issue-comment object.
fn make_comment(id: u64, author: &str, body: &str, created_at: &str) -> Value {
    json!({
        "id": id,
        "node_id": format!("IC_{id}"),
        "url": format!("https://example.test/comments/{id}"),
        "html_url": format!("https://example.test/comments/{id}/html"),
        "issue_url": null,
        "body": body,
        "body_text": null,
        "body_html": null,
        "author_association": "OWNER",
        "user": make_user(author),
        "created_at": created_at,
        "updated_at": created_at
    })
}

/// Build an octocrab-`Issue`-valid issue object (every field present so the
/// strict model never hits a missing-field error).
#[allow(clippy::too_many_arguments)]
fn make_issue(
    number: u64,
    title: &str,
    body: &str,
    state: &str,
    labels: &[String],
    assignees: &[String],
    updated_at: &str,
    closed_at: Option<&str>,
) -> Value {
    let u = |p: &str| json!(format!("https://example.test/issues/{number}/{p}"));
    json!({
        "id": number,
        "node_id": format!("I_{number}"),
        "url": u("url"),
        "repository_url": json!("https://example.test/repo"),
        "labels_url": u("labels"),
        "comments_url": u("comments"),
        "events_url": u("events"),
        "html_url": u("html"),
        "number": number,
        "state": state,
        "state_reason": null,
        "title": title,
        "body": body,
        "body_text": null,
        "body_html": null,
        "user": make_user("octocat"),
        "labels": labels.iter().enumerate().map(|(i, n)| make_label(i as u64, n)).collect::<Vec<_>>(),
        "assignee": assignees.first().map(|a| make_user(a)),
        "assignees": assignees.iter().map(|a| make_user(a)).collect::<Vec<_>>(),
        "author_association": "OWNER",
        "milestone": null,
        "locked": false,
        "active_lock_reason": null,
        "comments": 0,
        "pull_request": null,
        "closed_at": closed_at,
        "closed_by": null,
        "created_at": updated_at,
        "updated_at": updated_at
    })
}

// ---------------------------------------------------------------------------
// CLI harness
// ---------------------------------------------------------------------------

/// Build the `clove-sync-github` plugin once (across all tests) and return the
/// directory that contains it, to be placed on `CLOVE_PLUGIN_PATH` so that
/// `clove sync github` resolves the plugin. Built via escargot into the workspace
/// target dir (the binary persists there for the process lifetime).
///
/// Note the co-location assumption: plugin resolution searches the running
/// `clove`'s own directory *before* `CLOVE_PLUGIN_PATH` (see `plugin.rs`).
/// escargot builds this plugin into the same workspace `target/<profile>/` that
/// holds the test's `cargo_bin("clove")`, so the current-exe-dir hit and the
/// `CLOVE_PLUGIN_PATH` hit are the identical, freshly-rebuilt file — no stale
/// sibling can win. If escargot's output dir ever diverged from the test binary's
/// dir, a stale sibling could take precedence.
fn plugin_dir() -> &'static Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let run = escargot::CargoBuild::new()
            .package("clove-sync-github")
            .bin("clove-sync-github")
            .run()
            .expect("build clove-sync-github plugin");
        run.path()
            .parent()
            .expect("plugin binary has a parent dir")
            .to_owned()
    })
}

fn clove(dir: &Path, addr: SocketAddr) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    // `clove sync github` dispatches to the external `clove-sync-github` plugin;
    // point the plugin search path at the escargot-built binary's dir.
    cmd.env("CLOVE_PLUGIN_PATH", plugin_dir());
    // A dummy token (the mock ignores it) and the API-base override that aims
    // octocrab at our server instead of github.com — inherited by the plugin the
    // host exec's, which is what actually talks to the mock.
    cmd.env("GITHUB_TOKEN", "test-token");
    cmd.env("CLOVE_GITHUB_API_URL", format!("http://{addr}"));
    // Keep any retry backoff effectively instant.
    cmd.env("CLOVE_GITHUB_RETRY_MS", "1");
    cmd
}

fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    clove(dir.path(), addr)
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// Run `clove sync github <repo> [extra…]` and return the parsed JSON envelope.
fn sync(dir: &Path, addr: SocketAddr, extra: &[&str]) -> Value {
    let mut args = vec!["sync", "--format", "json", "github", "owner/repo"];
    args.extend_from_slice(extra);
    let out = clove(dir, addr).args(&args).output().unwrap();
    assert!(out.status.success(), "sync failed: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad json: {e}\n{out:?}"))
}

/// Run `clove sync github <repo>` allowing failure; returns the raw process output.
fn sync_raw(dir: &Path, addr: SocketAddr, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["sync", "--format", "json", "github", "owner/repo"];
    args.extend_from_slice(extra);
    clove(dir, addr).args(&args).output().unwrap()
}

/// Run a directional view `clove <mux> github <repo>` (`import` = pull-only,
/// `export` = push-only) and return the parsed JSON envelope. Dispatches to the
/// same `clove-sync-github` plugin as `sync` via the umbrella fallback (§4.2).
fn run_mux(dir: &Path, addr: SocketAddr, mux: &str, extra: &[&str]) -> Value {
    let mut args = vec![mux, "--format", "json", "github", "owner/repo"];
    args.extend_from_slice(extra);
    let out = clove(dir, addr).args(&args).output().unwrap();
    assert!(out.status.success(), "{mux} github failed: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad json: {e}\n{out:?}"))
}

/// The number of local items in the store.
fn local_item_count(dir: &Path, addr: SocketAddr) -> usize {
    let out = clove(dir, addr)
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"].as_array().map(|a| a.len()).unwrap_or(0)
}

/// The single local item's id (assumes exactly one).
fn only_item_id(dir: &Path, addr: SocketAddr) -> String {
    let out = clove(dir, addr)
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"][0]["id"].as_str().unwrap().to_owned()
}

/// Read a local item object via `clove show`.
fn show(dir: &Path, addr: SocketAddr, id: &str) -> Value {
    let out = clove(dir, addr)
        .args(["show", id, "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"].clone()
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[test]
fn push_create_writes_back_ref_and_is_idempotent() {
    // The critical idempotency fix: a created issue's number must be written back
    // locally so the next sync UPDATES instead of creating a duplicate.
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Fix the bug", "--type", "bug"])
        .assert()
        .success();

    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["data"]["pushed_created"], 1, "{v}");
    assert_eq!(mock.issue_count(), 1, "one issue created on GitHub");
    assert_eq!(mock.issue(1).unwrap()["title"], "Fix the bug");

    // The local item now carries external_ref gh-1.
    let id = only_item_id(dir.path(), mock.addr);
    assert_eq!(show(dir.path(), mock.addr, &id)["external_ref"], "gh-1");

    // Second sync: nothing to push, no duplicate.
    let v2 = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v2["data"]["pushed_created"], 0, "{v2}");
    assert_eq!(v2["data"]["in_sync"], 1, "{v2}");
    assert_eq!(mock.issue_count(), 1, "no duplicate issue");
}

#[test]
fn import_github_is_pull_only() {
    // `clove import github` = the pull-only view of the reconcile (§4.2). A remote
    // issue is pulled; a local-only item that a full `sync` would push must NOT be
    // pushed to GitHub.
    let mock = MockGitHub::start();
    mock.seed(5, "From GitHub", "Remote body.", "open");
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Local only", "--type", "bug"])
        .assert()
        .success();

    let v = run_mux(dir.path(), mock.addr, "import", &[]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(
        v["data"]["pulled_created"], 1,
        "the remote issue is pulled: {v}"
    );
    assert_eq!(v["data"]["pushed_created"], 0, "import must not push: {v}");
    // The remote issue became a local item; the local-only item stayed local.
    assert_eq!(
        local_item_count(dir.path(), mock.addr),
        2,
        "pulled + local-only"
    );
    assert_eq!(
        mock.issue_count(),
        1,
        "import must not create remote issues"
    );
}

#[test]
fn export_github_is_push_only() {
    // `clove export github` = the push-only view. A local-only item is pushed; a
    // remote issue that a full `sync` would pull must NOT be pulled locally.
    let mock = MockGitHub::start();
    mock.seed(5, "From GitHub", "Remote body.", "open");
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Local only", "--type", "bug"])
        .assert()
        .success();

    let v = run_mux(dir.path(), mock.addr, "export", &[]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(
        v["data"]["pushed_created"], 1,
        "the local item is pushed: {v}"
    );
    assert_eq!(v["data"]["pulled_created"], 0, "export must not pull: {v}");
    // The remote gh-5 was NOT pulled: only the one local-only item exists locally.
    assert_eq!(
        local_item_count(dir.path(), mock.addr),
        1,
        "export must not pull remote issues into the store"
    );
    // The pushed local item exists on GitHub now (gh-5 seeded + the new push).
    assert_eq!(
        mock.issue_count(),
        2,
        "export created the local item on GitHub"
    );
}

#[test]
fn pull_create_makes_local_item() {
    let mock = MockGitHub::start();
    mock.seed(5, "From GitHub", "Remote body.", "open");
    let dir = init_repo();

    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pulled_created"], 1, "{v}");

    let id = only_item_id(dir.path(), mock.addr);
    let item = show(dir.path(), mock.addr, &id);
    assert_eq!(item["title"], "From GitHub");
    assert_eq!(item["external_ref"], "gh-5");
    assert_eq!(item["source_system"], "github");
}

#[test]
fn pull_update_applies_remote_edit() {
    let mock = MockGitHub::start();
    mock.seed(5, "Original", "Body.", "open");
    let dir = init_repo();
    sync(dir.path(), mock.addr, &[]); // establishes the link + state
    let id = only_item_id(dir.path(), mock.addr);

    // Edit on GitHub only.
    mock.edit(5, |i| i["title"] = json!("Edited on GitHub"));
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pulled_updated"], 1, "{v}");
    assert_eq!(
        show(dir.path(), mock.addr, &id)["title"],
        "Edited on GitHub"
    );
}

#[test]
fn push_update_applies_local_edit() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Original", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1
    let id = only_item_id(dir.path(), mock.addr);

    // A new wall-clock second so the local `updated` strictly advances.
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Local edit"])
        .assert()
        .success();

    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pushed_updated"], 1, "{v}");
    assert_eq!(mock.issue(1).unwrap()["title"], "Local edit");
}

#[test]
fn conflict_newer_remote_wins() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Original", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1, both sides linked
    let id = only_item_id(dir.path(), mock.addr);

    // Edit BOTH sides since the last sync. The remote tick is in 2030 (always
    // newer than the local 2026 wall clock), so "newer" resolves to remote.
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Local change"])
        .assert()
        .success();
    mock.edit(1, |i| i["title"] = json!("Remote change"));

    let v = sync(dir.path(), mock.addr, &[]);
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{v}");
    assert_eq!(conflicts[0]["resolution"], "remote_wins", "{v}");
    // Remote won → local item adopts the remote title.
    assert_eq!(show(dir.path(), mock.addr, &id)["title"], "Remote change");
}

#[test]
fn conflict_prefer_local_pushes_local() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Original", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    let id = only_item_id(dir.path(), mock.addr);

    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Local wins"])
        .assert()
        .success();
    mock.edit(1, |i| i["title"] = json!("Remote change"));

    let v = sync(dir.path(), mock.addr, &["--prefer", "local"]);
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{v}");
    assert_eq!(conflicts[0]["resolution"], "local_wins", "{v}");
    // Local won → the GitHub issue adopts the local title.
    assert_eq!(mock.issue(1).unwrap()["title"], "Local wins");
}

#[test]
fn dry_run_touches_neither_side() {
    let mock = MockGitHub::start();
    mock.seed(5, "Remote only", "Body.", "open");
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Local only", "--type", "bug"])
        .assert()
        .success();
    let local_id = only_item_id(dir.path(), mock.addr);

    let v = sync(dir.path(), mock.addr, &["--dry-run"]);
    // Plan reports both a pull and a push, but applies nothing.
    assert_eq!(v["data"]["push_create"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["data"]["pull_create"].as_array().unwrap().len(), 1, "{v}");

    // GitHub still has only the seeded issue; the local item has no ref.
    assert_eq!(mock.issue_count(), 1, "dry-run created no issue");
    assert!(show(dir.path(), mock.addr, &local_id)["external_ref"].is_null());
    // And no new local item was pulled in.
    let out = clove(dir.path(), mock.addr)
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    let ls: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        ls["data"].as_array().unwrap().len(),
        1,
        "dry-run pulled nothing"
    );
}

/// Local comment bodies for an item via `clove comments`.
fn local_comment_bodies(dir: &Path, addr: SocketAddr, id: &str) -> Vec<String> {
    let out = clove(dir, addr)
        .args(["comments", id, "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["body"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn comment_pull_creates_local_comment() {
    let mock = MockGitHub::start();
    mock.seed(5, "Issue", "Body.", "open");
    mock.seed_comment(5, "octocat", "A remote comment");
    let dir = init_repo();

    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["comments_pulled"], 1, "{v}");
    let id = only_item_id(dir.path(), mock.addr);
    let bodies = local_comment_bodies(dir.path(), mock.addr, &id);
    assert!(
        bodies.iter().any(|b| b.contains("A remote comment")),
        "{bodies:?}"
    );
}

#[test]
fn comment_push_creates_gh_comment() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Has a comment", "--type", "bug"])
        .assert()
        .success();
    let id = only_item_id(dir.path(), mock.addr);
    clove(dir.path(), mock.addr)
        .args(["comment", &id, "A local comment"])
        .assert()
        .success();

    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pushed_created"], 1, "{v}");
    assert_eq!(v["data"]["comments_pushed"], 1, "{v}");
    let bodies = mock.comment_bodies(1);
    assert!(bodies.iter().any(|b| b == "A local comment"), "{bodies:?}");
}

#[test]
fn comment_sync_is_idempotent_both_directions() {
    let mock = MockGitHub::start();
    mock.seed(5, "Issue", "Body.", "open");
    mock.seed_comment(5, "octocat", "remote comment");
    let dir = init_repo();
    sync(dir.path(), mock.addr, &[]); // pull issue + comment
    let id = only_item_id(dir.path(), mock.addr);
    clove(dir.path(), mock.addr)
        .args(["comment", &id, "local comment"])
        .assert()
        .success();

    // First sync pushes the local comment.
    let v1 = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v1["data"]["comments_pushed"], 1, "{v1}");

    // Second sync: nothing new in either direction, and no duplicates anywhere.
    let v2 = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v2["data"]["comments_pulled"], 0, "{v2}");
    assert_eq!(v2["data"]["comments_pushed"], 0, "{v2}");
    assert_eq!(
        mock.comment_bodies(5).len(),
        2,
        "no duplicate GitHub comments"
    );
    assert_eq!(
        local_comment_bodies(dir.path(), mock.addr, &id).len(),
        2,
        "no duplicate local comments"
    );
}

#[test]
fn idle_issue_comment_fetch_is_skipped_on_the_next_sync() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Chatty", "--type", "bug"])
        .assert()
        .success();

    // First sync links the issue; its comment thread is fetched and the
    // local comment is pushed.
    let id = only_item_id(dir.path(), mock.addr);
    clove(dir.path(), mock.addr)
        .args(["comment", &id, "hello"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    let after_first = mock.comment_get_count();
    assert!(after_first >= 1, "first sync fetches the thread");

    // Nothing changed on either side: the next sync must not re-fetch the
    // comments for the idle issue.
    sync(dir.path(), mock.addr, &[]);
    assert_eq!(
        mock.comment_get_count(),
        after_first,
        "idle issue's comments were re-fetched"
    );

    // A remote comment changes the count → the fetch happens again and the
    // comment is pulled.
    let gh_number = show(dir.path(), mock.addr, &id)["external_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("gh-")
        .unwrap()
        .parse()
        .unwrap();
    mock.seed_comment(gh_number, "octocat", "from github");
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["comments_pulled"], 1, "{v}");
    assert!(mock.comment_get_count() > after_first);

    // And a new local comment also breaks the skip.
    let before = mock.comment_get_count();
    clove(dir.path(), mock.addr)
        .args(["comment", &id, "another local"])
        .assert()
        .success();
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["comments_pushed"], 1, "{v}");
    assert!(mock.comment_get_count() > before);
}

#[test]
fn no_comments_flag_skips_comment_sync() {
    let mock = MockGitHub::start();
    mock.seed(5, "Issue", "Body.", "open");
    mock.seed_comment(5, "octocat", "remote comment");
    let dir = init_repo();

    let v = sync(dir.path(), mock.addr, &["--no-comments"]);
    assert_eq!(v["data"]["comments_pulled"], 0, "{v}");
    let id = only_item_id(dir.path(), mock.addr);
    assert!(local_comment_bodies(dir.path(), mock.addr, &id).is_empty());
}

#[test]
fn push_preserves_human_added_assignee() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug", "--assignee", "alice"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1 assigned [alice]
    let id = only_item_id(dir.path(), mock.addr);

    // A human adds a second assignee on GitHub; clove syncs (pull) and keeps its
    // single primary (alice).
    mock.set_assignees(1, &["alice", "bob"]);
    sync(dir.path(), mock.addr, &[]);

    // A local-only edit triggers a push. clove must keep bob, not reset to [alice].
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Renamed"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);

    let logins = mock.assignee_logins(1);
    assert!(logins.contains(&"alice".to_owned()), "{logins:?}");
    assert!(
        logins.contains(&"bob".to_owned()),
        "human-added assignee must survive a clove push: {logins:?}"
    );
}

#[test]
fn push_preserves_not_planned_close_reason() {
    let mock = MockGitHub::start();
    mock.seed(5, "Bug", "Body.", "open");
    // Closed on GitHub as not-planned (a human's deliberate "won't do").
    mock.edit(5, |i| {
        i["state"] = json!("closed");
        i["closed_at"] = json!("2030-01-01T00:00:00Z");
        i["state_reason"] = json!("not_planned");
    });
    let dir = init_repo();
    sync(dir.path(), mock.addr, &[]); // pull-create the closed item
    let id = only_item_id(dir.path(), mock.addr);

    // A local-only edit (still closed) triggers a push. The push must preserve the
    // not_planned reason rather than resetting it to completed.
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Won't fix, renamed"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);

    assert_eq!(
        mock.state_reason(5).as_deref(),
        Some("not_planned"),
        "clove push must not reset a human's not_planned close reason"
    );
}

#[test]
fn labels_round_trip_push_and_pull() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    // Push a label up.
    clove(dir.path(), mock.addr)
        .args(["new", "Tagged", "--type", "bug", "--label", "area:core"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    assert_eq!(mock.label_names(1), vec!["area:core".to_owned()]);

    // A human adds a label on GitHub; a pull brings it down to the local item.
    mock.set_labels(1, &["area:core", "bug", "regression"]);
    let id = only_item_id(dir.path(), mock.addr);
    sync(dir.path(), mock.addr, &[]);
    let item = show(dir.path(), mock.addr, &id);
    let local: Vec<&str> = item["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect();
    assert!(
        local.contains(&"regression"),
        "pulled label missing: {local:?}"
    );
}

#[test]
fn type_and_deps_round_trip_through_clove_meta() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Parent", "--type", "bug"])
        .assert()
        .success();
    clove(dir.path(), mock.addr)
        .args(["new", "Dep target"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);

    // Find the two clove ids and the parent's GitHub number.
    let ids = all_item_ids(dir.path(), mock.addr);
    let mut parent: Option<(String, u64)> = None;
    let mut target_id = String::new();
    for id in &ids {
        let item = show(dir.path(), mock.addr, id);
        if item["title"] == "Parent" {
            let number = item["external_ref"]
                .as_str()
                .unwrap()
                .strip_prefix("gh-")
                .unwrap()
                .parse()
                .unwrap();
            parent = Some((id.clone(), number));
        } else {
            target_id = id.clone();
        }
    }
    let (parent_id, parent_gh) = parent.expect("parent pushed");

    // The push encoded the type into the clove-meta marker.
    let body = mock.issue(parent_gh).unwrap()["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("\"type\":\"bug\""), "{body}");

    // Remote-only edit: another clone changed the type and added a dep (both
    // ride the clove-meta marker). A pull must apply them.
    mock.edit(parent_gh, |i| {
        i["body"] = json!(format!(
            "Body.\n\n<!-- clove-meta: {{\"type\":\"chore\",\"deps\":[\"{target_id}\"]}} -->"
        ));
    });
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pulled_updated"], 1, "{v}");
    let item = show(dir.path(), mock.addr, &parent_id);
    assert_eq!(item["type"], "chore", "{item}");
    assert_eq!(item["deps"], json!([target_id]), "{item}");

    // A remote meta that owns an EMPTY dep set removes the dep locally too.
    mock.edit(parent_gh, |i| {
        i["body"] = json!("Body two.\n\n<!-- clove-meta: {\"type\":\"chore\"} -->");
    });
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pulled_updated"], 1, "{v}");
    let item = show(dir.path(), mock.addr, &parent_id);
    assert_eq!(item["deps"], json!([]), "{item}");
}

#[test]
fn close_state_round_trips() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    // Push a CLOSED item: created open, then closed in the same sync.
    clove(dir.path(), mock.addr)
        .args(["new", "Done already", "--type", "chore"])
        .assert()
        .success();
    let id = only_item_id(dir.path(), mock.addr);
    clove(dir.path(), mock.addr)
        .args(["close", &id])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    assert!(
        mock.is_closed(1),
        "a pushed closed item must end up closed on GitHub"
    );

    // Reopen on GitHub; a pull reopens the local item.
    mock.edit(1, |i| {
        i["state"] = json!("open");
        i["closed_at"] = Value::Null;
        i["state_reason"] = Value::Null;
    });
    sync(dir.path(), mock.addr, &[]);
    assert_eq!(
        show(dir.path(), mock.addr, &id)["status"],
        "open",
        "pull must reopen locally"
    );
}

#[test]
fn remote_missing_is_reported_not_fatal() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Linked", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1, link established
    let id = only_item_id(dir.path(), mock.addr);

    // The issue vanishes on GitHub (deleted); the next sync reports it as missing
    // and leaves the local item untouched (no crash, no re-create).
    mock.remove_issue(1);
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(
        v["data"]["remote_missing"].as_array().unwrap().len(),
        1,
        "{v}"
    );
    assert_eq!(v["data"]["remote_missing"][0], "gh-1", "{v}");
    assert_eq!(
        mock.issue_count(),
        0,
        "must not re-create the deleted issue"
    );
    assert_eq!(
        show(dir.path(), mock.addr, &id)["external_ref"],
        "gh-1",
        "local link preserved"
    );
}

#[test]
fn unassign_locally_clears_the_github_assignee() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Assigned", "--type", "bug", "--assignee", "alice"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1 assigned [alice]
    let id = only_item_id(dir.path(), mock.addr);
    assert_eq!(mock.assignee_logins(1), vec!["alice".to_owned()]);

    // Unassign locally; the next push must clear the assignee on GitHub.
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["assign", &id, "--clear"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    assert!(
        mock.assignee_logins(1).is_empty(),
        "unassigning locally must clear the GitHub assignee: {:?}",
        mock.assignee_logins(1)
    );
}

#[test]
fn concurrent_sync_is_rejected_while_locked() {
    // Hold the per-repo sync lock (as a daemon sync would), then a manual `clove
    // sync` of the same repo must fail cleanly rather than race into duplicate
    // issue creation.
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Local", "--type", "bug"])
        .assert()
        .success();

    let lock_path = dir.path().join(".clove/sync/github/owner_repo.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    let mut held = fd_lock::RwLock::new(file);
    let _guard = held.write().unwrap(); // hold the exclusive lock

    let out = clove(dir.path(), mock.addr)
        .args(["sync", "github", "owner/repo"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a locked sync must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("in progress"),
        "error should explain the lock: {stderr}"
    );
    // Nothing was pushed while the lock was held.
    assert_eq!(mock.issue_count(), 0, "locked sync must not push");
}

// ---------------------------------------------------------------------------
// Regression coverage for the sync-correctness defects.
// ---------------------------------------------------------------------------

/// All local item ids (any order).
fn all_item_ids(dir: &Path, addr: SocketAddr) -> Vec<String> {
    let out = clove(dir, addr)
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

/// C-sync-1: a remote-only edit must never demote a local `in_progress` to
/// `open` (GitHub has no `in_progress`; the pull only carries `open`).
#[test]
fn pull_update_preserves_local_in_progress() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1
    let id = only_item_id(dir.path(), mock.addr);

    // Mark it in_progress locally and sync so both sides are in sync again (the
    // GitHub issue stays "open" — there is no in_progress there).
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["start", &id])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]);
    assert_eq!(show(dir.path(), mock.addr, &id)["status"], "in_progress");

    // A remote-only edit triggers a pull_update. It must apply the new title but
    // NOT rewrite the status back to open.
    mock.edit(1, |i| i["title"] = json!("Edited remotely"));
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pulled_updated"], 1, "{v}");
    let item = show(dir.path(), mock.addr, &id);
    assert_eq!(item["title"], "Edited remotely");
    assert_eq!(
        item["status"], "in_progress",
        "a remote non-status edit must not demote in_progress to open"
    );
}

/// C-sync-2 (create+close variant): the fingerprint recorded after a push-create
/// of a *closed* item must reflect the close PATCH, not the superseded create
/// response — otherwise the next run sees a spurious remote change.
#[test]
fn create_and_close_records_close_fingerprint() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Done already", "--type", "chore"])
        .assert()
        .success();
    let id = only_item_id(dir.path(), mock.addr);
    clove(dir.path(), mock.addr)
        .args(["close", &id])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create then close (two updated_at bumps)
    assert!(mock.is_closed(1));

    // Nothing changed on either side; the pair must be in sync, not pull-updated.
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(
        v["data"]["pulled_updated"], 0,
        "create+close must not leave a stale fingerprint: {v}"
    );
    assert_eq!(v["data"]["in_sync"], 1, "{v}");
}

/// C-sync-2 (comment variant): pushing a comment bumps the issue's updated_at on
/// GitHub, but that self-inflicted bump must not read as a remote change and
/// cause a spurious pull_update on the next run.
#[test]
fn comment_push_does_not_trigger_spurious_pull() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug"])
        .assert()
        .success();
    let id = only_item_id(dir.path(), mock.addr);
    sync(dir.path(), mock.addr, &[]); // push-create gh-1
    clove(dir.path(), mock.addr)
        .args(["comment", &id, "A local comment"])
        .assert()
        .success();

    let v1 = sync(dir.path(), mock.addr, &[]); // pushes the comment (bumps issue updated_at)
    assert_eq!(v1["data"]["comments_pushed"], 1, "{v1}");

    let v2 = sync(dir.path(), mock.addr, &[]);
    assert_eq!(
        v2["data"]["pulled_updated"], 0,
        "a pushed comment must not masquerade as a remote change: {v2}"
    );
    assert_eq!(v2["data"]["in_sync"], 1, "{v2}");
}

/// C-sync-3: a mid-run failure (a comment POST exhausting its attempts) must
/// still persist the bookkeeping for actions already applied, so the next run
/// does not re-pull the already-pushed comment into a duplicate local comment.
#[test]
fn mid_run_failure_persists_comment_bookkeeping() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "First", "--type", "bug"])
        .assert()
        .success();
    clove(dir.path(), mock.addr)
        .args(["new", "Second", "--type", "bug"])
        .assert()
        .success();
    for id in all_item_ids(dir.path(), mock.addr) {
        clove(dir.path(), mock.addr)
            .args(["comment", &id, &format!("note for {id}")])
            .assert()
            .success();
    }

    // The comment push for gh-1 succeeds; the one for gh-2 fails, aborting the run
    // after gh-1's comment is already on GitHub.
    mock.set_fail_comment_post_for(Some(2));
    let out = sync_raw(dir.path(), mock.addr, &[]);
    assert!(
        !out.status.success(),
        "the mid-run comment failure must surface: {out:?}"
    );
    assert_eq!(
        mock.issue_count(),
        2,
        "both issues were created before the failure"
    );

    // Recover: the next run must NOT re-pull gh-1's already-pushed comment as a
    // duplicate local comment. Total local comments stays 2 (one per item).
    mock.set_fail_comment_post_for(None);
    let v = sync_raw(dir.path(), mock.addr, &[]);
    assert!(v.status.success(), "recovery sync should succeed: {v:?}");
    let total_local: usize = all_item_ids(dir.path(), mock.addr)
        .iter()
        .map(|id| local_comment_bodies(dir.path(), mock.addr, id).len())
        .sum();
    assert_eq!(
        total_local, 2,
        "already-pushed comment must not be re-pulled as a duplicate"
    );
}

/// C-sync-4: removing the last local label must propagate — the push has to send
/// an empty label set to clear it on GitHub, not skip the field.
#[test]
fn push_clears_last_local_label() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Tagged", "--type", "bug", "--label", "area:core"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1 with the label
    assert_eq!(mock.label_names(1), vec!["area:core".to_owned()]);
    let id = only_item_id(dir.path(), mock.addr);

    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["label", &id, "rm", "area:core"])
        .assert()
        .success();
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pushed_updated"], 1, "{v}");
    assert!(
        mock.label_names(1).is_empty(),
        "removing the last label must clear it on GitHub: {:?}",
        mock.label_names(1)
    );
}

/// C-sync-6 (non-idempotent create): a create whose response is lost must not be
/// retried — a retry would mint a duplicate issue.
#[test]
fn issue_create_is_not_retried_on_lost_response() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug"])
        .assert()
        .success();

    mock.arm_create_commit_then_fail(1);
    let out = sync_raw(dir.path(), mock.addr, &[]);
    assert!(
        !out.status.success(),
        "a lost-response create must surface, not silently retry: {out:?}"
    );
    assert_eq!(
        mock.issue_count(),
        1,
        "a non-idempotent create must not be retried into a duplicate"
    );
    assert_eq!(mock.issue_post_count(), 1, "create attempted exactly once");
}

/// C-sync-6 (permanent 4xx): an idempotent update that gets a permanent 4xx must
/// fail fast, not burn its full retry budget.
#[test]
fn idempotent_update_is_not_retried_on_4xx() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1 (open → no PATCH)
    let id = only_item_id(dir.path(), mock.addr);
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Renamed"])
        .assert()
        .success();

    mock.set_patch_fail_status(Some(422));
    let before = mock.patch_count();
    let out = sync_raw(dir.path(), mock.addr, &[]);
    assert!(!out.status.success(), "a 422 must surface: {out:?}");
    assert_eq!(
        mock.patch_count() - before,
        1,
        "a permanent 4xx must not be retried"
    );
}

/// C-sync-6 (guard): an idempotent update *is* retried through a transient 5xx.
#[test]
fn idempotent_update_retries_transient_5xx() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1
    let id = only_item_id(dir.path(), mock.addr);
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["set", &id, "title=Renamed"])
        .assert()
        .success();

    mock.arm_patch_transient_fails(2); // two 503s, then success
    let before = mock.patch_count();
    let v = sync(dir.path(), mock.addr, &[]);
    assert_eq!(v["data"]["pushed_updated"], 1, "{v}");
    assert_eq!(
        mock.patch_count() - before,
        3,
        "two transient failures then success = three attempts"
    );
    assert_eq!(mock.issue(1).unwrap()["title"], "Renamed");
}

/// C-sync-7: an unparseable local item file must abort the sync rather than being
/// silently dropped (which would pull-create its linked issue as a duplicate).
#[test]
fn unparseable_local_file_aborts_sync() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Good", "--type", "bug"])
        .assert()
        .success();

    // Drop a corrupt item file into the store.
    let bad = dir.path().join(".clove/issues/proj-BADBAD01.md");
    std::fs::write(&bad, "---\n: : not : valid : yaml\n---\nbody\n").unwrap();

    let out = sync_raw(dir.path(), mock.addr, &[]);
    assert!(
        !out.status.success(),
        "an unparseable local file must abort the sync: {out:?}"
    );
    assert_eq!(
        mock.issue_count(),
        0,
        "nothing must be pushed while a local file is unparseable"
    );
}

/// C-sync-8: a conflict skipped under `--prefer manual` must NOT advance the
/// assignee baseline. If it does, a later local-wins push treats the stale GitHub
/// assignee as a human-added extra and leaves both the old and new assignee.
#[test]
fn manual_skipped_conflict_does_not_restamp_assignee() {
    let mock = MockGitHub::start();
    let dir = init_repo();
    clove(dir.path(), mock.addr)
        .args(["new", "Task", "--type", "bug", "--assignee", "alice"])
        .assert()
        .success();
    sync(dir.path(), mock.addr, &[]); // push-create gh-1 assigned [alice]
    let id = only_item_id(dir.path(), mock.addr);
    assert_eq!(mock.assignee_logins(1), vec!["alice".to_owned()]);

    // Reassign locally to bob AND change the issue remotely → a both-sides conflict.
    std::thread::sleep(Duration::from_millis(1100));
    clove(dir.path(), mock.addr)
        .args(["assign", &id, "bob"])
        .assert()
        .success();
    mock.edit(1, |i| i["title"] = json!("Remote title change"));

    let v = sync(dir.path(), mock.addr, &["--prefer", "manual"]);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["data"]["conflicts"][0]["resolution"], "skipped", "{v}");
    assert_eq!(
        mock.assignee_logins(1),
        vec!["alice".to_owned()],
        "the skipped conflict pushed nothing"
    );

    // Resolve local-wins: the push replaces clove's assignee (alice → bob) and,
    // with a correct baseline, does not leave the stale alice behind.
    let v3 = sync(dir.path(), mock.addr, &["--prefer", "local"]);
    assert_eq!(
        v3["data"]["conflicts"][0]["resolution"], "local_wins",
        "{v3}"
    );
    let logins = mock.assignee_logins(1);
    assert!(
        logins.contains(&"bob".to_owned()),
        "bob must be assigned: {logins:?}"
    );
    assert!(
        !logins.contains(&"alice".to_owned()),
        "the stale assignee must not linger as a phantom human extra: {logins:?}"
    );
}
