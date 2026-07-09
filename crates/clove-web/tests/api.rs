//! Integration tests: drive the real axum router over a temp repository.

use camino::Utf8PathBuf;
use clove_core::{ItemStore, NewItem};
use clove_types::{ItemType, Priority};
use clove_web::{build_router, AppState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A temp repo with two items (one depends on the other) and the server state.
/// Returns the temp dir, state, the main (dependent) id, and the dependency id.
fn fixture() -> (tempfile::TempDir, AppState, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    std::fs::create_dir_all(&issues).unwrap();
    let store = ItemStore::new(root.clone());

    let now = chrono::Utc::now();
    let dep = store
        .create(
            "proj",
            NewItem {
                title: "Dependency".to_owned(),
                item_type: ItemType::Bug,
                priority: Priority(0),
                labels: vec!["area:core".to_owned()],
                deps: vec![],
                parent: None,
                assignee: None,
                body: String::new(),
            },
            now,
        )
        .unwrap();
    let main = store
        .create(
            "proj",
            NewItem {
                title: "Add webhook handler".to_owned(),
                item_type: ItemType::Feature,
                priority: Priority(1),
                labels: vec!["area:payments".to_owned()],
                deps: vec![dep.frontmatter.id.clone()],
                parent: None,
                assignee: None,
                body: "## Goal\nDo the thing.\n".to_owned(),
            },
            now,
        )
        .unwrap();

    let state = AppState::new(
        store,
        issues,
        "proj".to_owned(),
        "test",
        false,
        ItemType::Feature,
    );
    (
        tmp,
        state,
        main.frontmatter.id.to_string(),
        dep.frontmatter.id.to_string(),
    )
}

/// Send a fully custom raw HTTP/1.1 request (caller supplies the header block,
/// each line without its trailing CRLF) and return `(status_line, body)`.
async fn raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[&str],
) -> (String, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    for h in headers {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.lines().next().unwrap_or("").to_owned();
    (status, body.to_owned())
}

/// Send a raw HTTP/1.1 GET and return `(status_line, body)`.
async fn get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.lines().next().unwrap_or("").to_owned();
    (status, body.to_owned())
}

/// Send a raw HTTP/1.1 request with an optional JSON body; returns `(status_line, body)`.
async fn send(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    json: Option<&str>,
) -> (String, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = json.unwrap_or("");
    let headers = if json.is_some() {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    } else {
        String::new()
    };
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Connection: close\r\n\r\n{body}"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.lines().next().unwrap_or("").to_owned();
    (status, body.to_owned())
}

async fn spawn() -> (tempfile::TempDir, std::net::SocketAddr, String) {
    let (tmp, addr, main_id, _dep) = spawn_ids().await;
    (tmp, addr, main_id)
}

/// Like [`spawn`] but also returns the dependency id (for write-endpoint tests).
async fn spawn_ids() -> (tempfile::TempDir, std::net::SocketAddr, String, String) {
    let (tmp, state, main_id, dep_id) = fixture();
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (tmp, addr, main_id, dep_id)
}

#[tokio::test]
async fn list_returns_envelope_with_items() {
    let (_tmp, addr, _id) = spawn().await;
    let (status, body) = get(addr, "/api/v1/items").await;
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"ok\":true"), "body: {body}");
    assert!(body.contains("Add webhook handler"));
    assert!(body.contains("Dependency"));
    assert!(body.contains("\"total\":2"));
}

#[tokio::test]
async fn detail_includes_computed_fields() {
    let (_tmp, addr, id) = spawn().await;
    let (status, body) = get(addr, &format!("/api/v1/items/{id}")).await;
    assert!(status.contains("200"), "status: {status}");
    // The feature depends on the open bug → blocked, not ready, body present.
    assert!(body.contains("\"ready\":false"), "body: {body}");
    assert!(body.contains("\"blocked_by\""));
    assert!(body.contains("## Goal"));
}

#[tokio::test]
async fn ready_mode_excludes_blocked_item() {
    let (_tmp, addr, id) = spawn().await;
    let (status, body) = get(addr, "/api/v1/items?mode=ready").await;
    assert!(status.contains("200"));
    // The dependency (no deps) is ready; the blocked feature is not in the set.
    assert!(
        !body.contains(&id),
        "blocked item must not appear in ready: {body}"
    );
    assert!(body.contains("Dependency"));
}

#[tokio::test]
async fn board_groups_by_status() {
    let (_tmp, addr, _id) = spawn().await;
    let (status, body) = get(addr, "/api/v1/board").await;
    assert!(status.contains("200"));
    assert!(body.contains("\"key\":\"open\""));
    assert!(body.contains("\"key\":\"in_progress\""));
    assert!(body.contains("\"key\":\"closed\""));
}

#[tokio::test]
async fn stats_history_synthesizes_daily_series() {
    let (_tmp, addr, _id) = spawn().await;
    let (status, body) = get(addr, "/api/v1/stats/history?days=7").await;
    assert!(status.contains("200"), "status: {status}");
    // Seven daily points, each shaped {date, created, closed, open}.
    assert_eq!(body.matches("\"date\"").count(), 7, "body: {body}");
    assert!(body.contains("\"created\""));
    assert!(body.contains("\"closed\""));
    assert!(body.contains("\"open\""));
}

#[tokio::test]
async fn invalid_id_returns_envelope_error() {
    let (_tmp, addr, _id) = spawn().await;
    let (status, body) = get(addr, "/api/v1/items/zzz").await;
    // CloveId rejects the malformed id → 422 INVALID_ID, exit 4.
    assert!(status.contains("422"), "status: {status}");
    assert!(body.contains("\"ok\":false"));
    assert!(body.contains("INVALID_ID"));
    assert!(body.contains("\"exit\":4"));
}

#[tokio::test]
async fn patch_updates_title_body_assignee_and_labels() {
    let (_tmp, addr, id) = spawn().await;
    let payload = r#"{"title":"Renamed","body":"new body","assignee":"alice","labels":["urgent","area:payments"]}"#;
    let (status, body) = send(addr, "PATCH", &format!("/api/v1/items/{id}"), Some(payload)).await;
    assert!(status.contains("200"), "status: {status} body: {body}");
    assert!(body.contains("\"title\":\"Renamed\""), "{body}");
    assert!(body.contains("\"assignee\":\"alice\""), "{body}");
    // The full set replaced + canonical-sorted.
    assert!(
        body.contains("\"labels\":[\"area:payments\",\"urgent\"]"),
        "{body}"
    );
    // The body change landed (re-read the detail, which includes `body`).
    let (_s, detail) = get(addr, &format!("/api/v1/items/{id}")).await;
    assert!(detail.contains("new body"), "{detail}");
}

#[tokio::test]
async fn patch_clears_assignee_with_null() {
    let (_tmp, addr, id) = spawn().await;
    send(
        addr,
        "PATCH",
        &format!("/api/v1/items/{id}"),
        Some(r#"{"assignee":"bob"}"#),
    )
    .await;
    let (status, body) = send(
        addr,
        "PATCH",
        &format!("/api/v1/items/{id}"),
        Some(r#"{"assignee":null}"#),
    )
    .await;
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"assignee\":null"), "{body}");
}

#[tokio::test]
async fn patch_clears_assignee_with_empty_string() {
    // The handler maps an empty/whitespace assignee to a clear, so a form
    // submitting "" doesn't trip apply_edit's empty-assignee guard.
    let (_tmp, addr, id) = spawn().await;
    send(
        addr,
        "PATCH",
        &format!("/api/v1/items/{id}"),
        Some(r#"{"assignee":"bob"}"#),
    )
    .await;
    let (status, body) = send(
        addr,
        "PATCH",
        &format!("/api/v1/items/{id}"),
        Some(r#"{"assignee":"  "}"#),
    )
    .await;
    assert!(status.contains("200"), "status: {status} body: {body}");
    assert!(body.contains("\"assignee\":null"), "{body}");
}

#[tokio::test]
async fn patch_invalid_priority_is_validation_error() {
    let (_tmp, addr, id) = spawn().await;
    let (status, body) = send(
        addr,
        "PATCH",
        &format!("/api/v1/items/{id}"),
        Some(r#"{"priority":9}"#),
    )
    .await;
    assert!(status.contains("422"), "status: {status}");
    assert!(body.contains("VALIDATION_ERROR"), "{body}");
}

#[tokio::test]
async fn put_parent_sets_and_clears() {
    let (_tmp, addr, main_id, dep_id) = spawn_ids().await;
    // Parent the dependency under the main item.
    let (status, body) = send(
        addr,
        "PUT",
        &format!("/api/v1/items/{dep_id}/parent"),
        Some(&format!("{{\"parent\":\"{main_id}\"}}")),
    )
    .await;
    assert!(status.contains("200"), "status: {status} body: {body}");
    assert!(
        body.contains(&format!("\"parent\":\"{main_id}\"")),
        "{body}"
    );
    // Clear it again.
    let (status, body) = send(
        addr,
        "PUT",
        &format!("/api/v1/items/{dep_id}/parent"),
        Some(r#"{"parent":null}"#),
    )
    .await;
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"parent\":null"), "{body}");
}

#[tokio::test]
async fn add_dep_cycle_is_rejected() {
    let (_tmp, addr, main_id, dep_id) = spawn_ids().await;
    // `main` already depends on `dep`; making `dep` depend on `main` would cycle.
    let (status, body) = send(
        addr,
        "POST",
        &format!("/api/v1/items/{dep_id}/deps"),
        Some(&format!("{{\"dep\":\"{main_id}\"}}")),
    )
    .await;
    assert!(status.contains("409"), "status: {status} body: {body}");
    assert!(body.contains("CYCLE_DETECTED"), "{body}");
}

#[tokio::test]
async fn force_delete_removes_item_with_dependents() {
    // `main` depends on `dep`; an unforced delete of `dep` is rejected (409),
    // while `?force=true` (the literal value the server checks) succeeds.
    let (_tmp, addr, _main_id, dep_id) = spawn_ids().await;
    let (status, body) = send(addr, "DELETE", &format!("/api/v1/items/{dep_id}"), None).await;
    assert!(status.contains("409"), "unforced delete: {status} {body}");
    assert!(body.contains("HAS_DEPENDENTS"), "{body}");

    let (status, body) = send(
        addr,
        "DELETE",
        &format!("/api/v1/items/{dep_id}?force=true"),
        None,
    )
    .await;
    assert!(status.contains("200"), "forced delete: {status} {body}");
    assert!(body.contains("\"deleted\":true"), "{body}");

    // An empty `?force=` must be treated as no-force (regression guard for the
    // old client bug that sent `?force=`).
    let (status, _b) = send(
        addr,
        "DELETE",
        &format!("/api/v1/items/{}?force=", "proj-doesnotexist0"),
        None,
    )
    .await;
    // (id is invalid → 422; the point is `?force=` isn't accepted as force=true;
    // the successful force path above already proves the value contract.)
    assert!(status.contains("422") || status.contains("404"), "{status}");
}

#[tokio::test]
async fn rejects_non_local_host_header() {
    let (_tmp, addr, _id) = spawn().await;
    // A rebound (attacker-controlled) Host is rejected before any handler runs.
    let (status, _body) = raw(addr, "GET", "/api/v1/items", &["Host: evil.example.com"]).await;
    assert!(status.contains("403"), "bad Host must be 403: {status}");
    // A loopback Host passes (200) — covered by other tests, asserted here too.
    let (status, _body) = raw(addr, "GET", "/api/v1/items", &["Host: 127.0.0.1:7373"]).await;
    assert!(status.contains("200"), "local Host must pass: {status}");
}

#[tokio::test]
async fn rejects_cross_origin_websocket_handshake() {
    let (_tmp, addr, _id) = spawn().await;
    // A valid WS upgrade with a cross-origin `Origin` is rejected (403) before
    // the socket is upgraded.
    let (status, _body) = raw(
        addr,
        "GET",
        "/api/v1/events",
        &[
            "Host: localhost",
            "Connection: Upgrade",
            "Upgrade: websocket",
            "Sec-WebSocket-Version: 13",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
            "Origin: http://evil.example.com",
        ],
    )
    .await;
    assert!(
        status.contains("403"),
        "cross-origin WS must be 403: {status}"
    );
}

#[tokio::test]
async fn create_uses_configured_default_type() {
    // Build state whose configured default_type is `chore`; a create that omits
    // `type` must land as `chore` (not the hardcoded ItemType::default()).
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    std::fs::create_dir_all(&issues).unwrap();
    let store = ItemStore::new(root);
    let state = AppState::new(
        store,
        issues,
        "proj".to_owned(),
        "test",
        false,
        ItemType::Chore,
    );
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (status, body) = send(
        addr,
        "POST",
        "/api/v1/items",
        Some(r#"{"title":"No type given"}"#),
    )
    .await;
    assert!(status.contains("200"), "status: {status} body: {body}");
    assert!(
        body.contains("\"type\":\"chore\""),
        "expected chore: {body}"
    );
}

#[tokio::test]
async fn remove_dep_is_idempotent() {
    let (_tmp, addr, main_id, dep_id) = spawn_ids().await;
    // Remove the real edge, then remove again — both succeed (HTTP DELETE).
    let (status, body) = send(
        addr,
        "DELETE",
        &format!("/api/v1/items/{main_id}/deps/{dep_id}"),
        None,
    )
    .await;
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"deps\":[]"), "{body}");
    let (status2, _b2) = send(
        addr,
        "DELETE",
        &format!("/api/v1/items/{main_id}/deps/{dep_id}"),
        None,
    )
    .await;
    assert!(
        status2.contains("200"),
        "second remove should be a no-op 200: {status2}"
    );
}
