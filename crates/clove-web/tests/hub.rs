//! Integration tests for the daemon hub's web front: one listener serving every
//! loaded project under `/p/<slug>/`, driven over real TCP.

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{ItemStore, NewItem};
use clove_types::{ItemType, Priority};
use clove_web::{AppState, HubWeb};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A repo at `<parent>/<name>` holding one item titled `title`.
fn repo(parent: &Utf8Path, name: &str, title: &str) -> (Utf8PathBuf, AppState) {
    let root = parent.join(name);
    let issues = root.join(".clove").join("issues");
    std::fs::create_dir_all(&issues).unwrap();
    let store = ItemStore::new(root.clone());
    store
        .create(
            "proj",
            NewItem {
                title: title.to_owned(),
                item_type: ItemType::Feature,
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
    let state = AppState::new(
        store,
        issues,
        "proj".to_owned(),
        "daemon",
        true,
        ItemType::Feature,
    );
    (root, state)
}

fn tmp_root() -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    (tmp, root)
}

async fn serve(hub: &HubWeb) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = hub.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    }
}

async fn request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    json: Option<&str>,
) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = json.unwrap_or("");
    let extra = if json.is_some() {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    } else {
        String::new()
    };
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n{body}"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

async fn get(addr: std::net::SocketAddr, path: &str) -> Reply {
    request(addr, "GET", path, "localhost", None).await
}

fn titles(body: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn each_project_is_served_under_its_own_prefix() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "Alpha item");
    let (b_root, b) = repo(&parent, "beta", "Beta item");
    assert_eq!(hub.mount(&a_root, a), "alpha");
    assert_eq!(hub.mount(&b_root, b), "beta");
    let addr = serve(&hub).await;

    let alpha = get(addr, "/p/alpha/api/v1/items").await;
    assert_eq!(alpha.status, 200, "{}", alpha.body);
    assert_eq!(titles(&alpha.body), vec!["Alpha item"]);
    let beta = get(addr, "/p/beta/api/v1/items").await;
    assert_eq!(titles(&beta.body), vec!["Beta item"]);
}

#[tokio::test]
async fn a_write_in_one_project_never_shows_in_another() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "Alpha item");
    let (b_root, b) = repo(&parent, "beta", "Beta item");
    hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let created = request(
        addr,
        "POST",
        "/p/alpha/api/v1/items",
        "localhost",
        Some(r#"{"title":"Only in alpha"}"#),
    )
    .await;
    assert!(created.status < 300, "create failed: {}", created.body);

    let mut alpha = titles(&get(addr, "/p/alpha/api/v1/items").await.body);
    alpha.sort();
    assert_eq!(alpha, vec!["Alpha item", "Only in alpha"]);
    assert_eq!(
        titles(&get(addr, "/p/beta/api/v1/items").await.body),
        vec!["Beta item"],
        "beta must not see alpha's write"
    );
    assert_eq!(
        std::fs::read_dir(b_root.join(".clove/issues"))
            .unwrap()
            .count(),
        1,
        "nothing was written into beta's store"
    );
}

#[tokio::test]
async fn projects_endpoint_lists_every_mounted_project() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let reply = get(addr, "/api/v1/projects").await;
    assert_eq!(reply.status, 200);
    let v: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(v["ok"], true);
    let projects = v["data"]["projects"].as_array().unwrap();
    let urls: Vec<&str> = projects
        .iter()
        .map(|p| p["url"].as_str().unwrap())
        .collect();
    assert_eq!(urls, vec!["/p/alpha/", "/p/beta/"]);
    assert_eq!(projects[0]["name"], "alpha");
    assert_eq!(projects[0]["root"], a_root.as_str());
}

#[tokio::test]
async fn with_one_project_unprefixed_paths_redirect_into_it() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    hub.mount(&a_root, a);
    let addr = serve(&hub).await;

    for (from, to) in [
        ("/", "/p/alpha/"),
        ("/list?q=x", "/p/alpha/list?q=x"),
        ("/items/proj-1", "/p/alpha/items/proj-1"),
        ("/api/v1/items", "/p/alpha/api/v1/items"),
    ] {
        let reply = get(addr, from).await;
        assert_eq!(reply.status, 307, "{from}");
        assert_eq!(reply.header("location").as_deref(), Some(to), "{from}");
    }
}

#[tokio::test]
async fn with_several_projects_the_root_is_a_picker() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let root = get(addr, "/").await;
    assert_eq!(root.status, 200);
    assert!(root.body.contains(r#"href="/p/alpha/""#), "{}", root.body);
    assert!(root.body.contains(r#"href="/p/beta/""#), "{}", root.body);

    // Which project an unprefixed API call means is now ambiguous.
    assert_eq!(get(addr, "/api/v1/items").await.status, 404);
}

#[tokio::test]
async fn with_no_projects_the_root_says_so() {
    let hub = HubWeb::new();
    let addr = serve(&hub).await;
    let root = get(addr, "/").await;
    assert_eq!(root.status, 200);
    assert!(root.body.contains("clove daemon start"), "{}", root.body);
}

#[tokio::test]
async fn an_unknown_project_is_not_found() {
    let hub = HubWeb::new();
    let addr = serve(&hub).await;
    assert_eq!(get(addr, "/p/nope/api/v1/items").await.status, 404);
}

#[tokio::test]
async fn the_bare_prefix_redirects_to_its_slash_form() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;
    let reply = get(addr, "/p/alpha").await;
    assert_eq!(reply.status, 307);
    assert_eq!(reply.header("location").as_deref(), Some("/p/alpha/"));
}

#[tokio::test]
async fn the_app_page_runs_under_the_project_base() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let page = get(addr, "/p/alpha/board").await;
    assert_eq!(page.status, 200);
    // Only a real SvelteKit build carries the runtime-base global; the Node-free
    // placeholder page has nothing to rewrite.
    if page.body.contains("__sveltekit_") {
        assert!(
            page.body.contains(r#"base: "/p/alpha""#),
            "the SPA must boot with the project base: {}",
            page.body
        );
        assert!(
            !page.body.contains(r#""/_app/"#),
            "assets stay under the prefix"
        );
    }
}

#[tokio::test]
async fn two_repos_with_one_name_get_distinct_slugs() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent.join("one"), "app", "First");
    let (b_root, b) = repo(&parent.join("two"), "app", "Second");
    let first = hub.mount(&a_root, a);
    let second = hub.mount(&b_root, b);
    assert_eq!(first, "app");
    assert!(
        second.starts_with("app-") && second.len() == "app-".len() + 6,
        "{second}"
    );

    let addr = serve(&hub).await;
    assert_eq!(
        titles(&get(addr, &format!("/p/{second}/api/v1/items")).await.body),
        vec!["Second"]
    );
}

#[tokio::test]
async fn unmounting_removes_the_project() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    hub.unmount(&beta);
    assert_eq!(get(addr, "/p/beta/api/v1/items").await.status, 404);
    assert_eq!(get(addr, "/p/alpha/api/v1/items").await.status, 200);
}

#[tokio::test]
async fn the_event_socket_upgrades_through_the_prefix() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    hub.mount(&a_root, a);
    let addr = serve(&hub).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = "GET /p/alpha/api/v1/events HTTP/1.1\r\nHost: localhost\r\n\
               Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake reply")
        .unwrap();
    let head = String::from_utf8_lossy(&buf[..n]);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
}

#[tokio::test]
async fn the_hub_rejects_a_non_local_host() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    hub.mount(&a_root, a);
    let addr = serve(&hub).await;
    for path in ["/", "/api/v1/projects", "/p/alpha/api/v1/items"] {
        let reply = request(addr, "GET", path, "evil.example.com", None).await;
        assert_eq!(reply.status, 403, "{path}");
    }
}
