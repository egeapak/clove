//! The daemon hub's web front (DESIGN §8.10): one listener serving every loaded
//! project, each under its own `/p/<slug>/` prefix.
//!
//! A project's router is the ordinary [`build_router`] over its own
//! [`AppState`]; this layer only picks which one a request belongs to and strips
//! the prefix before handing it over. Projects mount and unmount while the
//! server runs (the hub loads and evicts them), so the table sits behind a lock
//! rather than being baked into an immutable axum route set.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use serde_json::json;
use tower::ServiceExt;

use crate::error::{ok_data, ApiError};
use crate::{build_router, host_guard, AppState};

/// The hub's table of mounted projects. Cheap to clone; clones share the table.
#[derive(Clone, Default)]
pub struct HubWeb {
    projects: Arc<RwLock<BTreeMap<String, Mounted>>>,
}

struct Mounted {
    name: String,
    root: Utf8PathBuf,
    router: Router,
    /// The project's live-update watcher; dropped (and so stopped) on unmount.
    _watcher: Option<notify::RecommendedWatcher>,
}

/// One project as `GET /api/v1/projects` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectEntry {
    pub slug: String,
    pub name: String,
    pub root: String,
    /// The project's app path, e.g. `/p/clove/`.
    pub url: String,
}

impl HubWeb {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount the project rooted at `root` and return its slug. Its file
    /// watcher (live updates) runs until the project is unmounted.
    ///
    /// The slug is the repo directory's name, so bookmarks survive a hub
    /// restart; only when that name is already taken by another loaded project
    /// does it gain a suffix derived from the path.
    pub fn mount(&self, root: &Utf8Path, state: AppState) -> String {
        let name = root.file_name().unwrap_or("project").to_owned();
        let mut projects = self.projects.write().unwrap_or_else(|e| e.into_inner());
        let slug = unique_slug(&projects, &name, root);
        let state = state.with_base_path(&format!("/p/{slug}"));
        let watcher = crate::watch::spawn(state.clone());
        projects.insert(
            slug.clone(),
            Mounted {
                name,
                root: root.to_owned(),
                router: build_router(state),
                _watcher: watcher,
            },
        );
        slug
    }

    /// Remove a project; its URLs answer 404 from now on.
    pub fn unmount(&self, slug: &str) {
        self.projects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(slug);
    }

    /// Every mounted project, by slug.
    pub fn projects(&self) -> Vec<ProjectEntry> {
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(slug, m)| ProjectEntry {
                slug: slug.clone(),
                name: m.name.clone(),
                root: m.root.to_string(),
                url: format!("/p/{slug}/"),
            })
            .collect()
    }

    /// The hub router: the project listing, the root page, and prefix dispatch.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(root_page))
            .route("/api/v1/projects", get(list_projects))
            .fallback(dispatch)
            .layer(axum::middleware::from_fn(host_guard))
            .with_state(self.clone())
    }

    /// Serve the hub router on `listener` until it fails.
    pub async fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }

    fn router_for(&self, slug: &str) -> Option<Router> {
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(slug)
            .map(|m| m.router.clone())
    }

    /// The slug when exactly one project is mounted.
    fn only_slug(&self) -> Option<String> {
        let projects = self.projects.read().unwrap_or_else(|e| e.into_inner());
        match projects.len() {
            1 => projects.keys().next().cloned(),
            _ => None,
        }
    }
}

async fn list_projects(State(hub): State<HubWeb>) -> Response {
    ok_data(json!({ "projects": hub.projects() }))
}

/// `/`: straight into the only project, else a picker.
async fn root_page(State(hub): State<HubWeb>) -> Response {
    if let Some(slug) = hub.only_slug() {
        return Redirect::temporary(&format!("/p/{slug}/")).into_response();
    }
    Html(picker_html(&hub.projects())).into_response()
}

async fn dispatch(State(hub): State<HubWeb>, mut request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();

    let Some(rest) = path.strip_prefix("/p/") else {
        // Before the hub, the single daemon served its project at the root.
        // With one project loaded that is still unambiguous, so old bookmarks
        // and scripts keep working; with several it is not.
        return match hub.only_slug() {
            Some(slug) => Redirect::temporary(&format!("/p/{slug}{path}{query}")).into_response(),
            None => not_found("no project is addressed; open / to pick one"),
        };
    };
    let (slug, tail) = match rest.split_once('/') {
        Some((slug, tail)) => (slug, Some(tail)),
        None => (rest, None),
    };
    let Some(router) = hub.router_for(slug) else {
        return not_found(&format!("no project `{slug}` is loaded"));
    };
    let Some(tail) = tail else {
        return Redirect::temporary(&format!("/p/{slug}/{query}")).into_response();
    };
    let Ok(uri) = format!("/{tail}{query}").parse::<Uri>() else {
        return (StatusCode::BAD_REQUEST, "bad request path").into_response();
    };
    *request.uri_mut() = uri;
    match router.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn not_found(message: &str) -> Response {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "PROJECT_NOT_FOUND",
        exit: 2,
        message: message.to_owned(),
    }
    .into_response()
}

fn unique_slug(taken: &BTreeMap<String, Mounted>, name: &str, root: &Utf8Path) -> String {
    let base = slugify(name);
    if !taken.contains_key(&base) {
        return base;
    }
    let hash = path_hash(root);
    let mut candidate = format!("{base}-{}", &hash[..6]);
    let mut n = 2;
    while taken.contains_key(&candidate) {
        candidate = format!("{base}-{}-{n}", &hash[..6]);
        n += 1;
    }
    candidate
}

/// Lower-case ASCII alphanumerics and single dashes — safe in a URL path
/// segment and in the injected JS string without escaping.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "project".to_owned()
    } else {
        slug.to_owned()
    }
}

/// FNV-1a over the path: a stable collision suffix across hub restarts.
fn path_hash(path: &Utf8Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn picker_html(projects: &[ProjectEntry]) -> String {
    let body = if projects.is_empty() {
        "<p class=\"empty\">No project is loaded. Run <code>clove daemon start</code> \
         (or <code>clove serve</code>) inside a clove project.</p>"
            .to_owned()
    } else {
        let items: String = projects
            .iter()
            .map(|p| {
                format!(
                    "<li><a href=\"{url}\"><span class=\"name\">{name}</span>\
                     <span class=\"root\">{root}</span></a></li>",
                    url = escape(&p.url),
                    name = escape(&p.name),
                    root = escape(&p.root),
                )
            })
            .collect();
        format!("<ul>{items}</ul>")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"dark light\"><title>clove</title>\
         <style>{PICKER_CSS}</style></head><body><main><h1>clove</h1>{body}</main>\
         </body></html>"
    )
}

const PICKER_CSS: &str = "body{margin:0;font:14px/1.5 system-ui,sans-serif;\
background:#0f1115;color:#d7dae0}main{max-width:640px;margin:48px auto;padding:0 16px}\
h1{font-size:18px;margin:0 0 16px}ul{list-style:none;margin:0;padding:0}\
li a{display:flex;flex-direction:column;padding:10px 12px;margin:0 0 8px;\
border:1px solid #2a2f3a;border-radius:8px;color:inherit;text-decoration:none}\
li a:hover{border-color:#6c8cff}.name{font-weight:600}\
.root{font-size:12px;color:#8b93a3;overflow-wrap:anywhere}\
code{font-family:ui-monospace,monospace}.empty{color:#8b93a3}\
@media (prefers-color-scheme:light){body{background:#fafafa;color:#1d2330}\
li a{border-color:#d9dde5}.root,.empty{color:#5d6576}}";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_keeps_a_url_safe_name() {
        assert_eq!(slugify("clove"), "clove");
        assert_eq!(slugify("My Repo (2)"), "my-repo-2");
        assert_eq!(slugify("..."), "project");
    }

    #[test]
    fn picker_escapes_names() {
        let html = picker_html(&[ProjectEntry {
            slug: "x".into(),
            name: "<script>".into(),
            root: "/a&b".into(),
            url: "/p/x/".into(),
        }]);
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("/a&amp;b"));
        assert!(!html.contains("<script>"));
    }
}
