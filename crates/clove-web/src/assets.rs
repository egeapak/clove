//! Serve the embedded SvelteKit build with an SPA fallback.
//!
//! Only the **gzip-compressed** assets are embedded (`build.rs` mirrors `dist/`
//! into `dist-gz/` as `<path>.gz`). At startup we decompress each once into an
//! in-memory table holding **both** the gzip bytes and the decompressed bytes, so
//! every request is served from memory with zero per-request compression:
//! gzip-capable clients get the stored gzip bytes (`Content-Encoding: gzip`),
//! others get the decompressed bytes. Embedding gzip (not the larger raw assets)
//! and decompressing with pure-Rust `flate2`/miniz_oxide keeps the binary small
//! and free of any brotli/zstd library.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::AppState;

#[derive(RustEmbed)]
#[folder = "dist-gz/"]
struct Assets;

/// One asset, both forms resident in memory.
struct Asset {
    gz: Vec<u8>,
    raw: Vec<u8>,
    mime: String,
    cache: &'static str,
}

static TABLE: OnceLock<HashMap<String, Asset>> = OnceLock::new();

/// Decompress every embedded `*.gz` once into the in-memory table.
fn table() -> &'static HashMap<String, Asset> {
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for path in Assets::iter() {
            let Some(logical) = path.strip_suffix(".gz") else {
                continue;
            };
            let Some(file) = Assets::get(&path) else {
                continue;
            };
            let gz = file.data.into_owned();
            let raw = gunzip(&gz);
            let mime = mime_guess::from_path(logical)
                .first_or_octet_stream()
                .to_string();
            let cache = cache_for(logical);
            map.insert(
                logical.to_owned(),
                Asset {
                    gz,
                    raw,
                    mime,
                    cache,
                },
            );
        }
        map
    })
}

/// Force the decompress-into-memory step at server start (so the first request
/// isn't the one that pays for it).
pub fn warm() {
    let _ = table();
}

/// The SPA entry page rewritten to run under `base` (e.g. `/p/clove`), or
/// `None` when the build has no entry page.
///
/// SvelteKit's fallback page boots from an inline
/// `__sveltekit_<hash> = { base: "" }` and reads its runtime base from that
/// global, so setting it there moves every `{base}` link, the router, and the
/// lazily-loaded chunks under the prefix. The page's own absolute `/_app/`
/// preloads move with it. A page without the global (the Node-free
/// placeholder) is returned unchanged.
pub fn index_for_base(base: &str) -> Option<Vec<u8>> {
    let raw = String::from_utf8_lossy(&table().get("index.html")?.raw).into_owned();
    Some(rewrite_base(&raw, base).into_bytes())
}

fn rewrite_base(page: &str, base: &str) -> String {
    let Some(global) = page.find("__sveltekit_") else {
        return page.to_owned();
    };
    let quoted = serde_json::to_string(base).unwrap_or_else(|_| "\"\"".to_owned());
    let (head, tail) = page.split_at(global);
    let tail = tail.replacen(r#"base: """#, &format!("base: {quoted}"), 1);
    let assets = format!("\"{base}/_app/");
    format!("{head}{tail}").replace("\"/_app/", &assets)
}

fn gunzip(gz: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = flate2::read::GzDecoder::new(gz).read_to_end(&mut out);
    out
}

/// Hashed assets are immutable; the entry HTML must always revalidate.
fn cache_for(path: &str) -> &'static str {
    if path.starts_with("_app/") || path.contains("immutable") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Static + SPA-fallback handler (registered as the router fallback).
pub async fn static_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Anything under /api that reached the fallback is a genuine 404.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let map = table();
    let candidate = if path.is_empty() { "index.html" } else { path };
    let asset = map.get(candidate);

    // The SPA entry page. Under a hub prefix it is the rewritten copy, which
    // is never cached: the prefix is per-project, the build is not.
    if asset.is_none() || candidate == "index.html" {
        if let Some(page) = state.index_page() {
            return (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                page.to_vec(),
            )
                .into_response();
        }
    }
    let asset = asset.or_else(|| map.get("index.html")); // SPA fallback

    let Some(asset) = asset else {
        return (StatusCode::NOT_FOUND, "index.html missing from build").into_response();
    };

    if accepts_gzip(&headers) {
        (
            [
                (header::CONTENT_TYPE, asset.mime.as_str()),
                (header::CACHE_CONTROL, asset.cache),
                (header::CONTENT_ENCODING, "gzip"),
                (header::VARY, "Accept-Encoding"),
            ],
            asset.gz.clone(),
        )
            .into_response()
    } else {
        (
            [
                (header::CONTENT_TYPE, asset.mime.as_str()),
                (header::CACHE_CONTROL, asset.cache),
                (header::VARY, "Accept-Encoding"),
            ],
            asset.raw.clone(),
        )
            .into_response()
    }
}

/// Whether the client advertised gzip support (ignoring an explicit `gzip;q=0`).
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|part| {
        let mut it = part.split(';');
        let token = it.next().unwrap_or("").trim();
        let not_disabled = !it.any(|p| p.trim().replace(' ', "") == "q=0");
        (token == "gzip" || token == "x-gzip") && not_disabled
    })
}

#[cfg(test)]
mod tests {
    use super::rewrite_base;

    const PAGE: &str = r#"<link href="/_app/immutable/entry/start.js" rel="modulepreload">
<script>
  __sveltekit_abc123 = {
    base: ""
  };
  import("/_app/immutable/entry/app.js");
</script>"#;

    #[test]
    fn rewrite_sets_the_runtime_base_and_prefixes_assets() {
        let out = rewrite_base(PAGE, "/p/clove");
        assert!(out.contains(r#"base: "/p/clove""#), "{out}");
        assert!(out.contains(r#"href="/p/clove/_app/immutable/entry/start.js""#));
        assert!(out.contains(r#"import("/p/clove/_app/immutable/entry/app.js")"#));
        assert!(!out.contains(r#""/_app/"#));
    }

    #[test]
    fn rewrite_leaves_a_page_without_the_global_alone() {
        let placeholder = "<html><body>clove</body></html>";
        assert_eq!(rewrite_base(placeholder, "/p/clove"), placeholder);
    }

    #[test]
    fn rewrite_escapes_the_base_as_a_js_string() {
        let out = rewrite_base(PAGE, r#"/p/a"b"#);
        assert!(out.contains(r#"base: "/p/a\"b""#), "{out}");
    }
}
