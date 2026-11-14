//! The web access token (DESIGN §8.4), over real TCP: a request without it is
//! refused, and the URL the CLI prints turns into a cookie the SPA rides on.

use camino::Utf8PathBuf;
use clove_core::{ItemStore, NewItem};
use clove_types::{ItemType, Priority};
use clove_web::{build_router, AppState, HubWeb, WebAuth};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn state(root: &Utf8PathBuf, title: &str) -> AppState {
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
    AppState::new(
        store,
        issues,
        "proj".to_owned(),
        "standalone",
        false,
        ItemType::Feature,
    )
}

fn tmp_root() -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    (tmp, root)
}

async fn serve(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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

/// `method path` with extra header lines (each ends in `\r\n`) and a JSON body.
async fn request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    json: Option<&str>,
) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = json.unwrap_or("");
    let content = match json {
        Some(_) => format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ),
        None => String::new(),
    };
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{headers}{content}Connection: close\r\n\r\n{body}",
        addr.port()
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

async fn get(addr: std::net::SocketAddr, path: &str, headers: &str) -> Reply {
    request(addr, "GET", path, headers, None).await
}

/// The path and query of a URL that [`WebAuth::url`] made.
fn path_of(url: &str, addr: std::net::SocketAddr) -> String {
    url.strip_prefix(&format!("http://{addr}"))
        .unwrap()
        .to_owned()
}

/// Without the token nothing is served: not the API, not a write, not the
/// page. The refusal says where the token is.
#[tokio::test]
async fn a_request_without_the_token_is_refused() {
    let (_tmp, root) = tmp_root();
    let auth = WebAuth::generate().unwrap();
    let addr = serve(build_router(state(&root, "secret plan").with_auth(auth))).await;

    for path in ["/api/v1/items", "/api/v1/meta", "/", "/api/v1/events"] {
        let reply = get(addr, path, "").await;
        assert_eq!(reply.status, 401, "{path}: {}", reply.head);
        assert!(!reply.body.contains("secret plan"), "{path}");
        assert!(reply.body.contains("clove serve"), "{}", reply.body);
    }
    let write = request(
        addr,
        "POST",
        "/api/v1/items",
        "",
        Some(r#"{"title":"planted"}"#),
    )
    .await;
    assert_eq!(write.status, 401);
    for wrong in [
        "/api/v1/items?token=",
        "/api/v1/items?token=0000",
        "/api/v1/items?nottoken=x",
    ] {
        assert_eq!(get(addr, wrong, "").await.status, 401, "{wrong}");
    }
    let wrong_cookie = get(addr, "/api/v1/items", "Cookie: clove_web_1=nope\r\n").await;
    assert_eq!(wrong_cookie.status, 401);
    let wrong_bearer = get(addr, "/api/v1/items", "Authorization: Bearer nope\r\n").await;
    assert_eq!(wrong_bearer.status, 401);
}

/// The printed URL works as it is: the page load sets the cookie and
/// redirects to the same page without the token, and the cookie alone then
/// carries the SPA's API calls and writes.
#[tokio::test]
async fn the_printed_url_becomes_a_cookie_session() {
    let (_tmp, root) = tmp_root();
    let auth = WebAuth::generate().unwrap();
    let addr = serve(build_router(
        state(&root, "secret plan").with_auth(auth.clone()),
    ))
    .await;
    let url = auth.url(&format!("http://{addr}/"));

    let landing = get(addr, &path_of(&url, addr), "").await;
    assert_eq!(landing.status, 303, "{}", landing.head);
    assert_eq!(landing.header("location").as_deref(), Some("/"));
    let set_cookie = landing.header("set-cookie").expect("a cookie is set");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    assert!(
        set_cookie.starts_with(&format!("clove_web_{}=", addr.port())),
        "{set_cookie}"
    );
    let cookie = format!("Cookie: {}\r\n", set_cookie.split(';').next().unwrap());

    let page = get(addr, "/", &cookie).await;
    assert_eq!(page.status, 200, "{}", page.head);
    let items = get(addr, "/api/v1/items", &cookie).await;
    assert_eq!(items.status, 200);
    assert!(items.body.contains("secret plan"));
    let created = request(
        addr,
        "POST",
        "/api/v1/items",
        &cookie,
        Some(r#"{"title":"made in the UI"}"#),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);

    // Other query parameters survive the redirect.
    let deep = get(
        addr,
        &format!("/list?status=open&token={}", auth.token()),
        "",
    )
    .await;
    assert_eq!(deep.status, 303);
    assert_eq!(
        deep.header("location").as_deref(),
        Some("/list?status=open")
    );
}

/// A script needs no cookie: the bearer header, or `?token=` on the API
/// request itself, is enough.
#[tokio::test]
async fn a_script_can_send_the_token_directly() {
    let (_tmp, root) = tmp_root();
    let auth = WebAuth::generate().unwrap();
    let addr = serve(build_router(
        state(&root, "secret plan").with_auth(auth.clone()),
    ))
    .await;

    let bearer = format!("Authorization: Bearer {}\r\n", auth.token());
    let items = get(addr, "/api/v1/items", &bearer).await;
    assert_eq!(items.status, 200);
    assert!(items.body.contains("secret plan"));

    let by_query = get(
        addr,
        &format!("/api/v1/items?limit=1&token={}", auth.token()),
        "",
    )
    .await;
    assert_eq!(by_query.status, 200, "{}", by_query.head);
    assert!(by_query.body.contains("secret plan"));
}

/// One token guards the whole hub: the project list, the root page and every
/// mounted project. The project URL the daemon reports carries it.
#[tokio::test]
async fn the_hub_is_guarded_as_a_whole() {
    let (_tmp, root) = tmp_root();
    let project = root.join("alpha");
    let auth = WebAuth::generate().unwrap();
    let hub = HubWeb::new().with_auth(auth.clone());
    let slug = hub.mount(&project, state(&project, "alpha plan"));
    let addr = serve(hub.router()).await;

    for path in [
        "/".to_owned(),
        "/api/v1/projects".to_owned(),
        format!("/p/{slug}/"),
        format!("/p/{slug}/api/v1/items"),
    ] {
        assert_eq!(get(addr, &path, "").await.status, 401, "{path}");
    }

    let url = auth.url(&format!("http://{addr}/p/{slug}/"));
    let landing = get(addr, &path_of(&url, addr), "").await;
    assert_eq!(landing.status, 303, "{}", landing.head);
    assert_eq!(
        landing.header("location"),
        Some(format!("/p/{slug}/")),
        "{}",
        landing.head
    );
    let set_cookie = landing.header("set-cookie").unwrap();
    let cookie = format!("Cookie: {}\r\n", set_cookie.split(';').next().unwrap());
    let items = get(addr, &format!("/p/{slug}/api/v1/items"), &cookie).await;
    assert_eq!(items.status, 200);
    assert!(items.body.contains("alpha plan"));
    assert_eq!(get(addr, "/api/v1/projects", &cookie).await.status, 200);
}

/// A router built without a token stays open: the library default, which the
/// hub relies on for the per-project routers behind its own guard.
#[tokio::test]
async fn a_router_without_a_token_is_open() {
    let (_tmp, root) = tmp_root();
    let addr = serve(build_router(state(&root, "open plan"))).await;
    assert_eq!(get(addr, "/api/v1/items", "").await.status, 200);
}
