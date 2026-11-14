//! `about.toml` `accepted` must be `deny.toml` `[licenses] allow` plus clove's
//! own `AGPL-3.0-only`.
//!
//! cargo-deny gates every pull request with the `allow` list. cargo-about
//! writes `THIRD_PARTY_LICENSES.txt` with the `accepted` list, and
//! `cargo about generate --fail` runs in the release workflow, after the tag is
//! pushed. A license added to one list only is found there, too late. CI also
//! runs the generate step ("third-party notices"); this test names the cause.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// The string elements of the first `key = [ ... ]` array in a TOML file.
/// Enough for these two files: one string per line, `#` comments.
fn string_array(file: &str, key: &str) -> BTreeSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines();
    let opening = format!("{key} = [");
    assert!(
        lines.any(|line| line.trim() == opening),
        "{file} has no `{opening}` line"
    );
    let mut values = BTreeSet::new();
    for line in lines {
        let line = line.trim();
        if line.starts_with(']') {
            assert!(!values.is_empty(), "{file} `{key}` is empty");
            return values;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let value = line
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(value, _)| value)
            .unwrap_or_else(|| panic!("{file} `{key}`: cannot read `{line}`"));
        values.insert(value.to_owned());
    }
    panic!("{file} `{key}` has no closing bracket");
}

#[test]
fn cargo_about_accepts_what_cargo_deny_allows() {
    let mut expected = string_array("deny.toml", "allow");
    assert!(
        !expected.contains("AGPL-3.0-only"),
        "deny.toml allows clove's AGPL crates one by one in [[licenses.exceptions]], \
         never in `allow`, so that a third-party AGPL dependency is rejected"
    );
    expected.insert("AGPL-3.0-only".to_owned());
    assert_eq!(
        string_array("about.toml", "accepted"),
        expected,
        "about.toml `accepted` must equal deny.toml `[licenses] allow` plus AGPL-3.0-only"
    );
}
