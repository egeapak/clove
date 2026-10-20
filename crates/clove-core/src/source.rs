//! Where to get the source of this clove build (AGPL-3.0 section 13).
//!
//! clove is AGPL-3.0-only. Section 13 asks that a modified version which users
//! reach over a network (`clove serve`) offers those users its source. This is
//! the single place that answers "where is the source": the web UI links to it
//! (`/api/v1/meta` → `source_code`) and `clove version` prints it.
//!
//! A fork sets `CLOVE_SOURCE_URL` at build time to point at its own source; the
//! default is this exact release's tree in the upstream repository.

/// The SPDX id of clove's license.
pub const LICENSE: &str = "AGPL-3.0-only";

/// The version this build reports next to its source.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The URL of the source of this build: `$CLOVE_SOURCE_URL` at build time, or
/// the upstream tree of this version's tag.
pub fn source_url() -> String {
    match option_env!("CLOVE_SOURCE_URL") {
        Some(url) if !url.trim().is_empty() => url.trim().to_owned(),
        _ => format!("{}/tree/v{VERSION}", env!("CARGO_PKG_REPOSITORY")),
    }
}

/// The `source_code` object the web UI and `clove version` share.
pub fn source_json() -> serde_json::Value {
    serde_json::json!({
        "url": source_url(),
        "version": VERSION,
        "license": LICENSE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_points_at_this_versions_upstream_tree() {
        if option_env!("CLOVE_SOURCE_URL").is_some() {
            return; // a fork build: the override is under test elsewhere
        }
        assert_eq!(
            source_url(),
            format!("https://github.com/egeapak/clove/tree/v{VERSION}")
        );
        assert_eq!(source_json()["license"], "AGPL-3.0-only");
    }
}
