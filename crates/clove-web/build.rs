//! Build script: rebuild the embedded SvelteKit SPA when its sources change.
//!
//! Design constraints:
//! - **Never breaks a Node-free build.** If `npm` is absent (e.g. CI's Rust
//!   matrix), or the build fails, we emit a warning and fall back to the
//!   committed `dist/`. `cargo build` stays hermetic.
//! - **Only rebuilds when needed.** We run `npm run build` only if a source file
//!   is newer than `dist/index.html` (so a fresh checkout with an up-to-date
//!   committed `dist/` does nothing). Set `CLOVE_SKIP_WEB_BUILD=1` to force-skip.

use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    let web = Path::new("web");
    // Re-run when any of these change.
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

    if std::env::var_os("CLOVE_SKIP_WEB_BUILD").is_some() {
        return;
    }
    if !web.join("package.json").exists() {
        return; // no frontend project checked out; use committed dist
    }
    if !npm_available() {
        println!("cargo:warning=npm not found; using the committed clove-web/dist (run `npm run build` in crates/clove-web/web to refresh)");
        return;
    }

    let dist_stamp = mtime(Path::new("dist/index.html"));
    let src_newest = newest_mtime(web.join("src"))
        .max(mtime(&web.join("package.json")))
        .max(mtime(&web.join("svelte.config.js")))
        .max(mtime(&web.join("vite.config.ts")));

    // Up-to-date committed build → nothing to do.
    if let (Some(dist), Some(src)) = (dist_stamp, src_newest) {
        if dist >= src {
            return;
        }
    }

    if !web.join("node_modules").exists() && !run(web, &["install", "--no-audit", "--no-fund"]) {
        println!("cargo:warning=clove-web: `npm install` failed; using the committed dist");
        return;
    }
    if !run(web, &["run", "build"]) {
        println!("cargo:warning=clove-web: `npm run build` failed; using the committed dist");
    }
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
