//! Frontmatter parsing (DESIGN.md §4, §12.2).
//!
//! Pipeline for each file:
//! 1. Size guard from the opened file's metadata (reject absurdly large files
//!    before read): one `open` (refusing symlinks and non-regular files) and
//!    one `fstat`, with no second lookup by path. [`parse_frontmatter_file`]
//!    then reads only as far as a frontmatter block can reach, not the body.
//! 2. Locate the `---` … `---` frontmatter fences (`memchr`, zero-copy slice).
//! 3. Enforce the frontmatter byte budget and reject YAML anchors/aliases (bomb
//!    guard) before handing bytes to the YAML parser.
//! 4. Deserialize with `serde_norway` into [`ItemFrontmatter`]
//!    (`deny_unknown_fields`); missing `schema` defaults to 1.
//! 5. Field validation ([`crate::validate::validate_item`]).
//! 6. Confirm `id` matches the file name stem.

use camino::Utf8Path;

use crate::error::CloveError;
use crate::limits::{MAX_BODY_BYTES, MAX_FRONTMATTER_BYTES};
use crate::model::{Item, ItemFrontmatter};
use crate::validate::validate_item;

/// Open an item file for reading and return it with its size. A symlink is
/// refused, not followed, and so is anything that is not a regular file: the
/// store's scan skips both (DESIGN §12.3), so a read by path must not resolve
/// one either (a link to a device or a FIFO would block or never end).
fn open_item_file(path: &Utf8Path) -> Result<(std::fs::File, u64), CloveError> {
    let io_err = |source| CloveError::Io {
        path: path.to_owned(),
        source,
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    crate::fs_safe::no_follow(&mut options, path).map_err(io_err)?;
    // Never wait on a FIFO: the regular-file check below then rejects it.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let file = options.open(path).map_err(io_err)?;
    let metadata = file.metadata().map_err(io_err)?;
    if !metadata.is_file() {
        return Err(io_err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
    Ok((file, metadata.len()))
}

/// Parse the item file at `path`, validating that its `id` matches the file
/// name stem.
pub fn parse_item_file(path: &Utf8Path) -> Result<Item, CloveError> {
    // One path resolution, not two. This used to call `std::fs::metadata(path)`
    // for the size guard and then `std::fs::read(path)` — and `read` itself
    // opens and fstats the descriptor anyway, so the standalone `metadata` was a
    // second *by-path* lookup whose answer we already get for free. Locally that
    // is noise; on a filesystem where every operation is a round trip (NFS, SMB,
    // a network block device) path resolution is the expensive kind of call, and
    // `clove search` makes one per item: measured at ~38% of the scan's cost
    // under injected latency, of which this is roughly half.
    //
    // The guard is unchanged — the size is still checked before anything is
    // allocated for the body.
    let (file, len) = open_item_file(path)?;

    // Cheap early rejection of pathologically large files before we allocate.
    let ceiling = (MAX_FRONTMATTER_BYTES + MAX_BODY_BYTES).saturating_add(4096);
    if len as usize > ceiling {
        return Err(CloveError::BodyTooLarge {
            path: path.to_owned(),
            limit: MAX_BODY_BYTES,
        });
    }

    // Read through a `Take` rather than calling `read_to_end` on the `File`
    // directly: std's `File` specialization re-`fstat`s to size its buffer, which
    // would put the second call straight back. `Take` uses the generic path, and
    // we already know the size. The limit is the *ceiling*, not `len`, so a file
    // that grows between the stat and the read is rejected by the guard below
    // rather than silently truncated.
    let mut bytes = Vec::with_capacity(len as usize + 1);
    let limit = ceiling as u64 + 1;
    std::io::Read::read_to_end(&mut std::io::Read::take(&file, limit), &mut bytes).map_err(
        |source| CloveError::Io {
            path: path.to_owned(),
            source,
        },
    )?;
    if bytes.len() > ceiling {
        return Err(CloveError::BodyTooLarge {
            path: path.to_owned(),
            limit: MAX_BODY_BYTES,
        });
    }

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

/// Parse `bytes` into an [`Item`] **without** checking that the `id` matches any
/// file name. Used by the git merge driver, where git hands us temp file paths
/// (`%A`/`%B`/`%O`) whose stems are arbitrary, not the item id. Still applies all
/// structural guards (fences, byte budgets, alias bomb guard) and field
/// validation. `path` is used only for error context.
pub fn parse_item_lenient(bytes: &[u8], path: &Utf8Path) -> Result<Item, CloveError> {
    parse_item_inner(bytes, path)
}

/// Parse only the frontmatter of the file at `path`, without allocating the
/// body — the `scan_lazy` fast path for `ls`/`ready`/`blocked` (DESIGN §13.3).
/// Still validates the `id` matches the file name stem.
pub fn parse_frontmatter_file(path: &Utf8Path) -> Result<ItemFrontmatter, CloveError> {
    use std::io::Read as _;
    let io_err = |source| CloveError::Io {
        path: path.to_owned(),
        source,
    };
    let (file, len) = open_item_file(path)?;
    // Same cheap early rejection as `parse_item_file`: this is the hot scan
    // path (`ls`/`ready`/every mutation's `scan_or_fail`), so a multi-GB file
    // dropped into the issues dir must be rejected from metadata, not read
    // fully into memory on every scan just to fail the budget check after.
    let ceiling = (MAX_FRONTMATTER_BYTES + MAX_BODY_BYTES).saturating_add(4096);
    if len as usize > ceiling {
        return Err(CloveError::BodyTooLarge {
            path: path.to_owned(),
            limit: MAX_BODY_BYTES,
        });
    }

    // Read only as far as a frontmatter within budget can reach (plus the BOM
    // and both fences), not the body. The body's size still counts: it is the
    // file size minus where the body starts.
    let head_cap = MAX_FRONTMATTER_BYTES + FENCE_OVERHEAD_BYTES;
    let mut bytes = Vec::with_capacity((len as usize).min(head_cap));
    (&file)
        .take(head_cap as u64)
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    let truncated = (len as usize) > bytes.len();

    let head = if truncated {
        match split_frontmatter(&bytes, path) {
            // The closing fence must be followed by more of the head: one that
            // ends exactly at the cut might continue (`----`) in the file.
            Ok((frontmatter, body)) if !body.is_empty() => {
                Some((frontmatter, len as usize - (bytes.len() - body.len())))
            }
            _ => None,
        }
    } else {
        None
    };
    let frontmatter = match head {
        Some((frontmatter_bytes, body_len)) => {
            parse_frontmatter_checked(frontmatter_bytes, body_len, path)?
        }
        None => {
            // The whole file fits in the head, or no closing fence is within
            // reach: read the rest so the verdict is the full parse's.
            if truncated {
                (&file)
                    .take(ceiling as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_err)?;
                if bytes.len() > ceiling {
                    return Err(CloveError::BodyTooLarge {
                        path: path.to_owned(),
                        limit: MAX_BODY_BYTES,
                    });
                }
            }
            parse_frontmatter_parts(&bytes, path)?.0
        }
    };

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

/// Bytes a file may spend around a frontmatter block: a UTF-8 BOM, the opening
/// fence, and the closing fence with its line terminators, plus one byte of
/// body so the closing fence is seen whole.
const FENCE_OVERHEAD_BYTES: usize = 3 + 5 + 6 + 1;

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
    let frontmatter = parse_frontmatter_checked(frontmatter_bytes, body_bytes.len(), path)?;
    Ok((frontmatter, body_bytes))
}

/// Budgets, alias guard, deserialize and validate for a frontmatter block
/// already split from a body of `body_len` bytes.
fn parse_frontmatter_checked(
    frontmatter_bytes: &[u8],
    body_len: usize,
    path: &Utf8Path,
) -> Result<ItemFrontmatter, CloveError> {
    if frontmatter_bytes.len() > MAX_FRONTMATTER_BYTES {
        return Err(CloveError::FrontmatterTooLarge {
            path: path.to_owned(),
            limit: MAX_FRONTMATTER_BYTES,
        });
    }
    if body_len > MAX_BODY_BYTES {
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
        serde_norway::from_slice(frontmatter_bytes).map_err(|err| CloveError::InvalidYaml {
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

    Ok(frontmatter)
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

    // A UTF-8 byte-order mark before the opening fence is dropped: the same
    // Windows editors that rewrite line endings as CRLF may add one.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);

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
/// Only flags a `&`/`*` that (a) is followed by an anchor-name character, (b)
/// sits at a node position (see [`at_node_start`]), and (c) is not inside a
/// quoted scalar. This catches `key: &a`, `[*a]`, `{&b}`, `- *a` while leaving
/// free text alone — a title `Fix *the* thing` or `R&D` is mid-scalar, and
/// crucially the writer's own quoted output (`title: 'Fix: *very* broken'`) is
/// a quoted scalar in which `&`/`*` are literal text, exactly as the YAML
/// parser will treat them. Without the quote-awareness the guard rejected
/// files clove itself wrote, bricking the item until hand-edited.
///
/// Exposed (`pub`) so foreign-input parsers outside this crate — notably the tk
/// importer, which feeds untrusted frontmatter to the YAML parser — can reuse
/// the exact same guard instead of copy-pasting it.
pub fn contains_yaml_anchor_or_alias(frontmatter: &[u8]) -> bool {
    // Indent of the line that opened a block scalar (`key: |` / `>`): lines
    // indented deeper than it are scalar *content* the parser treats as plain
    // text, so they are skipped entirely (a multiline title rendered as `|-`
    // may legitimately contain `- *milk*` or `see: *note*` lines).
    let mut block_scalar_indent: Option<usize> = None;
    // Flow-collection depth (`[`/`{`): a `,` only separates nodes in flow
    // context; in block context it is ordinary scalar text.
    let mut flow_depth = 0usize;

    for line in frontmatter.split(|&b| b == b'\n') {
        let indent = line
            .iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count();
        let blank = indent == line.len();
        if let Some(opener_indent) = block_scalar_indent {
            if blank || indent > opener_indent {
                continue; // still inside the block scalar's content
            }
            block_scalar_indent = None;
        }
        if scan_line_for_anchor(line, &mut flow_depth) {
            return true;
        }
        if opens_block_scalar(line) {
            block_scalar_indent = Some(indent);
        }
    }
    false
}

/// Whether a line's value opens a YAML block scalar: after trimming trailing
/// whitespace and the optional chomping/indent indicators (`+`, `-`, digits),
/// it ends in `|` or `>` preceded by a space or `:` (i.e. `key: |`, `key: >2-`).
fn opens_block_scalar(line: &[u8]) -> bool {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }
    while end > 0 && matches!(line[end - 1], b'+' | b'-' | b'0'..=b'9') {
        end -= 1;
    }
    end > 0
        && matches!(line[end - 1], b'|' | b'>')
        && (end == 1 || matches!(line[end - 2], b' ' | b'\t' | b':'))
}

/// Scan one line (already known not to be block-scalar content) for an
/// anchor/alias at a node position, tracking quoted scalars so their content —
/// including the writer's own `title: 'Fix: *very* broken'` output — is
/// treated as the literal text the YAML parser sees. `flow_depth` persists
/// across lines (a flow collection may span several).
fn scan_line_for_anchor(line: &[u8], flow_depth: &mut usize) -> bool {
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < line.len() {
        let byte = line[index];
        if let Some(q) = quote {
            if byte == q {
                if q == b'\'' && line.get(index + 1) == Some(&b'\'') {
                    index += 1; // `''` escape inside a single-quoted scalar
                } else {
                    quote = None;
                }
            } else if q == b'"' && byte == b'\\' {
                index += 1; // skip the escaped character
            }
            index += 1;
            continue;
        }
        // A plain scalar that starts at a block-context node position runs to
        // the end of the line, or to the `: ` that makes it a mapping key. The
        // YAML parser reads everything in it as text, so `[`, `{`, `&` and `*`
        // there are literal (`title: Deref [*ptr] crashes` is what the writer
        // emits for that title) and must not open a flow collection or count
        // as an anchor. Flow context is excluded: a plain scalar there ends at
        // the next flow indicator.
        if *flow_depth == 0
            && starts_plain_scalar(line, index)
            && at_node_start(line, index, *flow_depth)
        {
            match plain_scalar_key_end(line, index) {
                Some(after_colon) => {
                    index = after_colon;
                    continue;
                }
                None => return false,
            }
        }
        match byte {
            // A quote only *opens* a quoted scalar at a node position; a quote
            // mid-scalar (`it's`) is literal text and must not swallow the
            // rest of the line (that would let `[it's, &bomb]` slip through).
            b'\'' | b'"' if at_node_start(line, index, *flow_depth) => quote = Some(byte),
            b'[' | b'{' => *flow_depth += 1,
            b']' | b'}' => *flow_depth = flow_depth.saturating_sub(1),
            b'&' | b'*' => {
                let followed_by_name = line
                    .get(index + 1)
                    .is_some_and(|&next| next.is_ascii_alphanumeric() || next == b'_');
                if followed_by_name && at_node_start(line, index, *flow_depth) {
                    return true;
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
}

/// Whether the byte at `index` can begin a plain (unquoted) scalar: not
/// whitespace and not a YAML indicator. `-`, `?` and `:` begin one only when a
/// non-space character follows (`-x`), otherwise they are structure (`- x`).
fn starts_plain_scalar(line: &[u8], index: usize) -> bool {
    match line[index] {
        b' ' | b'\t' | b'\r' => false,
        b'[' | b']' | b'{' | b'}' | b',' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\''
        | b'"' | b'%' | b'@' | b'`' => false,
        b'-' | b'?' | b':' => line
            .get(index + 1)
            .is_some_and(|next| !matches!(next, b' ' | b'\t' | b'\r')),
        _ => true,
    }
}

/// For a plain scalar starting at `start` in block context: the index just
/// past the `:` that ends it as a mapping key (`- name: &a x`), or `None` when
/// the scalar runs to the end of the line (a value).
fn plain_scalar_key_end(line: &[u8], start: usize) -> Option<usize> {
    (start..line.len()).find_map(|i| {
        let ends_key = line[i] == b':'
            && line
                .get(i + 1)
                .is_none_or(|next| matches!(next, b' ' | b'\t' | b'\r'));
        ends_key.then_some(i + 1)
    })
}

/// Whether `index` is a YAML *node start* position: after `key: `, after a
/// flow `[` / `{`, after a `,` **in flow context**, or after a block-sequence
/// `- ` whose dash chain reaches the start of the line.
///
/// Mirrors the parser's tokenization closely enough for the guard: `key: &a`
/// is an anchor but `foo:&a`, `a - *b*` and `hello, *world*` are plain-scalar
/// text (the `:`/`-` forms need a following space; the `,` only separates
/// nodes inside `[...]`/`{...}`).
fn at_node_start(bytes: &[u8], index: usize, flow_depth: usize) -> bool {
    let mut back = index;
    let mut crossed_dash = false;
    loop {
        let start = back;
        while back > 0 && matches!(bytes[back - 1], b' ' | b'\t') {
            back -= 1;
        }
        let had_space = back < start;
        if back == 0 {
            // First non-space token on its line (`bytes` is a single line). A
            // block-sequence entry (`- &a`, `- - &a`) is a node position; a
            // bare `&x`/`'x'` at line start is left alone (conservative — the
            // root of a frontmatter document is always a mapping).
            return crossed_dash;
        }
        return match bytes[back - 1] {
            b'[' | b'{' => true,
            b',' => flow_depth > 0,
            b':' => had_space,
            // A block-sequence dash opens a node position (`- &a`), but only
            // when the dash chain itself reaches the start of the line
            // (`  - &a`, `- - &a`) rather than sitting inside plain text
            // (`a - *b*`): keep walking left through the chain.
            b'-' if had_space => {
                crossed_dash = true;
                back -= 1;
                continue;
            }
            _ => false,
        };
    }
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
    fn guard_ignores_quoted_scalars_the_writer_produces() {
        // These titles force the writer into quoted or plain renderings whose
        // `&`/`*` are literal text; the guard flagging any of them means clove
        // corrupts its own store (write succeeds, every later read fails).
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        for title in [
            "Fix: *very* broken parser",    // single-quoted by the writer
            "Fix: &ref handling",           // single-quoted
            "hello, *world*",               // plain (comma is not special in block context)
            "foo:*bar",                     // plain (no space after the colon)
            "It's here: *starred*",         // quoted, with a literal apostrophe
            "steps\nsee: *note*\n- *milk*", // block scalar: content lines are text
        ] {
            let mut item = sample_item();
            item.frontmatter.title = title.to_owned();
            let path = dir.join(format!("{}.md", item.frontmatter.id));
            write_item_file(&item, &path).unwrap();
            let parsed = parse_item_file(&path).unwrap_or_else(|err| {
                panic!("writer output for title {title:?} must re-parse: {err}")
            });
            assert_eq!(parsed.frontmatter.title, title);
        }
    }

    #[test]
    fn guard_ignores_brackets_inside_plain_scalars_the_writer_produces() {
        // The writer renders these as plain block scalars. A `[`/`{` inside
        // one is text: it must not open a flow collection, and an unbalanced
        // one must not leak flow context into the lines that follow.
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        for text in [
            "Deref [*ptr] crashes",
            "closure {&mut x} borrow",
            "list [a, &b] parse",
            "see [a, *b]",
            "unbalanced [ then, &later",
            "-x [*y]",
        ] {
            let mut item = sample_item();
            item.frontmatter.title = text.to_owned();
            item.frontmatter.assignee = Some(format!("bob [*x] {text}"));
            item.frontmatter.external_ref = Some("ref, &x".to_owned());
            let path = dir.join(format!("{}.md", item.frontmatter.id));
            write_item_file(&item, &path).unwrap();
            let parsed = parse_item_file(&path)
                .unwrap_or_else(|err| panic!("writer output for {text:?} must re-parse: {err}"));
            assert_eq!(parsed.frontmatter, item.frontmatter);
        }
    }

    #[test]
    fn guard_still_rejects_real_anchors_and_aliases() {
        for fm in [
            "key: !!seq [&a x, *a]", // a tag before the flow collection
            "- name: &a x",          // a plain key does not hide its value
            "- name [x: &a y",       // nor does a bracket inside the key
            "labels:\n  [&a x, *a]", // flow collection on its own line
            "key: {a: &b 1}",
            "key: &a x",
            "deps: [*a]",
            "deps: [&a x, *a, *a]",
            "labels:\n  - &a x\n  - *a", // block-sequence entries
            "labels: [it's, &a x]",      // a mid-scalar quote must not mask it
            "key: 'quoted' \nother: [&boom x]",
        ] {
            assert!(contains_yaml_anchor_or_alias(fm.as_bytes()), "{fm}");
        }
        for fm in [
            "title: 'Fix: *very* broken'",
            "title: \"Fix: &ref stuff\"",
            "title: a - *b* c",
            "title: hello, *world*",
            "title: foo:*bar",
            "title: it's *fine* and R&D",
            "title: Deref [*ptr] crashes",
            "title: closure {&mut x} borrow",
            "title: open [ bracket\nassignee: a, &b",
            "labels:\n  - see [a, *b]",
            "title: |-\n  see: *note*\n  - *milk*\nother: x",
        ] {
            assert!(!contains_yaml_anchor_or_alias(fm.as_bytes()), "{fm}");
        }
    }

    #[test]
    fn accepts_a_leading_utf8_bom() {
        let contents = "\u{feff}---\r\nschema: 1\r\nid: proj-7AF3K2MN\r\ntitle: x\r\nstatus: open\r\ntype: bug\r\npriority: 2\r\ncreated: 2026-06-02T10:00:00Z\r\nupdated: 2026-06-02T10:00:00Z\r\n---\r\nbody\r\n";
        let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
        let item = parse_item_file(&path).unwrap();
        assert_eq!(item.frontmatter.title, "x");
        assert_eq!(item.body, "body\r\n");
        assert!(parse_frontmatter_file(&path).is_ok());
    }

    const HEAD: &str = "---\nschema: 1\nid: proj-7AF3K2MN\ntitle: x\nstatus: open\ntype: bug\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n";

    /// The frontmatter-only parse stops reading after the frontmatter, and its
    /// verdict on every file is the full parse's.
    #[test]
    fn frontmatter_parse_agrees_with_the_full_parse_on_large_files() {
        let verdict = |contents: &str| {
            let (_tmp, path) = write_temp("proj-7AF3K2MN.md", contents);
            let full = parse_item_file(&path).map(|item| item.frontmatter);
            let lazy = parse_frontmatter_file(&path);
            match (&full, &lazy) {
                (Ok(a), Ok(b)) => assert_eq!(a, b),
                (Err(a), Err(b)) => assert_eq!(
                    std::mem::discriminant(a),
                    std::mem::discriminant(b),
                    "full: {a:?}, lazy: {b:?}"
                ),
                other => panic!("verdicts differ: {other:?}"),
            }
            lazy
        };
        let filler = |n: usize| "b".repeat(n);

        // A body far larger than the head that is read.
        assert!(verdict(&format!("{HEAD}---\n{}", filler(MAX_FRONTMATTER_BYTES * 4))).is_ok());
        // Body at and over its budget.
        assert!(verdict(&format!("{HEAD}---\n{}", filler(MAX_BODY_BYTES))).is_ok());
        assert!(matches!(
            verdict(&format!("{HEAD}---\n{}", filler(MAX_BODY_BYTES + 1))),
            Err(CloveError::BodyTooLarge { .. })
        ));
        // Frontmatter over its budget: the fence is beyond the head.
        assert!(matches!(
            verdict(&format!(
                "{HEAD}assignee: {}\n---\n{}",
                filler(MAX_FRONTMATTER_BYTES),
                filler(100)
            )),
            Err(CloveError::FrontmatterTooLarge { .. })
        ));
        // No closing fence anywhere.
        assert!(matches!(
            verdict(&format!("{HEAD}{}", filler(MAX_FRONTMATTER_BYTES * 2))),
            Err(CloveError::UnterminatedFrontmatter { .. })
        ));
        // A `---` that ends exactly where the head is cut, for each way the
        // file may go on: a real fence, and `----`, which is not one.
        let pad =
            MAX_FRONTMATTER_BYTES + FENCE_OVERHEAD_BYTES - HEAD.len() - "assignee: \n---".len();
        for rest in [
            "\nbody\n",
            "-\nstill frontmatter: no\n---\nbody\n",
            "\r\nbody",
        ] {
            let contents = format!("{HEAD}assignee: {}\n---{rest}", filler(pad));
            assert_eq!(
                contents.find(rest).unwrap(),
                MAX_FRONTMATTER_BYTES + FENCE_OVERHEAD_BYTES
            );
            let _ = verdict(&contents);
        }
    }

    /// A symlinked or non-regular item file is refused, not followed: the scan
    /// skips such files, and a read by path must agree.
    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_item_file() {
        let (_tmp, target) = write_temp("proj-7AF3K2MN.md", &format!("{HEAD}---\n"));
        let link = target.with_file_name("proj-AAAAAAAA.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(parse_item_file(&target).is_ok());
        assert!(matches!(parse_item_file(&link), Err(CloveError::Io { .. })));
        assert!(matches!(
            parse_frontmatter_file(&link),
            Err(CloveError::Io { .. })
        ));

        // A FIFO must be rejected without blocking on the open.
        let fifo = target.with_file_name("proj-BBBBBBBB.md");
        let c_path = std::ffi::CString::new(fifo.as_str()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
        assert!(matches!(parse_item_file(&fifo), Err(CloveError::Io { .. })));
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
