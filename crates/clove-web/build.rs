//! Build script: produce the embedded SvelteKit SPA (`dist/`).
//!
//! `dist/` is **not** committed — this script generates it:
//! - If `npm` is available and a source is newer than `dist/`, run `npm run
//!   build` to produce the real UI.
//! - Otherwise (no npm, e.g. CI's Node-free Rust matrix, or a build failure) we
//!   ensure a minimal placeholder `dist/index.html` exists so `rust-embed` still
//!   compiles and the binary serves a "run npm build" page (the JSON API is fully
//!   functional regardless). `cargo build` therefore never requires Node.
//!
//! Set `CLOVE_SKIP_WEB_BUILD=1` to skip the npm build (the placeholder/committed
//! `dist/` is used as-is).

use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    let web = Path::new("web");
    let dist = Path::new("dist");
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

    // Always guarantee dist/index.html exists so the rust-embed macro compiles,
    // even before (or without) a real frontend build.
    ensure_placeholder(dist);

    if std::env::var_os("CLOVE_SKIP_WEB_BUILD").is_some() {
        return;
    }
    if !web.join("package.json").exists() {
        return; // no frontend project checked out; placeholder stands
    }
    if !npm_available() {
        println!("cargo:warning=npm not found; serving a placeholder web UI (run `npm run build` in crates/clove-web/web for the real UI)");
        return;
    }

    // Skip the npm build when the committed/previous dist is already up to date.
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
        println!("cargo:warning=clove-web: `npm install` failed; serving the placeholder web UI");
        return;
    }
    if !run(web, &["run", "build"]) {
        println!("cargo:warning=clove-web: `npm run build` failed; serving the placeholder web UI");
    }
}

/// Write a minimal `dist/index.html` if none exists (so `rust-embed` compiles).
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

/// The mtime of the real built entry (hashed assets dir), ignoring a bare
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
