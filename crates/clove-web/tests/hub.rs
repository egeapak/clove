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

fn is_slug_of(slug: &str, name: &str) -> bool {
    slug.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hash| hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()))
}

#[tokio::test]
async fn each_project_is_served_under_its_own_prefix() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "Alpha item");
    let (b_root, b) = repo(&parent, "beta", "Beta item");
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    assert!(is_slug_of(&alpha, "alpha"), "{alpha}");
    assert!(is_slug_of(&beta, "beta"), "{beta}");
    let addr = serve(&hub).await;

    let reply = get(addr, &format!("/p/{alpha}/api/v1/items")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(titles(&reply.body), vec!["Alpha item"]);
    let reply = get(addr, &format!("/p/{beta}/api/v1/items")).await;
    assert_eq!(titles(&reply.body), vec!["Beta item"]);
}

#[tokio::test]
async fn a_write_in_one_project_never_shows_in_another() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "Alpha item");
    let (b_root, b) = repo(&parent, "beta", "Beta item");
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let created = request(
        addr,
        "POST",
        &format!("/p/{alpha}/api/v1/items"),
        "localhost",
        Some(r#"{"title":"Only in alpha"}"#),
    )
    .await;
    assert!(created.status < 300, "create failed: {}", created.body);

    let mut alpha_titles = titles(&get(addr, &format!("/p/{alpha}/api/v1/items")).await.body);
    alpha_titles.sort();
    assert_eq!(alpha_titles, vec!["Alpha item", "Only in alpha"]);
    assert_eq!(
        titles(&get(addr, &format!("/p/{beta}/api/v1/items")).await.body),
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
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
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
    assert_eq!(urls, vec![format!("/p/{alpha}/"), format!("/p/{beta}/")]);
    assert_eq!(projects[0]["name"], "alpha");
    assert_eq!(projects[0]["root"], a_root.as_str());
}

#[tokio::test]
async fn with_one_project_unprefixed_paths_redirect_into_it() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let alpha = hub.mount(&a_root, a);
    let addr = serve(&hub).await;

    for (from, to) in [
        ("/", "/"),
        ("/list?q=x", "/list?q=x"),
        ("/items/proj-1", "/items/proj-1"),
        ("/api/v1/items", "/api/v1/items"),
    ] {
        let reply = get(addr, from).await;
        assert_eq!(reply.status, 307, "{from}");
        let to = format!("/p/{alpha}{to}");
        assert_eq!(reply.header("location"), Some(to), "{from}");
    }
}

#[tokio::test]
async fn with_several_projects_the_root_is_a_picker() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let root = get(addr, "/").await;
    assert_eq!(root.status, 200);
    for slug in [&alpha, &beta] {
        let link = format!(r#"href="/p/{slug}/""#);
        assert!(root.body.contains(&link), "{}", root.body);
    }

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
    let alpha = hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;
    let reply = get(addr, &format!("/p/{alpha}")).await;
    assert_eq!(reply.status, 307);
    assert_eq!(reply.header("location"), Some(format!("/p/{alpha}/")));
}

#[tokio::test]
async fn the_app_page_runs_under_the_project_base() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    let alpha = hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let page = get(addr, &format!("/p/{alpha}/board")).await;
    assert_eq!(page.status, 200);
    // Only a real SvelteKit build carries the runtime-base global; the Node-free
    // placeholder page has nothing to rewrite.
    if page.body.contains("__sveltekit_") {
        let base = format!(r#"base: "/p/{alpha}""#);
        assert!(
            page.body.contains(&base),
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
    assert!(is_slug_of(&first, "app"), "{first}");
    assert!(is_slug_of(&second, "app"), "{second}");
    assert_ne!(first, second);

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
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    hub.unmount(&beta);
    let gone = get(addr, &format!("/p/{beta}/api/v1/items")).await;
    assert_eq!(gone.status, 404);
    let kept = get(addr, &format!("/p/{alpha}/api/v1/items")).await;
    assert_eq!(kept.status, 200);
}

#[tokio::test]
async fn the_event_socket_upgrades_through_the_prefix() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let alpha = hub.mount(&a_root, a);
    let addr = serve(&hub).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "GET /p/{alpha}/api/v1/events HTTP/1.1\r\nHost: localhost\r\n\
         Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
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
    let alpha = hub.mount(&a_root, a);
    let addr = serve(&hub).await;
    let items = format!("/p/{alpha}/api/v1/items");
    for path in ["/", "/api/v1/projects", items.as_str()] {
        let reply = request(addr, "GET", path, "evil.example.com", None).await;
        assert_eq!(reply.status, 403, "{path}");
    }
}

/// A fresh server state for an existing repo (a second mount of it).
fn state_for(root: &Utf8Path) -> AppState {
    AppState::new(
        ItemStore::new(root.to_owned()),
        root.join(".clove").join("issues"),
        "proj".to_owned(),
        "daemon",
        true,
        ItemType::Feature,
    )
}

/// A slug is a function of the repository alone: it survives unmount and a
/// hub restart, and another repository with the same name never gets it —
/// whichever loads first. A tab left open on a repository's URL can never
/// start writing into a different one.
#[tokio::test]
async fn a_slug_belongs_to_one_repository_across_restarts() {
    let (_tmp, parent) = tmp_root();
    let (a_root, a) = repo(&parent.join("one"), "proj", "A");
    let (b_root, b) = repo(&parent.join("two"), "proj", "B");

    let before = HubWeb::new();
    let a_slug = before.mount(&a_root, a);
    before.unmount(&a_slug);
    drop(before);

    // After a restart, the other `proj` happens to load first.
    let after = HubWeb::new();
    let b_slug = after.mount(&b_root, b);
    assert_ne!(b_slug, a_slug, "A's slug went to another repo");
    let addr = serve(&after).await;
    let stale = request(
        addr,
        "POST",
        &format!("/p/{a_slug}/api/v1/items"),
        "localhost",
        Some(r#"{"title":"from a stale tab"}"#),
    )
    .await;
    assert_eq!(stale.status, 404, "{}", stale.body);
    assert_eq!(
        std::fs::read_dir(b_root.join(".clove/issues"))
            .unwrap()
            .count(),
        1,
        "nothing was written into B"
    );

    assert_eq!(
        after.mount(&a_root, state_for(&a_root)),
        a_slug,
        "A keeps its slug"
    );
}

/// Open a WebSocket to `path`, with an optional `Origin`; returns the stream
/// and the handshake's response head.
async fn ws_connect(
    addr: std::net::SocketAddr,
    path: &str,
    origin: Option<&str>,
) -> (tokio::net::TcpStream, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\n{origin}Connection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        addr.port()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake reply")
        .unwrap();
    (stream, String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Unmounting a project closes its live-update sockets: a tab left open must
/// not keep listening to a project the hub no longer serves.
#[tokio::test]
async fn unmounting_closes_the_projects_event_sockets() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let slug = hub.mount(&a_root, a);
    let addr = serve(&hub).await;
    let (mut stream, head) = ws_connect(addr, &format!("/p/{slug}/api/v1/events"), None).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");

    hub.unmount(&slug);
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return true,
                // A close frame (opcode 0x8) ends the conversation too.
                Ok(n) if buf[..n].contains(&0x88) => return true,
                Ok(_) => continue,
            }
        }
    })
    .await;
    assert!(
        closed.unwrap_or(false),
        "the socket stayed open after unmount"
    );
}

/// A page on another local port is another origin: its WebSocket handshake is
/// refused even though it, too, is loopback.
#[tokio::test]
async fn an_event_socket_from_another_local_port_is_refused() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let alpha = hub.mount(&a_root, a);
    let addr = serve(&hub).await;
    let events = format!("/p/{alpha}/api/v1/events");
    let (_s, head) = ws_connect(addr, &events, Some("http://localhost:3000")).await;
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    let same = format!("http://localhost:{}", addr.port());
    let (_s, head) = ws_connect(addr, &events, Some(&same)).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
}

/// The `script-src` sources of a Content-Security-Policy.
fn script_src(csp: &str) -> Vec<String> {
    csp.split(';')
        .map(str::trim)
        .find_map(|directive| directive.strip_prefix("script-src "))
        .map(|sources| sources.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The CSP source for each inline `<script>` in `page`.
fn inline_script_hashes(page: &str) -> Vec<String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    let mut hashes = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("<script") {
        let tag_end = start + rest[start..].find('>').unwrap() + 1;
        let body_end = tag_end + rest[tag_end..].find("</script>").unwrap();
        if !rest[start..tag_end].contains("src=") {
            let digest = sha2::Sha256::digest(&rest.as_bytes()[tag_end..body_end]);
            let encoded = base64::engine::general_purpose::STANDARD.encode(digest);
            hashes.push(format!("'sha256-{encoded}'"));
        }
        rest = &rest[body_end..];
    }
    hashes
}

/// The SPA's inline scripts run by hash, not by `'unsafe-inline'`, under every
/// base; the picker runs no script at all.
#[tokio::test]
async fn hub_pages_allow_only_their_own_inline_scripts() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    let alpha = hub.mount(&a_root, a);
    let beta = hub.mount(&b_root, b);
    let addr = serve(&hub).await;

    let picker = get(addr, "/").await;
    let csp = picker.header("content-security-policy").unwrap_or_default();
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert_eq!(script_src(&csp), vec!["'none'"], "{csp}");

    for path in [
        format!("/p/{alpha}/"),
        format!("/p/{alpha}/board"),
        format!("/p/{beta}/"),
    ] {
        let page = get(addr, &path).await;
        let csp = page.header("content-security-policy").unwrap_or_default();
        assert!(csp.contains("default-src 'self'"), "{path}: {csp}");
        let sources = script_src(&csp);
        assert!(
            !sources.iter().any(|s| s == "'unsafe-inline'"),
            "{path}: {csp}"
        );
        let hashes = inline_script_hashes(&page.body);
        // A real build boots from inline scripts; only the Node-free
        // placeholder page has none.
        assert!(
            !hashes.is_empty() || !page.body.contains("__sveltekit_"),
            "{path}"
        );
        for hash in hashes {
            assert!(sources.contains(&hash), "{path}: {hash} missing from {csp}");
        }
    }
}

/// `clove serve`'s own page gets the same treatment.
#[tokio::test]
async fn the_standalone_page_allows_only_its_own_inline_scripts() {
    let (_tmp, parent) = tmp_root();
    let (_root, state) = repo(&parent, "alpha", "A");
    let addr = serve_router(clove_web::build_router(state)).await;
    let page = get(addr, "/").await;
    assert_eq!(page.status, 200);
    let csp = page.header("content-security-policy").unwrap_or_default();
    let sources = script_src(&csp);
    assert!(!sources.iter().any(|s| s == "'unsafe-inline'"), "{csp}");
    let hashes = inline_script_hashes(&page.body);
    assert!(!hashes.is_empty() || !page.body.contains("__sveltekit_"));
    for hash in hashes {
        assert!(sources.contains(&hash), "{hash} missing from {csp}");
    }
}

async fn serve_router(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// Nothing the server sends may be sniffed into another type — JSON, pages,
/// assets and errors alike, on the hub and on `clove serve`.
#[tokio::test]
async fn every_response_forbids_content_sniffing() {
    let (_tmp, parent) = tmp_root();
    let hub = HubWeb::new();
    let (a_root, a) = repo(&parent, "alpha", "A");
    let (b_root, b) = repo(&parent, "beta", "B");
    let alpha = hub.mount(&a_root, a);
    hub.mount(&b_root, b);
    let addr = serve(&hub).await;
    for path in [
        "/".to_owned(),
        "/api/v1/projects".to_owned(),
        "/p/nope/".to_owned(),
        format!("/p/{alpha}/"),
        format!("/p/{alpha}/api/v1/items"),
        format!("/p/{alpha}/api/v1/items/nope"),
    ] {
        let reply = get(addr, &path).await;
        assert_eq!(
            reply.header("x-content-type-options").as_deref(),
            Some("nosniff"),
            "{path}: {}",
            reply.head
        );
    }

    let (_root, state) = repo(&parent, "gamma", "G");
    let addr = serve_router(clove_web::build_router(state)).await;
    for path in ["/", "/api/v1/items", "/api/v1/items/nope"] {
        let reply = get(addr, path).await;
        assert_eq!(
            reply.header("x-content-type-options").as_deref(),
            Some("nosniff"),
            "{path}: {}",
            reply.head
        );
    }
}
