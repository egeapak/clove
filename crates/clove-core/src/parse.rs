//! Frontmatter parsing (DESIGN.md §4, §12.2).
//!
//! Pipeline for each file:
//! 1. Size guard from file metadata (reject absurdly large files before read).
//! 2. Locate the `---` … `---` frontmatter fences (`memchr`, zero-copy slice).
//! 3. Enforce the frontmatter byte budget and reject YAML anchors/aliases (bomb
//!    guard) before handing bytes to the YAML parser.
//! 4. Deserialize with `serde_yaml_neo` into [`ItemFrontmatter`]
//!    (`deny_unknown_fields`); missing `schema` defaults to 1.
//! 5. Field validation ([`crate::validate::validate_item`]).
//! 6. Confirm `id` matches the file name stem.

use camino::Utf8Path;

use crate::error::CloveError;
use crate::limits::{MAX_BODY_BYTES, MAX_FRONTMATTER_BYTES};
use crate::model::{Item, ItemFrontmatter};
use crate::validate::validate_item;

/// Parse the item file at `path`, validating that its `id` matches the file
/// name stem.
pub fn parse_item_file(path: &Utf8Path) -> Result<Item, CloveError> {
    let metadata = std::fs::metadata(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    // Cheap early rejection of pathologically large files before we allocate.
    let ceiling = (MAX_FRONTMATTER_BYTES + MAX_BODY_BYTES).saturating_add(4096);
    if metadata.len() as usize > ceiling {
        return Err(CloveError::BodyTooLarge {
            path: path.to_owned(),
            limit: MAX_BODY_BYTES,
        });
    }

    let bytes = std::fs::read(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;

    let item = parse_item_inner(&bytes, path)?;

    let stem = path.file_stem().unwrap_or_default();
    if item.frontmatter.id.as_str() != stem {
        return Err(CloveError::IdMismatch {
            path: path.to_owned(),
            id: item.frontmatter.id.to_string(),
            stem: stem.to_owned(),
        });
    }

    Ok(item)
}

/// Parse already-loaded `bytes`, validating that the parsed `id` matches
/// `expected_id`. `path` is used only for error context.
pub fn parse_item_bytes(
    bytes: &[u8],
    path: &Utf8Path,
    expected_id: &crate::id::CloveId,
) -> Result<Item, CloveError> {
    let item = parse_item_inner(bytes, path)?;
    if &item.frontmatter.id != expected_id {
        return Err(CloveError::IdMismatch {
            path: path.to_owned(),
            id: item.frontmatter.id.to_string(),
            stem: expected_id.to_string(),
        });
    }
    Ok(item)
}

/// Parse only the frontmatter of the file at `path`, without allocating the
/// body — the `scan_lazy` fast path for `ls`/`ready`/`blocked` (DESIGN §13.3).
/// Still validates the `id` matches the file name stem.
pub fn parse_frontmatter_file(path: &Utf8Path) -> Result<ItemFrontmatter, CloveError> {
    let bytes = std::fs::read(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    let (frontmatter, _body) = parse_frontmatter_parts(&bytes, path)?;
    let stem = path.file_stem().unwrap_or_default();
    if frontmatter.id.as_str() != stem {
        return Err(CloveError::IdMismatch {
            path: path.to_owned(),
            id: frontmatter.id.to_string(),
            stem: stem.to_owned(),
        });
    }
    Ok(frontmatter)
}

/// The structural parse: fences, budgets, alias guard, deserialize, validate.
/// Does not check the id against any external reference.
fn parse_item_inner(bytes: &[u8], path: &Utf8Path) -> Result<Item, CloveError> {
    let (frontmatter, body_bytes) = parse_frontmatter_parts(bytes, path)?;
    let body = String::from_utf8(body_bytes.to_vec()).map_err(|_| CloveError::InvalidYaml {
        path: path.to_owned(),
        message: "item body is not valid UTF-8".to_owned(),
    })?;
    Ok(Item { frontmatter, body })
}

/// Shared structural parse returning the validated frontmatter plus the
/// still-borrowed body bytes (no body allocation). Used by both the full parse
/// and the body-free `scan_lazy` path.
fn parse_frontmatter_parts<'a>(
    bytes: &'a [u8],
    path: &Utf8Path,
) -> Result<(ItemFrontmatter, &'a [u8]), CloveError> {
    let (frontmatter_bytes, body_bytes) = split_frontmatter(bytes, path)?;

    if frontmatter_bytes.len() > MAX_FRONTMATTER_BYTES {
        return Err(CloveError::FrontmatterTooLarge {
            path: path.to_owned(),
            limit: MAX_FRONTMATTER_BYTES,
        });
    }
    if body_bytes.len() > MAX_BODY_BYTES {
        return Err(CloveError::BodyTooLarge {
            path: path.to_owned(),
            limit: MAX_BODY_BYTES,
        });
    }
    if contains_yaml_anchor_or_alias(frontmatter_bytes) {
        return Err(CloveError::AliasNotAllowed {
            path: path.to_owned(),
        });
    }

    let frontmatter: ItemFrontmatter =
        serde_yaml_neo::from_slice(frontmatter_bytes).map_err(|err| CloveError::InvalidYaml {
            path: path.to_owned(),
            message: err.to_string(),
        })?;

    let errors = validate_item(&frontmatter);
    if !errors.is_empty() {
        return Err(CloveError::Invalid {
            path: path.to_owned(),
            count: errors.len(),
            summary: errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        });
    }

    Ok((frontmatter, body_bytes))
}

/// Split a file into (frontmatter bytes, body bytes) by locating the opening and
/// closing `---` fence lines. The frontmatter slice excludes the fences; the
/// body slice is everything after the closing fence's line terminator.
///
/// Tolerates both LF and CRLF line endings (files written by clove use LF, but a
/// Windows editor may rewrite them).
fn split_frontmatter<'a>(
    bytes: &'a [u8],
    path: &Utf8Path,
) -> Result<(&'a [u8], &'a [u8]), CloveError> {
    let missing = || CloveError::MissingFrontmatter {
        path: path.to_owned(),
    };

    // Opening fence at the very start.
    let open_len = if bytes.starts_with(b"---\n") {
        4
    } else if bytes.starts_with(b"---\r\n") {
        5
    } else {
        return Err(missing());
    };

    // Find the closing fence: a line consisting of exactly `---`.
    let finder = memchr::memmem::Finder::new(b"---");
    let mut search_from = open_len;
    loop {
        let relative = match finder.find(&bytes[search_from..]) {
            Some(rel) => rel,
            None => {
                return Err(CloveError::UnterminatedFrontmatter {
                    path: path.to_owned(),
                })
            }
        };
        let fence_start = search_from + relative;
        let after_fence = fence_start + 3;

        // Must start at a line boundary (preceded by `\n`).
        let at_line_start = fence_start > 0 && bytes[fence_start - 1] == b'\n';

        // Must be followed by a line terminator or EOF (so `----` / `--- x` are
        // not treated as the closing fence).
        let body_start = if after_fence == bytes.len() {
            Some(after_fence)
        } else if bytes[after_fence] == b'\n' {
            Some(after_fence + 1)
        } else if bytes[after_fence] == b'\r' && bytes.get(after_fence + 1) == Some(&b'\n') {
            Some(after_fence + 2)
        } else {
            None
        };

        match (at_line_start, body_start) {
            (true, Some(body_start)) => {
                // Frontmatter content runs from after the opening fence up to
                // (but not including) the `\n` preceding the closing fence.
                let frontmatter_end = fence_start - 1;
                let frontmatter = &bytes[open_len..frontmatter_end.max(open_len)];
                let body = &bytes[body_start..];
                return Ok((frontmatter, body));
            }
            _ => {
                // Not a standalone fence; keep searching past this occurrence.
                search_from = after_fence;
            }
        }
    }
}

/// Heuristic detector for YAML anchors (`&name`) and aliases (`*name`) at node
/// positions, used as a bomb guard before the YAML parser runs (DESIGN §12.2).
///
/// Only flags a `&`/`*` that (a) is followed by an anchor-name character and
/// (b) sits at a value or flow-element position — i.e. the nearest preceding
/// non-space character is `:`, `[`, `{`, or `,`. This catches `key: &a`,
/// `[*a]`, `{&b}` while leaving free text alone (a title `Fix *the* thing` or
/// `R&D` is not flagged, since `*`/`&` there are mid-scalar).
fn contains_yaml_anchor_or_alias(frontmatter: &[u8]) -> bool {
    for (index, &byte) in frontmatter.iter().enumerate() {
        if byte != b'&' && byte != b'*' {
            continue;
        }
        // Must be followed by an anchor-name start character.
        let followed_by_name = frontmatter
            .get(index + 1)
            .is_some_and(|&next| next.is_ascii_alphanumeric() || next == b'_');
        if !followed_by_name {
            continue;
        }
        // Walk back over spaces/tabs to the governing character.
        let mut back = index;
        while back > 0 && matches!(frontmatter[back - 1], b' ' | b'\t') {
            back -= 1;
        }
        let governing = if back == 0 {
            None
        } else {
            Some(frontmatter[back - 1])
        };
        if matches!(governing, Some(b':') | Some(b'[') | Some(b'{') | Some(b',')) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::CloveId;
    use crate::model::{ItemFrontmatter, ItemStatus, ItemType, Priority};
    use crate::write::write_item_file;

    fn write_temp(name: &str, contents: &str) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap().to_owned();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        (tmp, path)
    }

    fn sample_item() -> Item {
        Item {
            frontmatter: ItemFrontmatter {
                schema: 1,
                id: CloveId::new("proj-7AF3K2MN").unwrap(),
                title: "Round trip: a, b & c".to_owned(),
                status: ItemStatus::InProgress,
                item_type: ItemType::Bug,
                priority: Priority(0),
                created: "2026-06-02T10:00:00Z".parse().unwrap(),
                updated: "2026-06-02T11:00:00Z".parse().unwrap(),
                closed: None,
                assignee: Some("ege".to_owned()),
                parent: None,
                labels: vec!["area:core".to_owned()],
                deps: vec![CloveId::new("proj-3K2MZABC").unwrap()],
                relates: vec![],
                duplicates: vec![],
                supersedes: vec![],
                source_system: None,
                external_ref: None,
            },
            body: "Body line one.\nBody line two.\n".to_owned(),
        }
    }

    #[test]
    fn write_then_parse_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let item = sample_item();
        let path = dir.join(format!("{}.md", item.frontmatter.id));
        write_item_file(&item, &path).unwrap();

        let parsed = parse_item_file(&path).unwrap();
        assert_eq!(parsed, item);
    }

    #[test]
    fn missing_schema_defaults_to_v1() {
        let contents = "---\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\nlabels: []\ndeps: []\nrelates: []\nduplicates: []\nsupersedes: []\n---\nbody\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let item = parse_item_file(&path).unwrap();
        assert_eq!(item.frontmatter.schema, 1);
    }

    #[test]
    fn rejects_filename_mismatch() {
        let contents = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n";
        // File named differently from the embedded id.
        let (_tmp, path) = write_temp("proj-AAAAAAAA.md", contents);
        let err = parse_item_file(&path).unwrap_err();
        assert!(matches!(err, CloveError::IdMismatch { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_yaml_alias() {
        let contents = "---\nschema: 1\nid: &anchor proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let err = parse_item_file(&path).unwrap_err();
        assert!(
            matches!(err, CloveError::AliasNotAllowed { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn allows_ampersand_and_star_in_free_text() {
        // `R&D` and `*emphasis*` are plain scalar text, not anchors/aliases.
        let fm = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: \"R&D and a *star* note\"\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n";
        assert!(!contains_yaml_anchor_or_alias(
            fm.strip_prefix("---\n")
                .unwrap()
                .strip_suffix("---\n")
                .unwrap()
                .as_bytes()
        ));
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", fm);
        assert!(parse_item_file(&path).is_ok());
    }

    #[test]
    fn rejects_invalid_yaml() {
        let contents = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: [unterminated\n---\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let err = parse_item_file(&path).unwrap_err();
        assert!(matches!(err, CloveError::InvalidYaml { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_unknown_field() {
        let contents = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\nblocks: [proj-AAAAAAAA]\n---\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let err = parse_item_file(&path).unwrap_err();
        // deny_unknown_fields surfaces as a YAML parse error.
        assert!(matches!(err, CloveError::InvalidYaml { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_oversized_frontmatter() {
        let mut contents = String::from("---\nschema: 1\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\nassignee: ");
        contents.push_str(&"a".repeat(MAX_FRONTMATTER_BYTES + 10));
        contents.push_str("\n---\n");
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", &contents);
        let err = parse_item_file(&path).unwrap_err();
        assert!(
            matches!(err, CloveError::FrontmatterTooLarge { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_missing_fence() {
        let contents = "no frontmatter here\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let err = parse_item_file(&path).unwrap_err();
        assert!(
            matches!(err, CloveError::MissingFrontmatter { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn empty_body_is_fine() {
        let contents = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let item = parse_item_file(&path).unwrap();
        assert_eq!(item.body, "");
    }
}
