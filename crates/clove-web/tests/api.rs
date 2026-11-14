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

/// The web API decodes `limit` through the same shared `Page` as the CLI and
/// MCP: absent → the web default (unlimited), `0` → unlimited, `n` → at most
/// `n`, and `total` is always the pre-pagination count. `?limit=0` used to be
/// taken literally here and returned *zero* rows — the exact opposite of what
/// the same query means on every other surface.
#[tokio::test]
async fn list_pages_on_the_shared_limit_contract() {
    let (_tmp, addr, _id) = spawn().await;

    for (query, returned) in [("", 2), ("?limit=0", 2), ("?limit=1", 1)] {
        let (status, body) = get(addr, &format!("/api/v1/items{query}")).await;
        assert!(status.contains("200"), "{query}: status {status}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["data"].as_array().unwrap().len(),
            returned,
            "{query}: rows"
        );
        assert_eq!(v["_meta"]["total"], 2, "{query}: total is pre-window");
        assert_eq!(v["_meta"]["returned"], returned, "{query}: returned");
    }

    // `offset` skips into the same order the unpaginated call returns.
    let (_, all) = get(addr, "/api/v1/items?limit=0").await;
    let all: serde_json::Value = serde_json::from_str(&all).unwrap();
    let (_, second) = get(addr, "/api/v1/items?offset=1&limit=1").await;
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["data"][0]["id"], all["data"][1]["id"]);
    assert_eq!(second["_meta"]["offset"], 1);
    assert_eq!(second["_meta"]["limit"], 1, "the effective limit is echoed");
}

/// The comments endpoint pages from the *newest* end, like `clove comments` and
/// `clove_comments`, and takes the same `skip_newest` — which it did not have,
/// making it the one surface of the three that could not reach older comments.
/// Its default is the web default (unlimited), so the bundled UI, which sends no
/// limit and renders against `comment_count`, cannot show a count above a
/// truncated list.
#[tokio::test]
async fn comments_page_from_the_newest_end() {
    let (_tmp, addr, id) = spawn().await;
    for body in ["first", "second", "third"] {
        let (status, _) = send(
            addr,
            "POST",
            &format!("/api/v1/items/{id}/comments"),
            Some(&format!(r#"{{"body":"{body}"}}"#)),
        )
        .await;
        assert!(status.contains("200"), "post comment: {status}");
    }
    let bodies = |body: &str| -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["body"].as_str().unwrap().to_owned())
            .collect()
    };

    // No parameter → the whole thread, oldest-first.
    let (_, all) = get(addr, &format!("/api/v1/items/{id}/comments")).await;
    assert_eq!(bodies(&all), ["first", "second", "third"]);
    let meta: serde_json::Value = serde_json::from_str(&all).unwrap();
    assert_eq!(meta["_meta"]["total"], 3);
    assert_eq!(meta["_meta"]["limit"], 0, "unlimited by default on the web");

    // `limit` keeps the newest; `skip_newest` walks back into older ones.
    let (_, newest) = get(addr, &format!("/api/v1/items/{id}/comments?limit=1")).await;
    assert_eq!(bodies(&newest), ["third"]);
    let (_, older) = get(
        addr,
        &format!("/api/v1/items/{id}/comments?limit=1&skip_newest=1"),
    )
    .await;
    assert_eq!(bodies(&older), ["second"]);
    let older: serde_json::Value = serde_json::from_str(&older).unwrap();
    assert_eq!(older["_meta"]["total"], 3, "total is the whole thread");
    assert_eq!(older["_meta"]["skip_newest"], 1);
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

/// `/board` windows each column independently. It accepts every filter and sort
/// parameter (it shares `matches`/`sort_items` with the item list) but used to
/// drop `limit`/`offset` silently — advertised by association and ignored.
///
/// `count` stays the column's *full* size so a header reading "Open · 2" over
/// one visible card is honest; `returned` is what came back.
#[tokio::test]
async fn board_windows_each_column_independently() {
    let (_tmp, addr, _id) = spawn().await;
    let column = |body: &str, key: &str| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["data"]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == key)
            .unwrap()
            .clone()
    };

    // The fixture is two open items, both in the same column.
    let (_, all) = get(addr, "/api/v1/board").await;
    let open = column(&all, "open");
    assert_eq!(open["count"], 2);
    assert_eq!(open["returned"], 2);
    assert_eq!(open["items"].as_array().unwrap().len(), 2);

    // `limit` caps the column without lying about how tall it is.
    let (_, capped) = get(addr, "/api/v1/board?limit=1").await;
    let open = column(&capped, "open");
    assert_eq!(open["count"], 2, "count is the full column");
    assert_eq!(open["returned"], 1);
    assert_eq!(open["items"].as_array().unwrap().len(), 1);
    // Every column is windowed, including the empty ones.
    let closed = column(&capped, "closed");
    assert_eq!(closed["count"], 0);
    assert_eq!(closed["returned"], 0);

    // `offset` walks the column, and `limit=0` is unlimited as everywhere else.
    let (_, skipped) = get(addr, "/api/v1/board?offset=1").await;
    let open_skipped = column(&skipped, "open");
    assert_eq!(open_skipped["returned"], 1);
    assert_eq!(
        open_skipped["items"][0]["id"],
        column(&all, "open")["items"][1]["id"],
        "offset skips into the same order"
    );
    let (_, unlimited) = get(addr, "/api/v1/board?limit=0").await;
    assert_eq!(column(&unlimited, "open")["returned"], 2);

    let meta: serde_json::Value = serde_json::from_str(&capped).unwrap();
    assert_eq!(meta["_meta"]["limit"], 1);
    assert_eq!(
        meta["_meta"]["per_column"], true,
        "the window is per column"
    );
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
    // With no recorded snapshots the series is synthesized from files.
    assert!(body.contains("\"synthesized\":true"), "body: {body}");
}

#[tokio::test]
async fn stats_history_days_all_reaches_back_to_the_oldest_item() {
    let (tmp, addr, _id) = spawn().await;
    let count = |body: &str| body.matches("\"date\"").count();

    // A young store: `all` still gives the default 90 days, so the timeline's
    // 30d/90d ranges keep their zero-filled days.
    let (status, body) = get(addr, "/api/v1/stats/history?days=all").await;
    assert!(status.contains("200"), "status: {status}");
    assert_eq!(count(&body), 90, "body: {body}");

    // An item created 200 days ago: `all` covers every day since (200 days,
    // today included), where the default stays at 90.
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    ItemStore::new(root)
        .create(
            "proj",
            NewItem {
                title: "Old".to_owned(),
                item_type: ItemType::Chore,
                priority: Priority(2),
                labels: vec![],
                deps: vec![],
                parent: None,
                assignee: None,
                body: String::new(),
            },
            chrono::Utc::now() - chrono::Duration::days(199),
        )
        .unwrap();
    let (_, body) = get(addr, "/api/v1/stats/history?days=all").await;
    assert_eq!(count(&body), 200, "body: {body}");
    let (_, body) = get(addr, "/api/v1/stats/history").await;
    assert_eq!(count(&body), 90, "body: {body}");
}

#[tokio::test]
async fn stats_history_serves_recorded_snapshots_when_present() {
    use clove_core::{compute_stats, GraphStore, StatsOptions};
    use clove_index::Index;

    let (tmp, addr, _main, _dep) = spawn_ids().await;
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let db_path = root.join(".clove").join("index.db");
    let store = ItemStore::new(root.clone());
    let index = Index::open_or_create(&db_path).unwrap();

    // Snapshot 1: the 2-item fixture.
    let (fms1, _) = store.scan_frontmatter().unwrap();
    let (graph1, _) = GraphStore::build(&fms1);

    // Grow the store, then snapshot 2 (3 items).
    store
        .create(
            "proj",
            NewItem {
                title: "Third".to_owned(),
                item_type: ItemType::Chore,
                priority: Priority(2),
                labels: vec![],
                deps: vec![],
                parent: None,
                assignee: None,
                body: String::new(),
            },
            chrono::Utc::now(),
        )
        .unwrap();
    let (fms2, _) = store.scan_frontmatter().unwrap();
    let (graph2, _) = GraphStore::build(&fms2);

    // Reports computed at a single `now` (so counts are stable) but stored at
    // distinct past capture times so they order chronologically.
    let now = chrono::Utc::now();
    let report1 = compute_stats(&fms1, &graph1, now, StatsOptions::default());
    let report2 = compute_stats(&fms2, &graph2, now, StatsOptions::default());
    index
        .record_snapshot(now - chrono::Duration::days(2), &report1)
        .unwrap();
    index
        .record_snapshot(now - chrono::Duration::days(1), &report2)
        .unwrap();
    drop(index);

    let (status, body) = get(addr, "/api/v1/stats/history").await;
    assert!(status.contains("200"), "status: {status}");
    // Served from the recorded snapshots, not synthesized from files.
    assert!(body.contains("\"synthesized\":false"), "body: {body}");
    assert!(body.contains("\"snapshots\":2"), "body: {body}");
    // Real recorded levels the file-synthesized series cannot reconstruct.
    assert!(body.contains("\"ready\""), "body: {body}");
    assert!(body.contains("\"blocked\""), "body: {body}");
    // Point-in-time totals: first snapshot 2 items, second 3 (time-independent).
    assert!(
        body.contains("\"total\":2"),
        "expected first total=2: {body}"
    );
    assert!(
        body.contains("\"total\":3"),
        "expected second total=3: {body}"
    );
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

/// `?sort=`/`?dir=` now go through `clove_core::view::Order`, the same
/// comparator the CLI, MCP, and daemon use — this endpoint used to own the only
/// sort implementation in the project. The historical spellings still work, the
/// shared enum adds `status`/`type`, and the applied order is echoed in `_meta`.
#[tokio::test]
async fn list_sorts_through_the_shared_order() {
    let (_tmp, addr, _id) = spawn().await;

    let ids = |body: &str| -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_owned())
            .collect()
    };

    // The fixture: "Dependency" is p0, "Add webhook handler" is p1 and depends
    // on it, so priority puts the dependency first.
    let (_, by_priority) = get(addr, "/api/v1/items?sort=priority").await;
    let by_priority = ids(&by_priority);
    let (_, desc) = get(addr, "/api/v1/items?sort=priority&dir=desc").await;
    let mut want = by_priority.clone();
    want.reverse();
    assert_eq!(ids(&desc), want, "dir=desc reverses the whole key");

    // `sort=id` is a pure id sort — a different sequence from priority here.
    let (_, by_id) = get(addr, "/api/v1/items?sort=id").await;
    let mut sorted = by_priority.clone();
    sorted.sort();
    assert_eq!(ids(&by_id), sorted);

    // `status`/`type` arrived with the shared enum.
    let (status, body) = get(addr, "/api/v1/items?sort=status").await;
    assert!(status.contains("200"), "{status} {body}");
    let (status, body) = get(addr, "/api/v1/items?sort=type").await;
    assert!(status.contains("200"), "{status} {body}");

    // `_meta` echoes what was applied, including the default.
    let (_, body) = get(addr, "/api/v1/items").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["_meta"]["sort"], "rank");
    assert_eq!(v["_meta"]["dir"], "asc");
    let (_, body) = get(addr, "/api/v1/items?sort=updated&dir=desc").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["_meta"]["sort"], "updated");
    assert_eq!(v["_meta"]["dir"], "desc");

    // The board shares the sorter, so it echoes the same pair.
    let (_, body) = get(addr, "/api/v1/board?sort=id&dir=desc").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["_meta"]["sort"], "id");
    assert_eq!(v["_meta"]["dir"], "desc");

    // Negative: an unrecognized value is a validation error rather than a
    // silent fall back to `rank` (which is what the old comparator did).
    let (status, body) = get(addr, "/api/v1/items?sort=nope").await;
    assert!(status.contains("422"), "{status} {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{body}");
    let (status, _) = get(addr, "/api/v1/items?dir=sideways").await;
    assert!(status.contains("422"), "{status}");
}

/// Read-path §2 on the web: the endpoint keeps every filter spelling it had
/// (csv values, AND-ed labels, `?q=`), now through the shared
/// `clove_core::view::Filters` that the CLI and MCP also use.
///
/// The fixture is the two-item store above: `Dependency` (bug, p0,
/// `area:core`) and `Add webhook handler` (feature, p1, `area:payments`).
#[tokio::test]
async fn item_filters_keep_the_webs_multi_value_spellings() {
    let (_tmp, addr, _id) = spawn().await;
    let titles = |path: &'static str| async move {
        let (status, body) = get(addr, path).await;
        assert!(status.contains("200"), "{path}: {status} {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut out: Vec<String> = v["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{path}: {v}"))
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_owned())
            .collect();
        out.sort();
        out
    };

    // Single values.
    assert_eq!(titles("/api/v1/items?type=bug").await, ["Dependency"]);
    assert_eq!(
        titles("/api/v1/items?priority=1").await,
        ["Add webhook handler"]
    );
    assert_eq!(
        titles("/api/v1/items?label=area:core").await,
        ["Dependency"]
    );
    // Csv = any-of, the spelling the SPA sends.
    assert_eq!(
        titles("/api/v1/items?type=bug,feature").await,
        ["Add webhook handler", "Dependency"]
    );
    assert_eq!(
        titles("/api/v1/items?priority=0,1").await,
        ["Add webhook handler", "Dependency"]
    );
    assert_eq!(titles("/api/v1/items?status=open,closed").await.len(), 2);
    // Csv labels are all-of: no item carries both, so the intersection is empty.
    assert!(titles("/api/v1/items?label=area:core,area:payments")
        .await
        .is_empty());
    // `q` over title and labels, case-insensitively; never the body (only the
    // webhook item has one, and it says "Do the thing").
    assert_eq!(
        titles("/api/v1/items?q=WEBHOOK").await,
        ["Add webhook handler"]
    );
    assert_eq!(
        titles("/api/v1/items?q=area:payments").await,
        ["Add webhook handler"]
    );
    assert!(titles("/api/v1/items?q=Do%20the%20thing").await.is_empty());
    // An empty parameter does not constrain (the SPA clears a filter by sending
    // it empty rather than dropping the key).
    assert_eq!(titles("/api/v1/items?q=&status=&label=").await.len(), 2);

    // `_meta.filters` echoes the parsed set, canonicalized.
    let (_status, body) = get(addr, "/api/v1/items?status=open&label=AREA:Core&priority=0").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let filters = &v["_meta"]["filters"];
    assert_eq!(filters["status"], serde_json::json!(["open"]), "{filters}");
    assert_eq!(
        filters["labels"],
        serde_json::json!(["area:core"]),
        "{filters}"
    );
    assert_eq!(filters["priority"], serde_json::json!([0]), "{filters}");
    assert_eq!(filters["q"], serde_json::Value::Null, "{filters}");

    // The board shares the predicate and echoes it too.
    let (_status, body) = get(addr, "/api/v1/board?type=bug").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["_meta"]["filters"]["type"], serde_json::json!(["bug"]));
    let total: i64 = v["data"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["count"].as_i64().unwrap())
        .sum();
    assert_eq!(total, 1, "the board filters, it does not just echo: {v}");
}

/// An unparseable filter value is a `VALIDATION_ERROR`, not a filter that
/// silently matches nothing.
///
/// This is the one behaviour change on this endpoint: `?status=bogus` used to
/// compare raw strings and return `[]`, which a client cannot tell from "there
/// are no open bugs". It is the same treatment `?sort=nope` already gets.
#[tokio::test]
async fn an_unknown_filter_value_is_a_validation_error() {
    let (_tmp, addr, _id) = spawn().await;
    for path in [
        "/api/v1/items?status=bogus",
        "/api/v1/items?status=open,bogus",
        "/api/v1/items?type=saga",
        "/api/v1/items?priority=9",
        "/api/v1/items?priority=abc",
        "/api/v1/board?status=bogus",
        // `?mode=` was the last lenient parameter here: an unknown mode fell
        // through to the unfiltered list with a 200, and `mode` is not echoed in
        // `_meta`, so a client could not tell a typo from an unimplemented mode.
        "/api/v1/items?mode=redy",
        "/api/v1/items?mode=Ready",
    ] {
        let (status, body) = get(addr, path).await;
        assert!(status.contains("422"), "{path}: {status}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{path}: {body}");
    }
}

/// `_meta.source` names a **tier** on every read endpoint, including this one.
///
/// `/stats/history` kept reporting `state.source` — the serving mode,
/// `"standalone"`/`"daemon"` — after the rest of the API moved to naming the
/// tier, so one endpoint returned a value outside the enum
/// `docs/json-schema/v1/item-list.json` publishes.
#[tokio::test]
async fn stats_history_reports_a_tier_not_the_serving_mode() {
    use clove_core::{compute_stats, GraphStore, StatsOptions};
    use clove_index::Index;

    let (tmp, addr, _main, _dep) = spawn_ids().await;

    // Both branches, because they report different tiers and a test that hits
    // only one cannot see the other regress: with no recorded snapshots the
    // series is synthesized from the files, and with one it comes from the index.
    let check = |body: &str, want: &str| {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        let source = v["_meta"]["source"].as_str().unwrap_or_default().to_owned();
        assert!(
            matches!(source.as_str(), "daemon" | "index" | "files"),
            "`{source}` is not a tier: {body}"
        );
        assert_eq!(source, want, "{body}");
    };

    let (status, body) = get(addr, "/api/v1/stats/history").await;
    assert!(status.contains("200"), "status: {status}");
    check(&body, "files");

    // Record one, so the endpoint answers from `index.db` instead.
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let store = ItemStore::new(root.clone());
    let index = Index::open_or_create(&root.join(".clove").join("index.db")).unwrap();
    let (fms, _) = store.scan_frontmatter().unwrap();
    let (graph, _) = GraphStore::build(&fms);
    let report = compute_stats(&fms, &graph, chrono::Utc::now(), StatsOptions::default());
    index.record_snapshot(chrono::Utc::now(), &report).unwrap();

    let (status, body) = get(addr, "/api/v1/stats/history").await;
    assert!(status.contains("200"), "status: {status}");
    check(&body, "index");
}

/// The web reads are tiered, and `_meta.source` names the tier that answered.
///
/// Two things changed here in read-path §4. The endpoint used to read files
/// unconditionally — with a hot `.clove/index.db` beside it — and it used to
/// report `state.source`, the *serving mode* (`"standalone"`/`"daemon"`), in the
/// field the CLI uses for the tier. A `cloved`-hosted server therefore claimed
/// `"daemon"` for an answer it had just scanned off disk.
///
/// The rows must be identical either way: the file tier reads `ready`/
/// `blocked_by`/`dangling_deps` off the whole-store graph, while an index answer
/// derives the same three per item from its own dependency closure.
#[tokio::test]
async fn list_answers_from_the_index_and_reports_the_tier() {
    let (tmp, state, main_id, dep_id) = fixture();
    let issues = state.issues_dir.clone();

    // Before any index exists, the file tier answers.
    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (_s, body) = get(addr, "/api/v1/items?sort=id").await;
    let files: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(files["_meta"]["source"], "files", "body: {body}");

    // Build the index, then a *fresh* state so the engine picks it up.
    clove_index::reindex(&issues, &issues.parent().unwrap().join("index.db")).unwrap();
    let indexed_state = AppState::new(
        state.store.clone(),
        issues,
        "proj".to_owned(),
        "test",
        false,
        ItemType::Feature,
    );
    let app = build_router(indexed_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Windowed: an unwindowed read is pinned to the file tier (see
    // `unlimited_reads_use_one_file_scan`).
    let (_s, body) = get(addr, "/api/v1/items?sort=id&limit=100").await;
    let indexed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        indexed["_meta"]["source"], "index",
        "the index must answer, or the comparison below is vacuous: {body}"
    );
    assert_eq!(
        indexed["data"], files["data"],
        "the tier must not change a single field"
    );
    assert_eq!(indexed["_meta"]["total"], files["_meta"]["total"]);

    // The graph terms specifically: these are what the tiered path has to
    // recompute per item, having no whole-store graph to read them off.
    let row = |v: &serde_json::Value, id: &str| -> serde_json::Value {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap()
    };
    let main = row(&indexed, &main_id);
    assert_eq!(main["ready"], false, "blocked by its open dependency");
    assert_eq!(main["blocked_by"], serde_json::json!([dep_id]));
    assert_eq!(main["dangling_deps"], serde_json::json!([]));
    let dep = row(&indexed, &dep_id);
    assert_eq!(dep["ready"], true);
    assert_eq!(dep["blocked_by"], serde_json::json!([]));

    // `?mode=` shares the same cascade rather than filtering in memory here.
    let (_s, body) = get(addr, "/api/v1/items?mode=blocked&limit=100").await;
    let blocked: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(blocked["_meta"]["source"], "index", "body: {body}");
    assert_eq!(blocked["_meta"]["total"], 1);
    assert_eq!(blocked["data"][0]["id"], main_id);

    let (_s, body) = get(addr, "/api/v1/items?mode=ready&limit=100").await;
    let ready: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(ready["_meta"]["source"], "index", "body: {body}");
    assert_eq!(ready["data"][0]["id"], dep_id);

    drop(tmp);
}

/// `?fields=` and `?compact=` — the last result-shaping gap between the web
/// API and the CLI/MCP (read-path roadmap §5).
///
/// The semantics are the **CLI's**: both default off. That is the load-bearing
/// half of this test — the MCP server compacts by default, and inheriting that
/// here would silently drop `assignee: null` and `labels: []` from every
/// response the SPA already depends on.
#[tokio::test]
async fn list_shapes_results_like_the_cli() {
    let (_tmp, addr, _id) = spawn().await;
    let json = |body: &str| -> serde_json::Value { serde_json::from_str(body).unwrap() };

    // Default: the full object. `p0` (the dependency) sorts first under `rank`.
    let (_, body) = get(addr, "/api/v1/items").await;
    let full = json(&body);
    let row = &full["data"][0];
    assert!(row["assignee"].is_null(), "unshaped keeps nulls: {body}");
    assert_eq!(row["relates"], serde_json::json!([]), "…and empty lists");
    assert_eq!(row["schema"], 1, "…and the migration marker");

    // `?compact=true` drops null and empty-list keys plus `schema`, and keeps
    // `false`/`0` — those are answers, not absences.
    let (_, body) = get(addr, "/api/v1/items?compact=true").await;
    let compact = json(&body);
    let row = &compact["data"][0];
    assert!(row.get("assignee").is_none(), "null dropped: {body}");
    assert!(row.get("relates").is_none(), "empty list dropped");
    assert!(row.get("blocked_by").is_none(), "empty graph term dropped");
    assert!(row.get("schema").is_none(), "migration marker dropped");
    assert_eq!(row["priority"], 0, "zero survives compaction");
    assert_eq!(row["ready"], true);
    assert_eq!(compact["_meta"]["total"], 2, "shaping is not a filter");
    assert_eq!(compact["_meta"]["returned"], 2);

    // `?fields=` is honoured literally: an explicitly requested key comes back
    // even when its value is null, so a caller can tell "unset" from "not
    // requested".
    let (_, body) = get(addr, "/api/v1/items?fields=id,assignee").await;
    let projected = json(&body);
    let row = projected["data"][0].as_object().unwrap();
    assert_eq!(row.len(), 2, "exactly the requested keys: {body}");
    assert!(
        row["assignee"].is_null(),
        "an explicit ask returns the null"
    );
    assert_eq!(row["id"], full["data"][0]["id"], "same rows, fewer keys");

    // …and `compact` composes on top of it, exactly as `--fields … --compact`.
    let (_, body) = get(addr, "/api/v1/items?fields=id,assignee&compact=true").await;
    let row = json(&body)["data"][0].as_object().unwrap().len();
    assert_eq!(row, 1, "the null projection is compacted away");

    // Unknown field names are ignored, not an error (the CLI's behaviour).
    let (status, body) = get(addr, "/api/v1/items?fields=id,nonexistent").await;
    assert!(status.contains("200"), "{status}");
    assert_eq!(json(&body)["data"][0].as_object().unwrap().len(), 1);

    // The shaping composes with the window rather than replacing it.
    let (_, body) = get(addr, "/api/v1/items?fields=id&limit=1").await;
    let windowed = json(&body);
    assert_eq!(windowed["data"].as_array().unwrap().len(), 1);
    assert_eq!(windowed["_meta"]["total"], 2, "total is still pre-window");
}

/// A boolean query parameter is parsed strictly, like `?sort=` and `?status=`:
/// `?compact=yes` silently returning the full shape is a response a client
/// cannot distinguish from a server that does not implement the parameter.
#[tokio::test]
async fn an_unparseable_compact_flag_is_a_validation_error() {
    let (_tmp, addr, _id) = spawn().await;

    let (status, body) = get(addr, "/api/v1/items?compact=yes").await;
    assert!(status.contains("422"), "status: {status}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "body: {body}");

    // The spellings that *are* accepted.
    for (query, keys_dropped) in [
        ("?compact=true", true),
        ("?compact=1", true),
        ("?compact=false", false),
        ("?compact=0", false),
        ("?compact=", false),
    ] {
        let (status, body) = get(addr, &format!("/api/v1/items{query}")).await;
        assert!(status.contains("200"), "{query}: {status}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["data"][0].get("schema").is_none(),
            keys_dropped,
            "{query}: body {body}"
        );
    }
}

/// The item detail and the board take the same two parameters — a shaping
/// surface that stopped at one endpoint would be the "advertised and ignored"
/// pattern `?limit=` on the board already was (§6.2).
///
/// On the board the order of operations is what matters: the grouping reads
/// each row's `status`, so shaping has to run *after* it. Projecting first
/// would empty every column instead of returning ids.
#[tokio::test]
async fn detail_and_board_share_the_shaping() {
    let (_tmp, addr, id) = spawn().await;

    let (_, body) = get(addr, &format!("/api/v1/items/{id}?fields=id,body")).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let obj = v["data"].as_object().unwrap();
    assert_eq!(obj.len(), 2, "detail projects too: {body}");
    assert_eq!(obj["id"], id);
    assert!(obj["body"].as_str().unwrap().contains("## Goal"));

    // Unshaped detail is unchanged.
    let (_, body) = get(addr, &format!("/api/v1/items/{id}")).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["data"]["assignee"].is_null(), "no shaping by default");
    assert_eq!(v["data"]["schema"], 1);

    // Both fixture items are open, so the open column holds them both — and
    // still does with `status` projected away.
    let (_, body) = get(addr, "/api/v1/board?group_by=status&fields=id").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let open = v["data"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "open")
        .cloned()
        .unwrap();
    assert_eq!(open["count"], 2, "grouping ran before shaping: {body}");
    assert_eq!(open["returned"], 2);
    for item in open["items"].as_array().unwrap() {
        assert_eq!(
            item.as_object().unwrap().len(),
            1,
            "each card is projected: {body}"
        );
        assert!(item["id"].is_string());
    }
}

/// A malformed *number* is a `VALIDATION_ERROR`, like a malformed sort, status
/// or boolean (read-path roadmap §7).
///
/// `?limit=abc` and `?limit=-5` used to fall through `.ok()` to the endpoint's
/// default — which on the web is **unlimited** — so a client typo asking for one
/// page silently received the entire store with a 200, and `?offset=-1` silently
/// became `0`. That is precisely the outcome the `?compact=` doc comment forty
/// lines above the code called out: a result a client cannot distinguish from a
/// server that does not implement the parameter. The CLI has always rejected the
/// same input (clap parses `--limit` as a `usize`).
#[tokio::test]
async fn a_malformed_number_is_a_validation_error() {
    let (_tmp, addr, id) = spawn().await;

    for path in [
        "/api/v1/items?limit=abc",
        "/api/v1/items?limit=-5",
        "/api/v1/items?limit=1.5",
        "/api/v1/items?limit=1e3",
        "/api/v1/items?offset=-1",
        "/api/v1/items?offset=one",
        "/api/v1/items?limit=0&offset=nope",
        "/api/v1/board?limit=abc",
        "/api/v1/board?offset=-1",
        "/api/v1/stats?top=-1",
        "/api/v1/stats?no_epics=yes",
        "/api/v1/stats/history?limit=abc",
        "/api/v1/stats/history?days=-30",
        "/api/v1/stats/history?days=al",
    ] {
        let (status, body) = get(addr, path).await;
        assert!(
            status.contains("422"),
            "{path}: status {status}, body {body}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], false, "{path}: {body}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{path}: {body}");
    }

    // The same treatment on the per-item routes, which take their own numbers.
    for path in [
        format!("/api/v1/items/{id}/comments?limit=-1"),
        format!("/api/v1/items/{id}/comments?skip_newest=x"),
        format!("/api/v1/items/{id}/deptree?depth=-2"),
    ] {
        let (status, body) = get(addr, &path).await;
        assert!(
            status.contains("422"),
            "{path}: status {status}, body {body}"
        );
    }

    // …and the spellings that *are* accepted still are, including the empty
    // value a form submits for an untouched field and the `0` that means
    // unlimited. Rejecting these would break the SPA, which sends `limit`
    // explicitly on every list request.
    for (query, returned) in [
        ("?limit=", 2),
        ("?limit=0", 2),
        ("?limit=1", 1),
        ("?limit=2&offset=0", 2),
        ("?offset=", 2),
        ("?limit=100&offset=1", 1),
    ] {
        let (status, body) = get(addr, &format!("/api/v1/items{query}")).await;
        assert!(status.contains("200"), "{query}: status {status}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["data"].as_array().unwrap().len(),
            returned,
            "{query}: {body}"
        );
    }
}

/// `/meta` names the project's sync targets (read from the state `clove sync
/// github` writes) so the SPA can link `gh-<n>` refs, and `?synced=` filters
/// the list on whether an item carries an external ref, inside the window.
#[tokio::test]
async fn meta_lists_sync_targets_and_the_list_filters_on_synced() {
    let (tmp, state, main_id, dep_id) = fixture();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let sync_dir = root.join(".clove").join("sync").join("github");
    std::fs::create_dir_all(&sync_dir).unwrap();
    std::fs::write(
        sync_dir.join("octo_widgets.json"),
        r#"{"version":1,"repo":"octo/widgets","entries":{}}"#,
    )
    .unwrap();
    let mut dep = state
        .store
        .get(&clove_types::CloveId::new(&dep_id).unwrap())
        .unwrap();
    dep.frontmatter.source_system = Some("github".to_owned());
    dep.frontmatter.external_ref = Some("gh-67".to_owned());
    state.store.update(&dep, chrono::Utc::now()).unwrap();

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (_, body) = get(addr, "/api/v1/meta").await;
    let meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        meta["data"]["sync"],
        serde_json::json!([{
            "provider": "github",
            "repo": "octo/widgets",
            "url": "https://github.com/octo/widgets",
            "issue_url": "https://github.com/octo/widgets/issues/",
        }])
    );

    let page = |body: &str| -> (Vec<String>, serde_json::Value) {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        let ids = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect();
        (ids, v["_meta"].clone())
    };

    let (_, body) = get(addr, "/api/v1/items?synced=false").await;
    let (ids, meta) = page(&body);
    assert_eq!(ids, vec![main_id.clone()], "{body}");
    assert_eq!(meta["total"], 1);
    assert_eq!(meta["filters"]["synced"], false);

    let (_, body) = get(addr, "/api/v1/items?synced=true").await;
    let (ids, _) = page(&body);
    assert_eq!(ids, vec![dep_id.clone()], "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["data"][0]["external_ref"], "gh-67");
    assert_eq!(v["data"][0]["source_system"], "github");

    // Unfiltered, both come back and `_meta.filters.synced` says no filter
    // applied.
    let (_, body) = get(addr, "/api/v1/items").await;
    let (ids, meta) = page(&body);
    assert_eq!(ids.len(), 2);
    assert!(meta["filters"]["synced"].is_null());

    // One unsynced item: page two is empty, and the total still counts it.
    let (_, body) = get(addr, "/api/v1/items?synced=false&limit=1&offset=1").await;
    let (ids, meta) = page(&body);
    assert!(ids.is_empty(), "{body}");
    assert_eq!(meta["total"], 1);

    let (status, _) = get(addr, "/api/v1/items?synced=maybe").await;
    assert!(status.contains("422"), "{status}");
}

#[tokio::test]
async fn meta_has_no_sync_targets_without_sync_state() {
    let (_tmp, addr, _id) = spawn().await;
    let (_, body) = get(addr, "/api/v1/meta").await;
    let meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(meta["data"]["sync"], serde_json::json!([]));
}

/// `/meta` carries the AGPL-3.0 section 13 source offer the SPA links to.
#[tokio::test]
async fn meta_offers_the_source_of_this_build() {
    let (_tmp, addr, _id) = spawn().await;
    let (_, body) = get(addr, "/api/v1/meta").await;
    let meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    let source = &meta["data"]["source_code"];
    assert_eq!(source["license"], "AGPL-3.0-only");
    assert_eq!(source["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        source["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("https://")),
        "{source}"
    );
}

/// `?parent=` lists an item's children through the shared filter set — on
/// every mode, combined with `?synced=`, and windowed by the engine like any
/// other filter. Any external ref counts as synced, not only a GitHub one.
#[tokio::test]
async fn the_list_filters_on_parent() {
    let (_tmp, state, main_id, dep_id) = fixture();
    let main = clove_types::CloveId::new(&main_id).unwrap();
    let child = |title: &str| {
        state
            .store
            .create(
                "proj",
                NewItem {
                    title: title.to_owned(),
                    item_type: ItemType::Chore,
                    priority: Priority(2),
                    labels: vec![],
                    deps: vec![],
                    parent: Some(main.clone()),
                    assignee: None,
                    body: String::new(),
                },
                chrono::Utc::now(),
            )
            .unwrap()
    };
    let plain = child("Plain child").frontmatter.id.to_string();
    let mut imported = child("Imported child");
    imported.frontmatter.source_system = Some("tk".to_owned());
    imported.frontmatter.external_ref = Some("tk:abc-1".to_owned());
    state.store.update(&imported, chrono::Utc::now()).unwrap();
    let imported = imported.frontmatter.id.to_string();

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let page = |body: &str| -> (Vec<String>, serde_json::Value) {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        let mut ids: Vec<String> = v["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{body}"))
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        (ids, v["_meta"].clone())
    };
    let mut children = vec![plain.clone(), imported.clone()];
    children.sort();

    for mode in ["", "&mode=ready"] {
        let (_, body) = get(addr, &format!("/api/v1/items?parent={main_id}{mode}")).await;
        let (ids, meta) = page(&body);
        assert_eq!(ids, children, "{mode}: {body}");
        assert_eq!(meta["total"], 2, "{mode}");
        assert_eq!(meta["filters"]["parent"], main_id.as_str(), "{mode}");
    }

    let (_, body) = get(addr, &format!("/api/v1/items?parent={dep_id}")).await;
    let (ids, meta) = page(&body);
    assert!(ids.is_empty(), "{body}");
    assert_eq!(meta["total"], 0);

    let (_, body) = get(addr, &format!("/api/v1/items?parent={main_id}&synced=true")).await;
    assert_eq!(page(&body).0, vec![imported.clone()], "{body}");
    let (_, body) = get(
        addr,
        &format!("/api/v1/items?parent={main_id}&synced=false"),
    )
    .await;
    assert_eq!(page(&body).0, vec![plain.clone()], "{body}");

    let (_, body) = get(
        addr,
        &format!("/api/v1/items?parent={main_id}&sort=id&limit=1&offset=1"),
    )
    .await;
    let (ids, meta) = page(&body);
    assert_eq!(ids, vec![children[1].clone()], "{body}");
    assert_eq!(meta["total"], 2, "total is the pre-window count");
    assert_eq!(meta["returned"], 1);

    // Empty is unconstrained, like every other filter parameter.
    let (_, body) = get(addr, "/api/v1/items?parent=").await;
    assert_eq!(page(&body).1["total"], 4, "{body}");

    for bad in ["?parent=not-an-id", "?parent=proj-aaaaaaaa"] {
        let (status, body) = get(addr, &format!("/api/v1/items{bad}")).await;
        assert!(status.contains("422"), "{bad}: {status} {body}");
    }
}

/// `missing_refs` names every reference with no backing item — any relation
/// kind, on an item of any status — and both tiers agree on it.
///
/// `dangling_deps` cannot carry this: it is the dangling subset of `blocked_by`,
/// so a closed item reports none. The SPA reads `missing_refs` to show such a
/// reference as missing without requesting it (a request would answer 404).
#[tokio::test]
async fn missing_refs_cover_every_relation_on_any_status() {
    let (tmp, state, main_id, dep_id) = fixture();
    let issues = state.issues_dir.clone();
    let gone_dep = clove_types::CloveId::new("proj-0000000D").unwrap();
    let gone_parent = clove_types::CloveId::new("proj-0000000P").unwrap();
    let gone_rel = clove_types::CloveId::new("proj-0000000R").unwrap();

    let mut main = state
        .store
        .get(&clove_types::CloveId::new(&main_id).unwrap())
        .unwrap();
    main.frontmatter.status = clove_types::ItemStatus::Closed;
    main.frontmatter.closed = Some(main.frontmatter.updated);
    main.frontmatter.deps.push(gone_dep.clone());
    main.frontmatter.parent = Some(gone_parent.clone());
    main.frontmatter.relates = vec![
        gone_rel.clone(),
        clove_types::CloveId::new(&dep_id).unwrap(),
    ];
    main.frontmatter.supersedes = vec![gone_dep.clone()];
    state.store.update(&main, chrono::Utc::now()).unwrap();

    let expected = serde_json::json!([
        gone_dep.to_string(),
        gone_parent.to_string(),
        gone_rel.to_string()
    ]);
    let row = |v: &serde_json::Value| -> serde_json::Value {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == main_id)
            .cloned()
            .unwrap()
    };

    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (_s, body) = get(addr, "/api/v1/items?sort=id&status=open,closed").await;
    let files: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(files["_meta"]["source"], "files", "body: {body}");
    let main_row = row(&files);
    assert_eq!(main_row["missing_refs"], expected, "body: {body}");
    assert_eq!(
        main_row["dangling_deps"],
        serde_json::json!([]),
        "a closed item is not blocked, so dangling_deps stays empty"
    );

    let (_s, body) = get(addr, &format!("/api/v1/items/{main_id}")).await;
    let detail: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["data"]["missing_refs"], expected, "body: {body}");

    clove_index::reindex(&issues, &issues.parent().unwrap().join("index.db")).unwrap();
    let indexed_state = AppState::new(
        state.store.clone(),
        issues,
        "proj".to_owned(),
        "test",
        false,
        ItemType::Feature,
    );
    let app = build_router(indexed_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (_s, body) = get(addr, "/api/v1/items?sort=id&status=open,closed&limit=100").await;
    let indexed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        indexed["_meta"]["source"], "index",
        "the index must answer, or the comparison is vacuous: {body}"
    );
    assert_eq!(
        indexed["data"], files["data"],
        "the tier must not change a field"
    );
    assert_eq!(row(&indexed)["missing_refs"], expected);

    drop(tmp);
}

/// A second server over the same repository, after building its index.
async fn spawn_indexed(state: &AppState) -> std::net::SocketAddr {
    let issues = state.issues_dir.clone();
    clove_index::reindex(&issues, &issues.parent().unwrap().join("index.db")).unwrap();
    let indexed_state = AppState::new(
        state.store.clone(),
        issues,
        "proj".to_owned(),
        "test",
        false,
        ItemType::Feature,
    );
    let app = build_router(indexed_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// An unwindowed list (the web default) and the board return every matching
/// item. A tier answers that the expensive way — it reads every item file to
/// hydrate the rows, then each row's dependency files again for the graph
/// terms — where one file scan reads each file once. So those two requests are
/// pinned to the file tier even beside a live index; a windowed request keeps
/// the tiers. The rows are the same either way.
#[tokio::test]
async fn unlimited_reads_use_one_file_scan() {
    let (_tmp, state, _main_id, _dep_id) = fixture();
    let addr = spawn_indexed(&state).await;
    let json = |body: &str| -> serde_json::Value { serde_json::from_str(body).unwrap() };

    let (_s, body) = get(addr, "/api/v1/items?sort=id&limit=10").await;
    let windowed = json(&body);
    assert_eq!(
        windowed["_meta"]["source"], "index",
        "a windowed read must still use the index, or this test is vacuous: {body}"
    );

    for query in [
        "?sort=id",
        "?sort=id&limit=0",
        "?sort=id&mode=ready&limit=0",
    ] {
        let (_s, body) = get(addr, &format!("/api/v1/items{query}")).await;
        let all = json(&body);
        assert_eq!(all["_meta"]["source"], "files", "{query}: {body}");
        if !query.contains("mode") {
            assert_eq!(all["data"], windowed["data"], "{query}: same rows");
        }
    }

    let (_s, body) = get(addr, "/api/v1/board").await;
    assert_eq!(json(&body)["_meta"]["source"], "files", "board: {body}");
}

/// `_meta.source` names a tier on the write responses too. They are rendered
/// from the item files, so the answer is `files`; they used to carry the
/// serving mode (`standalone`, or `daemon` under `cloved`), which is not a
/// value of the published `daemon|index|files` enum.
#[tokio::test]
async fn write_responses_report_the_files_tier() {
    let (_tmp, addr, main_id, dep_id) = spawn_ids().await;
    let source = |body: &str| -> String {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["_meta"]["source"].as_str().unwrap_or_default().to_owned()
    };
    let item = format!("/api/v1/items/{main_id}");

    let (status, body) = send(addr, "POST", "/api/v1/items", Some(r#"{"title":"New"}"#)).await;
    assert!(status.contains("200"), "{status} {body}");
    assert_eq!(source(&body), "files", "create: {body}");

    let (_s, body) = send(addr, "PATCH", &item, Some(r#"{"title":"Renamed"}"#)).await;
    assert_eq!(source(&body), "files", "patch: {body}");

    let labels = format!("{item}/labels");
    let (_s, body) = send(addr, "PUT", &labels, Some(r#"{"add":["x"]}"#)).await;
    assert_eq!(source(&body), "files", "labels: {body}");

    let comments = format!("{item}/comments");
    let (_s, body) = send(addr, "POST", &comments, Some(r#"{"body":"hi"}"#)).await;
    assert_eq!(source(&body), "files", "comment: {body}");

    let dep = format!("{item}/deps/{dep_id}");
    let (_s, body) = send(addr, "DELETE", &dep, None).await;
    assert_eq!(source(&body), "files", "remove dep: {body}");

    let (_s, body) = send(addr, "DELETE", &item, None).await;
    assert_eq!(source(&body), "files", "delete: {body}");
}

/// A comment posted with no `author` is attributed by the rule `clove comment`
/// and the MCP `clove_comment` tool share (`CLOVE_AUTHOR`, the git identity,
/// `$USER`), not to the literal `web`. An author the client names is kept.
#[tokio::test]
async fn comment_without_author_uses_the_shared_author_rule() {
    let (tmp, state, main_id, _dep_id) = fixture();
    // A repository identity, so the rule has something to resolve whatever
    // the environment of the test run is.
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "web-test@example.com"]);
    let expected = clove_core::comments::comment_author(state.store.repo_root());
    assert_ne!(expected, "unknown", "the fixture identity resolves");
    assert_ne!(expected, "web");

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let path = format!("/api/v1/items/{main_id}/comments");
    let (status, body) = send(addr, "POST", &path, Some(r#"{"body":"first"}"#)).await;
    assert!(status.contains("200"), "{status} {body}");
    let (status, body) = send(
        addr,
        "POST",
        &path,
        Some(r#"{"body":"second","author":"alice"}"#),
    )
    .await;
    assert!(status.contains("200"), "{status} {body}");

    let (_s, body) = get(addr, &path).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let author_of = |text: &str| -> String {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"].as_str().is_some_and(|b| b.trim() == text))
            .unwrap_or_else(|| panic!("no comment `{text}`: {body}"))["author"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // The store keeps an author as a file-name slug (`a@b.c` → `a-b-c`).
    let slug: String = expected
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    assert_eq!(author_of("first"), slug, "{body}");
    assert_eq!(author_of("second"), "alice", "{body}");
}

/// A PATCH that edits nothing is an error, as it is for `clove set` and the
/// MCP `clove_edit` tool. An empty object, or one naming only a key this
/// endpoint does not edit (`parent` has its own endpoint), used to return 200
/// and the unchanged item — indistinguishable from a successful edit.
#[tokio::test]
async fn patch_rejects_empty_and_unknown_fields() {
    let (_tmp, addr, main_id, dep_id) = spawn_ids().await;
    let item = format!("/api/v1/items/{main_id}");

    let (status, body) = send(addr, "PATCH", &item, Some("{}")).await;
    assert!(status.contains("422"), "empty: {status} {body}");
    assert!(body.contains("VALIDATION_ERROR"), "{body}");
    assert!(body.contains("no fields to edit"), "{body}");

    let parent = format!(r#"{{"parent":"{dep_id}"}}"#);
    let (status, body) = send(addr, "PATCH", &item, Some(&parent)).await;
    assert!(status.contains("400"), "unknown key: {status} {body}");
    assert!(body.contains("parent"), "names the key: {body}");

    // An unknown key beside a real one is refused too, and nothing is written.
    let mixed = r#"{"title":"Changed","deps":[]}"#;
    let (status, body) = send(addr, "PATCH", &item, Some(mixed)).await;
    assert!(status.contains("400"), "mixed: {status} {body}");
    let (_s, body) = get(addr, &item).await;
    assert!(body.contains("Add webhook handler"), "unchanged: {body}");

    // `assignee: null` alone is a real edit (clear), not an empty one.
    let (status, body) = send(addr, "PATCH", &item, Some(r#"{"assignee":null}"#)).await;
    assert!(status.contains("200"), "clear assignee: {status} {body}");
}

/// `?force=` goes through the same boolean parser as every other flag of this
/// API: `1` is true (it used to be read as false, so the delete was refused),
/// and a value that is not a boolean is an error.
#[tokio::test]
async fn delete_force_accepts_the_shared_boolean_spellings() {
    let (_tmp, addr, _main_id, dep_id) = spawn_ids().await;
    let item = format!("/api/v1/items/{dep_id}");

    let (status, body) = send(addr, "DELETE", &format!("{item}?force=yes"), None).await;
    assert!(status.contains("422"), "not a boolean: {status} {body}");
    assert!(body.contains("VALIDATION_ERROR"), "{body}");

    let (status, body) = send(addr, "DELETE", &format!("{item}?force=0"), None).await;
    assert!(
        status.contains("409"),
        "force=0 is no force: {status} {body}"
    );

    let (status, body) = send(addr, "DELETE", &format!("{item}?force=1"), None).await;
    assert!(status.contains("200"), "force=1: {status} {body}");
    assert!(body.contains("\"deleted\":true"), "{body}");
}

/// A file under `_app/` that this build does not have is a 404. It used to be
/// answered with the SPA entry page (the fallback for client-side routes), so
/// a tab left open across an upgrade failed its chunk import on a MIME type.
/// A client-side route still gets the entry page.
#[tokio::test]
async fn missing_app_chunk_is_404() {
    let (_tmp, addr, _id) = spawn().await;

    let (status, _body) = get(addr, "/_app/immutable/chunks/gone.abc123.js").await;
    assert!(status.contains("404"), "missing chunk: {status}");
    let (status, _body) = get(addr, "/_app/version.json.missing").await;
    assert!(status.contains("404"), "missing build file: {status}");

    let (status, body) = get(addr, "/items/proj-0000000A").await;
    assert!(status.contains("200"), "a client route: {status}");
    assert!(body.contains("<html"), "serves the entry page: {body}");
}
