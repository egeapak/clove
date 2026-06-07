//! Build script: produce the embedded SvelteKit SPA and gzip it for embedding.
//!
//! Pipeline: ensure a real (or placeholder) `dist/` exists, then mirror it into
//! `dist-gz/` as gzip-compressed files (`<path>.gz`). The crate embeds **only**
//! `dist-gz/` (via rust-embed) and decompresses it into memory once at startup —
//! so the binary carries the small gzip blob (not the larger uncompressed assets)
//! and we never link a brotli/zstd library. Both `dist/` and `dist-gz/` are
//! git-ignored and generated here.
//!
//! `dist/` is built with `npm run build` when `npm` is available and a source is
//! newer; otherwise a minimal placeholder `index.html` is used so a Node-free
//! `cargo build` still compiles. `CLOVE_SKIP_WEB_BUILD=1` skips the npm build.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use flate2::write::GzEncoder;
use flate2::Compression;

fn main() {
    let web = Path::new("web");
    let dist = Path::new("dist");
    let dist_gz = Path::new("dist-gz");
    for p in [
        "web/src",
        "web/package.json",
        "web/svelte.config.js",
        "web/vite.config.ts",
        "web/tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-env-changed=CLOVE_SKIP_WEB_BUILD");

    ensure_placeholder(dist);
    maybe_npm_build(web, dist);
    // Always (re)generate the embedded gzip mirror from the finalized dist.
    gzip_tree(dist, dist_gz);
}

/// Build `dist/` with npm when possible; otherwise leave the placeholder/previous
/// build in place. Never fails the Rust build.
fn maybe_npm_build(web: &Path, dist: &Path) {
    if std::env::var_os("CLOVE_SKIP_WEB_BUILD").is_some() {
        return;
    }
    if !web.join("package.json").exists() {
        return;
    }
    if !npm_available() {
        println!("cargo:warning=npm not found; embedding a placeholder web UI (run `npm run build` in crates/clove-web/web for the real UI)");
        return;
    }
    // Skip the npm build when the previous dist is already up to date.
    let dist_stamp = real_dist_mtime(dist);
    let src_newest = newest_mtime(web.join("src"))
        .max(mtime(&web.join("package.json")))
        .max(mtime(&web.join("svelte.config.js")))
        .max(mtime(&web.join("vite.config.ts")));
    if let (Some(d), Some(s)) = (dist_stamp, src_newest) {
        if d >= s {
            return;
        }
    }
    if !web.join("node_modules").exists() && !run(web, &["install", "--no-audit", "--no-fund"]) {
        println!("cargo:warning=clove-web: `npm install` failed; embedding the placeholder web UI");
        return;
    }
    if !run(web, &["run", "build"]) {
        println!(
            "cargo:warning=clove-web: `npm run build` failed; embedding the placeholder web UI"
        );
    }
}

/// Mirror every file under `src` into `dst` as a gzip-compressed `<name>.gz`.
fn gzip_tree(src: &Path, dst: &Path) {
    let _ = std::fs::remove_dir_all(dst);
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(rel) = path.strip_prefix(src) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let out = dst.join(rel).with_added_gz();
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut enc = GzEncoder::new(Vec::new(), Compression::best());
            if enc.write_all(&bytes).is_ok() {
                if let Ok(gz) = enc.finish() {
                    let _ = std::fs::write(&out, gz);
                }
            }
        }
    }
}

/// Helper to append a `.gz` suffix to a path.
trait AddGz {
    fn with_added_gz(&self) -> std::path::PathBuf;
}
impl AddGz for Path {
    fn with_added_gz(&self) -> std::path::PathBuf {
        let mut s = self.as_os_str().to_os_string();
        s.push(".gz");
        std::path::PathBuf::from(s)
    }
}
impl AddGz for std::path::PathBuf {
    fn with_added_gz(&self) -> std::path::PathBuf {
        self.as_path().with_added_gz()
    }
}

/// Write a minimal `dist/index.html` if none exists (so the gzip mirror and the
/// rust-embed macro always have something to embed).
fn ensure_placeholder(dist: &Path) {
    let index = dist.join("index.html");
    if index.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dist);
    let _ = std::fs::write(
        &index,
        "<!doctype html><html lang=\"en\" data-theme=\"midnight-ide\"><head>\
<meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>clove</title><style>html{background:#0d1117;color:#e6edf3;font-family:system-ui,sans-serif}\
body{display:grid;place-items:center;height:100vh;margin:0}code{color:#58a6ff}</style></head>\
<body><main><h1>clove web UI</h1><p>The SPA was not built. Run \
<code>npm run build</code> in <code>crates/clove-web/web</code> (or build with npm available).</p>\
<p>The JSON API is live at <code>/api/v1</code>.</p></main></body></html>\n",
    );
}

fn npm_available() -> bool {
    Command::new("npm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run(dir: &Path, args: &[&str]) -> bool {
    Command::new("npm")
        .args(args)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// The mtime of the real built entry (the hashed assets dir), ignoring a bare
/// placeholder `index.html` so a placeholder never counts as "up to date".
fn real_dist_mtime(dist: &Path) -> Option<SystemTime> {
    if dist.join("_app").exists() {
        mtime(&dist.join("index.html"))
    } else {
        None
    }
}

/// The newest modification time anywhere under `dir` (recursively).
fn newest_mtime(dir: std::path::PathBuf) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
            } else if let Some(t) = mtime(&path) {
                newest = Some(newest.map_or(t, |n| n.max(t)));
            }
        }
    }
    newest
}
