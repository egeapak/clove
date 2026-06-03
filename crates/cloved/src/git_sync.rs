//! Git auto-sync (T-D06, DESIGN §8.7). Compiled only under the default-on
//! `git-sync` feature so a `--no-default-features` build is verifiably free of
//! vendored libgit2 (M3_PLAN §1 / Phase 0 gate).
//!
//! **Phase 0 (this commit):** feature scaffolding only. The real opt-in
//! auto-commit logic — skip guards (active merge/rebase, malformed frontmatter),
//! `synced_at` re-commit suppression, never-push — lands in Phase 5.

/// True when this binary was built with git auto-sync support. A feature-less
/// build returns `false` and the daemon fails fast if `[daemon] git_sync = true`.
#[allow(dead_code)] // wired into the lifecycle in Phase 5 (T-D06)
pub fn available() -> bool {
    true
}

/// Ties the `git2` dependency to the `git-sync` feature so the Phase 0 gate can
/// prove a `--no-default-features` build links no libgit2, while the default
/// build does. Returns the bundled libgit2 version.
#[allow(dead_code)]
pub(crate) fn libgit2_version() -> (u32, u32, u32) {
    git2::Version::get().libgit2_version()
}
