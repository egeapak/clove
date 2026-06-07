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

use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

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
pub async fn static_handler(headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Anything under /api that reached the fallback is a genuine 404.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let map = table();
    let candidate = if path.is_empty() { "index.html" } else { path };
    let asset = map.get(candidate).or_else(|| map.get("index.html")); // SPA fallback

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
