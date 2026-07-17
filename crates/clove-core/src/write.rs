//! The hand-rolled canonical frontmatter serializer (DESIGN.md §4).
//!
//! `clove`'s value proposition is git-friendly plain files, so the exact bytes
//! on disk are a contract: a stable field order, inline sorted list fields, `[]`
//! for empty lists (never omitted — §14.5, required by the 3-way merge driver),
//! and omitted null scalar optionals. A general YAML serializer does not
//! guarantee field order across versions, so we own the *structure* here.
//!
//! We do **not** hand-roll YAML string quoting, though: each free-text scalar is
//! rendered by `serde_yaml_neo::to_string`, which decides plain vs.
//! single/double-quoted correctly. We keep the deterministic structure; the
//! library keeps the fiddly quoting. (Field *ordering* — the stable-diff-
//! critical part — stays under our control; the library only renders one scalar
//! at a time, behavior that is far more stable than mapping key ordering.)

use std::io::{self, Write};
use std::time::Duration;

use camino::Utf8Path;
use chrono::{DateTime, SecondsFormat, Utc};
use tempfile::NamedTempFile;

use crate::error::CloveError;
use crate::id::CloveId;
use crate::model::{Item, ItemFrontmatter};

/// Writes [`ItemFrontmatter`] in canonical form to any [`Write`] sink.
pub struct FrontmatterWriter<W: Write> {
    inner: W,
}

impl<W: Write> FrontmatterWriter<W> {
    /// Wrap a sink.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Write the fenced frontmatter block (`---` … `---`) for `fm` in canonical
    /// field order. Does not write the body — see [`write_item_file`].
    pub fn write_item(&mut self, fm: &ItemFrontmatter) -> io::Result<()> {
        let w = &mut self.inner;

        writeln!(w, "---")?;

        // Required fields, fixed order (DESIGN.md §2.2).
        writeln!(w, "schema: {}", fm.schema)?;
        writeln!(w, "id: {}", fm.id)?; // grammar-safe → always plain
        writeln!(w, "title: {}", render_scalar(&fm.title))?;
        writeln!(w, "status: {}", fm.status.as_str())?;
        writeln!(w, "type: {}", fm.item_type.as_str())?;
        writeln!(w, "priority: {}", fm.priority.get())?;
        writeln!(w, "created: {}", rfc3339(fm.created))?;
        writeln!(w, "updated: {}", rfc3339(fm.updated))?;

        // Optional scalar fields: omitted entirely when absent.
        if let Some(closed) = fm.closed {
            writeln!(w, "closed: {}", rfc3339(closed))?;
        }
        if let Some(assignee) = &fm.assignee {
            writeln!(w, "assignee: {}", render_scalar(assignee))?;
        }
        if let Some(parent) = &fm.parent {
            writeln!(w, "parent: {parent}")?; // grammar-safe id → plain
        }

        // List fields: always written, sorted + de-duped, inline flow. `[]` when
        // empty (never omitted) so the merge driver has a stable baseline line.
        write_label_list(w, "labels", &fm.labels)?;
        write_id_list(w, "deps", &fm.deps)?;
        write_id_list(w, "relates", &fm.relates)?;
        write_id_list(w, "duplicates", &fm.duplicates)?;
        write_id_list(w, "supersedes", &fm.supersedes)?;

        // Trailing optional scalars.
        if let Some(source_system) = &fm.source_system {
            writeln!(w, "source_system: {}", render_scalar(source_system))?;
        }
        if let Some(external_ref) = &fm.external_ref {
            writeln!(w, "external_ref: {}", render_scalar(external_ref))?;
        }

        writeln!(w, "---")?;
        Ok(())
    }
}

/// Serialize an [`Item`] to its file at `path` via an atomic write.
///
/// The file is `<frontmatter block>` immediately followed by the body bytes.
/// Written to a sibling temp file, fsync'd, then renamed into place (POSIX
/// atomic; Windows `MoveFileExW` with a small retry on `ERROR_ACCESS_DENIED`).
pub fn write_item_file(item: &Item, path: &Utf8Path) -> Result<(), CloveError> {
    let mut buffer: Vec<u8> = Vec::new();
    {
        let mut writer = FrontmatterWriter::new(&mut buffer);
        writer
            .write_item(&item.frontmatter)
            .map_err(|source| CloveError::Io {
                path: path.to_owned(),
                source,
            })?;
    }
    buffer.extend_from_slice(item.body.as_bytes());
    atomic_write(path, &buffer)
}

/// Render an RFC3339 timestamp with second precision and a `Z` suffix, matching
/// the canonical on-disk form.
fn rfc3339(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Render a free-text scalar with correct YAML quoting, delegated to
/// `serde_yaml_neo`. Serializing a string to YAML is infallible in practice.
fn render_scalar(value: &str) -> String {
    let rendered =
        serde_yaml_neo::to_string(value).expect("serializing a string scalar to YAML cannot fail");
    rendered.trim_end_matches('\n').to_string()
}

/// Render a scalar for use inside a flow `[...]` sequence.
///
/// `render_scalar` quotes in *block* context, where the flow indicators
/// `, [ ] { }` are not special — so a value like `needs, quote` comes back
/// unquoted and would corrupt a flow list. We therefore force an inline
/// double-quoted form when the block rendering is (a) a multiline block scalar,
/// or (b) left plain (unquoted) yet contains a flow indicator. Values serde
/// already quoted are safe in flow context as-is.
fn render_inline_scalar(value: &str) -> String {
    let rendered = render_scalar(value);
    let already_quoted = rendered.starts_with('\'') || rendered.starts_with('"');
    let is_multiline = rendered.contains('\n');
    if is_multiline || (!already_quoted && contains_flow_indicator(value)) {
        force_double_quoted(value)
    } else {
        rendered
    }
}

/// Whether `s` contains a YAML flow-sequence indicator that requires quoting
/// inside `[...]`.
fn contains_flow_indicator(s: &str) -> bool {
    s.bytes()
        .any(|b| matches!(b, b',' | b'[' | b']' | b'{' | b'}'))
}

/// Minimal inline double-quoted YAML scalar (defensive fallback only).
fn force_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Write `key: [a, b, c]` for a list of IDs, sorted and de-duped. IDs are
/// grammar-safe, so no quoting is needed.
fn write_id_list<W: Write>(w: &mut W, key: &str, ids: &[CloveId]) -> io::Result<()> {
    let mut values: Vec<&str> = ids.iter().map(CloveId::as_str).collect();
    values.sort_unstable();
    values.dedup();
    write!(w, "{key}: [")?;
    for (index, id) in values.iter().enumerate() {
        if index > 0 {
            write!(w, ", ")?;
        }
        write!(w, "{id}")?;
    }
    writeln!(w, "]")
}

/// Write `key: [a, b, c]` for a list of labels, sorted and de-duped, each
/// rendered with correct quoting.
fn write_label_list<W: Write>(w: &mut W, key: &str, labels: &[String]) -> io::Result<()> {
    let mut values: Vec<&str> = labels.iter().map(String::as_str).collect();
    values.sort_unstable();
    values.dedup();
    write!(w, "{key}: [")?;
    for (index, label) in values.iter().enumerate() {
        if index > 0 {
            write!(w, ", ")?;
        }
        write!(w, "{}", render_inline_scalar(label))?;
    }
    writeln!(w, "]")
}

/// Atomically replace `path` with `bytes`: write to a sibling temp file, fsync,
/// then rename.
fn atomic_write(path: &Utf8Path, bytes: &[u8]) -> Result<(), CloveError> {
    let parent = path.parent().ok_or_else(|| CloveError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory"),
    })?;

    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|source| CloveError::Io {
            path: parent.to_owned(),
            source,
        })?;
    temp.write_all(bytes).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    temp.as_file().sync_all().map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;
    persist_with_retry(temp, path)?;

    // The rename is atomic but not yet *durable*: on POSIX the directory entry
    // lives in the parent directory, so without fsyncing it a power failure
    // shortly after a "successful" write can roll the file back to its old
    // content. Windows has no directory handle to sync; the rename there is
    // made durable by the target-volume flush semantics of MoveFileEx.
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(parent.as_std_path()).map_err(|source| CloveError::Io {
            path: parent.to_owned(),
            source,
        })?;
        dir.sync_all().map_err(|source| CloveError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    Ok(())
}

/// Rename the temp file onto `path`, retrying on transient failures (Windows
/// `ERROR_ACCESS_DENIED` when another process briefly holds the target).
fn persist_with_retry(mut temp: NamedTempFile, path: &Utf8Path) -> Result<(), CloveError> {
    // Backoffs before retries 1, 2, 3 (DESIGN.md §4).
    const BACKOFF_MS: [u64; 3] = [10, 50, 150];
    let mut attempt = 0usize;
    loop {
        match temp.persist(path.as_std_path()) {
            Ok(_) => return Ok(()),
            Err(err) => {
                if attempt >= BACKOFF_MS.len() {
                    return Err(CloveError::Io {
                        path: path.to_owned(),
                        source: err.error,
                    });
                }
                std::thread::sleep(Duration::from_millis(BACKOFF_MS[attempt]));
                attempt += 1;
                temp = err.file;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemFrontmatter, ItemStatus, ItemType, Priority};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// A fully-populated item whose serialization must match the golden fixture.
    fn full_item() -> Item {
        Item {
            frontmatter: ItemFrontmatter {
                schema: 1,
                id: CloveId::new("proj-7AF3K2MN").unwrap(),
                title: "Article image download and compression".to_owned(),
                status: ItemStatus::Closed,
                item_type: ItemType::Feature,
                priority: Priority(1),
                created: ts("2026-06-02T10:00:00Z"),
                updated: ts("2026-06-02T14:23:00Z"),
                closed: Some(ts("2026-06-02T14:23:00Z")),
                assignee: Some("ege".to_owned()),
                parent: Some(CloveId::new("proj-2BK8NXYZ").unwrap()),
                labels: vec!["area:core".to_owned(), "perf".to_owned()],
                deps: vec![CloveId::new("proj-3K2MZABC").unwrap()],
                relates: vec![CloveId::new("proj-9P1QRSTU").unwrap()],
                duplicates: vec![CloveId::new("proj-DUP00001").unwrap()],
                supersedes: vec![CloveId::new("proj-SUP00001").unwrap()],
                source_system: Some("github".to_owned()),
                external_ref: Some("gh-42".to_owned()),
            },
            body: "Save compressed versions of images.\n".to_owned(),
        }
    }

    fn serialize_frontmatter(fm: &ItemFrontmatter) -> String {
        let mut buf: Vec<u8> = Vec::new();
        FrontmatterWriter::new(&mut buf).write_item(fm).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn golden_full_item_is_byte_identical() {
        let expected = include_str!("../tests/fixtures/full_item.md");
        let tmp = tempfile::tempdir().unwrap();
        let path = camino::Utf8Path::from_path(tmp.path())
            .unwrap()
            .join("out.md");
        write_item_file(&full_item(), &path).unwrap();
        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn output_is_idempotent() {
        let fm = full_item().frontmatter;
        assert_eq!(serialize_frontmatter(&fm), serialize_frontmatter(&fm));
    }

    #[test]
    fn lists_are_sorted_and_deduped() {
        let mut fm = full_item().frontmatter;
        fm.labels = vec!["perf".to_owned(), "area:core".to_owned(), "perf".to_owned()];
        fm.deps = vec![
            CloveId::new("proj-ZZZZZZZZ").unwrap(),
            CloveId::new("proj-AAAAAAAA").unwrap(),
            CloveId::new("proj-AAAAAAAA").unwrap(),
        ];
        let out = serialize_frontmatter(&fm);
        assert!(out.contains("labels: [area:core, perf]"), "got:\n{out}");
        assert!(
            out.contains("deps: [proj-AAAAAAAA, proj-ZZZZZZZZ]"),
            "got:\n{out}"
        );
    }

    #[test]
    fn empty_lists_render_as_brackets_and_null_scalars_are_omitted() {
        let mut fm = full_item().frontmatter;
        fm.labels.clear();
        fm.deps.clear();
        fm.relates.clear();
        fm.duplicates.clear();
        fm.supersedes.clear();
        fm.assignee = None;
        fm.parent = None;
        fm.closed = None;
        fm.status = ItemStatus::Open;
        fm.source_system = None;
        fm.external_ref = None;

        let out = serialize_frontmatter(&fm);
        for key in ["labels", "deps", "relates", "duplicates", "supersedes"] {
            assert!(
                out.contains(&format!("{key}: []")),
                "{key} must be []:\n{out}"
            );
        }
        for absent in [
            "assignee:",
            "parent:",
            "closed:",
            "source_system:",
            "external_ref:",
        ] {
            assert!(!out.contains(absent), "{absent} must be omitted:\n{out}");
        }
    }

    #[test]
    fn special_characters_in_title_are_quoted() {
        let mut fm = full_item().frontmatter;
        fm.title = "Fix: the parser, please".to_owned();
        let out = serialize_frontmatter(&fm);
        // serde_yaml_neo single-quotes a value containing `: ` and `,`.
        assert!(
            out.contains("title: 'Fix: the parser, please'"),
            "got:\n{out}"
        );
    }

    #[test]
    fn label_with_flow_characters_roundtrips_through_flow_list() {
        // A label containing flow indicators must survive a write → parse cycle
        // intact (i.e. it stays one element, not split on the comma/bracket).
        let mut fm = full_item().frontmatter;
        fm.labels = vec![
            "plain".to_owned(),
            "needs, quote".to_owned(),
            "has[bracket]".to_owned(),
        ];
        let out = serialize_frontmatter(&fm);
        let inner = out
            .strip_prefix("---\n")
            .unwrap()
            .strip_suffix("---\n")
            .unwrap();
        let parsed: ItemFrontmatter = serde_yaml_neo::from_str(inner).expect("parses back");
        let mut labels = parsed.labels.clone();
        labels.sort();
        assert_eq!(
            labels,
            vec![
                "has[bracket]".to_owned(),
                "needs, quote".to_owned(),
                "plain".to_owned()
            ],
            "got:\n{out}"
        );
    }
}
