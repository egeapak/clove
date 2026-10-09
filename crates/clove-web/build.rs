//! Build script: produce the embedded SvelteKit SPA and gzip it for embedding.
//!
//! The crate embeds **only** `$OUT_DIR/dist-gz/`: every asset gzip-compressed as
//! `<path>.gz`, listed in a generated `$OUT_DIR/assets_table.rs` of `include_bytes!`
//! entries and decompressed into memory once at startup — so the binary carries the
//! small gzip blob and we never link a brotli/zstd library.
//! Everything is generated into `OUT_DIR`; Cargo's publish verification rejects a
//! build script that writes into the package source.
//!
//! Where the SPA comes from:
//!
//! * **In the repository** (`web/package.json` exists): `dist/` is built with
//!   `npm run build` when `npm` is available and a source is newer, else a
//!   minimal placeholder `index.html` is used so a Node-free `cargo build` still
//!   compiles. `CLOVE_SKIP_WEB_BUILD=1` skips the npm build.
//! * **In a published package** (no `web/`): the built SPA is downloaded from
//!   this version's GitHub Release (`clove-web-dist-v<version>.tar.gz` plus its
//!   `.sha256`), verified, and extracted — no Node on the user's machine. A
//!   failed download or a checksum mismatch fails the build; it never falls back
//!   to the placeholder. Two escapes: `CLOVE_WEB_DIST_DIR` points at an already
//!   built `dist/` (offline and packager builds), and `DOCS_RS` embeds the
//!   placeholder because docs.rs builds without network access.
//!   `CLOVE_WEB_DIST_BASE_URL` serves the assets from a mirror instead.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use flate2::write::GzEncoder;
use flate2::Compression;

#[path = "build/fetch.rs"]
mod fetch;

fn main() {
    let web = Path::new("web");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let dist_gz = out.join("dist-gz");
    for p in [
        "build.rs",
        "build/fetch.rs",
        "web/src",
        "web/static",
        "web/package.json",
        "web/package-lock.json",
        "web/svelte.config.js",
        "web/vite.config.ts",
        "web/tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    for var in [
        "CLOVE_SKIP_WEB_BUILD",
        "CLOVE_WEB_DIST_DIR",
        "CLOVE_WEB_DIST_BASE_URL",
        "DOCS_RS",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let dist = if web.join("package.json").exists() {
        let dist = PathBuf::from("dist");
        ensure_placeholder(&dist);
        maybe_npm_build(web, &dist);
        dist
    } else {
        let dist = out.join("dist");
        acquire_packaged(&dist);
        dist
    };
    gzip_tree(&dist, &dist_gz).unwrap_or_else(|e| {
        fail(&format!(
            "could not compress the web UI into {}: {e}",
            dist_gz.display()
        ))
    });
    write_asset_table(&dist_gz, &out.join("assets_table.rs"))
        .unwrap_or_else(|e| fail(&format!("could not write the embedded asset table: {e}")));
}

/// Write `EMBEDDED: &[(&str, &[u8])]`, one `include_bytes!` entry per file under
/// `dist_gz`, sorted by path so the output is deterministic. Paths are relative
/// to `dist_gz` and use `/`.
fn write_asset_table(dist_gz: &Path, table: &Path) -> std::io::Result<()> {
    let mut files = Vec::new();
    let mut stack = vec![dist_gz.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dist_gz) {
                let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
                files.push(rel.join("/"));
            }
        }
    }
    files.sort();
    let mut code = String::from("pub static EMBEDDED: &[(&str, &[u8])] = &[\n");
    for rel in &files {
        code.push_str(&format!(
            "    ({rel:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/dist-gz/\", {rel:?}))),\n"
        ));
    }
    code.push_str("];\n");
    std::fs::write(table, code)
}

/// The packaged-crate case: put a built SPA in `dist`, or fail the build.
fn acquire_packaged(dist: &Path) {
    let _ = std::fs::remove_dir_all(dist);
    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo:warning=clove-web: building for docs.rs; embedding the placeholder web UI");
        ensure_placeholder(dist);
        return;
    }
    if let Some(dir) = std::env::var_os("CLOVE_WEB_DIST_DIR") {
        println!("cargo:rerun-if-changed={}", Path::new(&dir).display());
        copy_tree(Path::new(&dir), dist).unwrap_or_else(|e| {
            fail(&format!(
                "CLOVE_WEB_DIST_DIR={}: {e}",
                Path::new(&dir).display()
            ))
        });
        if !dist.join("index.html").is_file() {
            fail("CLOVE_WEB_DIST_DIR has no index.html at its root");
        }
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let base = fetch::base_url(
        &version,
        std::env::var("CLOVE_WEB_DIST_BASE_URL").ok().as_deref(),
    );
    let (tarball_name, checksum_name) = fetch::asset_names(&version);
    let result = (|| -> Result<usize, String> {
        let checksum = download(&format!("{base}/{checksum_name}"), 4096)?;
        let expected = fetch::parse_sha256(&String::from_utf8_lossy(&checksum))?;
        let tarball = download(&format!("{base}/{tarball_name}"), fetch::MAX_DOWNLOAD)?;
        fetch::verify(&tarball, &expected)?;
        std::fs::create_dir_all(dist).map_err(|e| e.to_string())?;
        fetch::extract(&tarball, dist)
    })();
    match result {
        Ok(files) => {
            println!("cargo:warning=clove-web: fetched the web UI ({files} files) from {base}")
        }
        Err(e) => fail(&format!(
            "could not fetch the web UI for clove-web {version} from {base}: {e}"
        )),
    }
}

/// Fail the build with a message that says what to do about it.
fn fail(reason: &str) -> ! {
    eprintln!(
        "error: clove-web: {reason}\n\
         \n\
         The `web` feature embeds the built web UI. Either build without the `web` \
         feature, install a release binary from https://github.com/egeapak/clove/releases, \
         or point CLOVE_WEB_DIST_DIR at an already built `dist/` directory."
    );
    std::process::exit(1);
}

/// GET `url`, refusing anything over `limit` bytes.
fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    fetch::check_url(url)?;
    // An https URL stays https across redirects; only a loopback http mirror
    // (already vetted by `check_url`) may be plain.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .https_only(url.starts_with("https://"))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

/// Copy a directory tree (regular files only).
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        // `Path::is_dir` follows symlinks, so a linked subdirectory is copied too.
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
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
        // Distinguish embedding a real (but possibly stale) prior build from
        // embedding the placeholder — ensure_placeholder keeps an existing
        // dist/index.html, so a previous real build is what actually ships.
        if real_dist_mtime(dist).is_some() {
            println!("cargo:warning=clove-web: npm not found; keeping the existing (possibly stale) web UI build");
        } else {
            println!("cargo:warning=clove-web: npm not found; embedding a placeholder web UI (run `npm run build` in crates/clove-web/web for the real UI)");
        }
        return;
    }
    // Skip the npm build when the previous dist is already up to date. The
    // staleness inputs must mirror the rerun-if-changed set above: a lock-only
    // dependency bump (package-lock.json) or a static-asset change (web/static/,
    // copied into dist by adapter-static) must force a rebuild.
    let dist_stamp = real_dist_mtime(dist);
    let src_newest = newest_mtime(web.join("src"))
        .max(newest_mtime(web.join("static")))
        .max(mtime(&web.join("package.json")))
        .max(mtime(&web.join("package-lock.json")))
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
fn gzip_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_dir_all(dst);
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(src)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let bytes = std::fs::read(&path)?;
            let out = dst.join(rel).with_added_gz();
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut enc = GzEncoder::new(Vec::new(), Compression::best());
            enc.write_all(&bytes)?;
            std::fs::write(&out, enc.finish()?)?;
        }
    }
    Ok(())
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
/// embedded asset table always has something to embed).
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
