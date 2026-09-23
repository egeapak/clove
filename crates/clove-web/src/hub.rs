//! The daemon hub's web front (DESIGN §8.10): one listener serving every loaded
//! project, each under its own `/p/<slug>/` prefix.
//!
//! A project's router is the ordinary [`build_router`] over its own
//! [`AppState`]; this layer only picks which one a request belongs to and strips
//! the prefix before handing it over. Projects mount and unmount while the
//! server runs (the hub loads and evicts them), so the table sits behind a lock
//! rather than being baked into an immutable axum route set.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use serde_json::json;
use tower::ServiceExt;

use crate::error::{ok_data, ApiError};
use crate::{build_router, host_guard, nosniff, AppState};

/// The hub's table of mounted projects. Cheap to clone; clones share the table.
#[derive(Clone, Default)]
pub struct HubWeb {
    projects: Arc<RwLock<Registry>>,
}

#[derive(Default)]
struct Registry {
    mounted: BTreeMap<String, Mounted>,
    /// Every slug handed out, by the repository it went to — to catch the
    /// (astronomically unlikely) short-hash collision between two paths.
    assigned: HashMap<Utf8PathBuf, String>,
}

struct Mounted {
    name: String,
    root: Utf8PathBuf,
    router: Router,
    /// Closes the project's event sockets on unmount.
    state: AppState,
    /// The project's live-update watcher, once armed; dropped (and so
    /// stopped) on unmount.
    watcher: Option<notify::RecommendedWatcher>,
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

    /// Mount the project rooted at `root` (its canonical path) and return its
    /// slug. Its file watcher (live updates) runs until the project is
    /// unmounted.
    ///
    /// The watcher is armed on a thread of its own, not here: FSEvents setup
    /// can take seconds on a busy Mac, and neither the project's load nor the
    /// other projects' requests (this takes the registry lock) may wait for
    /// it. Once armed it announces a change, so a page opened meanwhile
    /// refetches whatever it missed.
    ///
    /// The slug is `<name>-<hash of the path>`: a function of the repository
    /// alone, never of what else is loaded or in which order. So it survives
    /// unmount and a hub restart, and a tab left open on one repository's URL
    /// can never reach another repository that later loads under the same name.
    pub fn mount(&self, root: &Utf8Path, state: AppState) -> String {
        let name = root.file_name().unwrap_or("project").to_owned();
        let mut registry = self.projects.write().unwrap_or_else(|e| e.into_inner());
        let slug = match registry.assigned.get(root) {
            Some(slug) => slug.clone(),
            None => {
                let slug = slug_for(&registry.assigned, &name, root);
                registry.assigned.insert(root.to_owned(), slug.clone());
                slug
            }
        };
        let state = state.with_base_path(&format!("/p/{slug}"));
        let previous = registry.mounted.insert(
            slug.clone(),
            Mounted {
                name,
                root: root.to_owned(),
                router: build_router(state.clone()),
                state: state.clone(),
                watcher: None,
            },
        );
        drop(registry);
        if let Some(previous) = previous {
            retire(previous);
        }
        let (hub, armed_slug) = (self.clone(), slug.clone());
        std::thread::spawn(move || hub.arm_watcher(&armed_slug, state));
        slug
    }

    /// Arm the live-update watcher of the mount at `slug` made with `state`
    /// — unless that mount is gone by the time it is armed.
    fn arm_watcher(&self, slug: &str, state: AppState) {
        let Some(watcher) = crate::watch::spawn(state.clone()) else {
            return;
        };
        let mut registry = self.projects.write().unwrap_or_else(|e| e.into_inner());
        match registry.mounted.get_mut(slug) {
            Some(mounted) if mounted.state.is_same_mount(&state) => {
                mounted.watcher = Some(watcher);
                drop(registry);
                crate::watch::announce_change(&state);
            }
            // Unmounted (or mounted afresh) while this one armed.
            _ => {
                drop(registry);
                drop(watcher);
            }
        }
    }

    /// Remove a project: its URLs answer 404 from now on, and its open event
    /// sockets close.
    pub fn unmount(&self, slug: &str) {
        let removed = self
            .projects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .mounted
            .remove(slug);
        if let Some(mounted) = removed {
            retire(mounted);
        }
    }

    /// Every mounted project, by slug.
    pub fn projects(&self) -> Vec<ProjectEntry> {
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mounted
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
            .layer(axum::middleware::from_fn(nosniff))
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
            .mounted
            .get(slug)
            .map(|m| m.router.clone())
    }

    /// The slug when exactly one project is mounted.
    fn only_slug(&self) -> Option<String> {
        let registry = self.projects.read().unwrap_or_else(|e| e.into_inner());
        match registry.mounted.len() {
            1 => registry.mounted.keys().next().cloned(),
            _ => None,
        }
    }
}

/// Close a mount's event sockets and stop its watcher — the latter on a thread
/// of its own: stopping an FSEvents stream blocks, and the hub's teardown of
/// the project (which the next load of it waits for) must not.
fn retire(mounted: Mounted) {
    mounted.state.close();
    if mounted.watcher.is_some() {
        std::thread::spawn(move || drop(mounted));
    }
}

async fn list_projects(State(hub): State<HubWeb>) -> Response {
    ok_data(json!({ "projects": hub.projects() }))
}

/// `/`: straight into the only project, else a picker.
async fn root_page(State(hub): State<HubWeb>, headers: HeaderMap) -> Response {
    if let Some(slug) = hub.only_slug() {
        return Redirect::temporary(&format!("/p/{slug}/")).into_response();
    }
    (
        [(
            header::CONTENT_SECURITY_POLICY,
            crate::assets::content_security_policy(&headers, "'none'"),
        )],
        Html(picker_html(&hub.projects())),
    )
        .into_response()
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

/// `<name>-<first 8 hex of the path hash>`; the full hash should those 8
/// collide with another path's.
fn slug_for(assigned: &HashMap<Utf8PathBuf, String>, name: &str, root: &Utf8Path) -> String {
    let hash = path_hash(root);
    let short = format!("{}-{}", slugify(name), &hash[..8]);
    if assigned.values().any(|slug| *slug == short) {
        format!("{}-{hash}", slugify(name))
    } else {
        short
    }
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

/// FNV-1a over the path: the same on every hub, run after run.
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
