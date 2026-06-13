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
//! Gated on the `github` feature (the default build), matching the sync command.
#![cfg(feature = "github")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
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
            next_number: 1,
            next_comment_id: 1000,
            clock: 0,
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
            "GET" => (
                "200 OK",
                Value::Array(s.comments.get(&number).cloned().unwrap_or_default()),
            ),
            "POST" => {
                let id = s.next_comment_id;
                s.next_comment_id += 1;
                let created = s.tick();
                let body_text = body["body"].as_str().unwrap_or("").to_owned();
                let comment = make_comment(id, "tester", &body_text, &created);
                s.comments.entry(number).or_default().push(comment.clone());
                ("201 Created", comment)
            }
            _ => ("404 Not Found", json!({ "message": "unsupported" })),
        };
    }

    match (method, number) {
        ("GET", None) => ("200 OK", Value::Array(s.issues.clone())),
        ("POST", None) => {
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
            ("201 Created", issue)
        }
        ("PATCH", Some(number)) => {
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

fn clove(dir: &Path, addr: SocketAddr) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    // A dummy token (the mock ignores it) and the API-base override that aims
    // octocrab at our server instead of github.com.
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
    let mut args = vec!["sync", "github", "owner/repo", "--format", "json"];
    args.extend_from_slice(extra);
    let out = clove(dir, addr).args(&args).output().unwrap();
    assert!(out.status.success(), "sync failed: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad json: {e}\n{out:?}"))
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
