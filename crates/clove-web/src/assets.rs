//! Serve the embedded SvelteKit build with an SPA fallback.
//!
//! In release builds the `dist/` tree is baked into the binary; in debug builds
//! `rust-embed` reads it from disk so the frontend can be rebuilt without
//! recompiling Rust. Any unmatched non-`/api` path falls back to `index.html` so
//! client-side routes (`/board`, `/items/:id`, …) deep-link correctly.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "dist/"]
struct Assets;

/// Static + SPA-fallback handler (registered as the router fallback).
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Anything under /api that reached the fallback is a genuine 404.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let candidate = if path.is_empty() { "index.html" } else { path };
    if let Some(content) = Assets::get(candidate) {
        return serve(candidate, content.data.into_owned());
    }
    // SPA fallback.
    match Assets::get("index.html") {
        Some(content) => serve("index.html", content.data.into_owned()),
        None => (StatusCode::NOT_FOUND, "index.html missing from build").into_response(),
    }
}

fn serve(path: &str, body: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Hashed assets are immutable; the entry HTML must always revalidate.
    let cache = if path.starts_with("_app/") || path.contains("immutable") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}
