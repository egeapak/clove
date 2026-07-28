//! Append-only comment files (DESIGN.md §2.5).
//!
//! Each comment is its own file under `.clove/issues/<id>/comments/`, so two
//! comments added on different branches are distinct files and merge without
//! conflict by construction — the decisive reason for the sidecar design.
//!
//! ## Deviations from §2.5 (both for cross-platform safety)
//!
//! - **Colon-free timestamp.** §2.5 puts a raw RFC3339 timestamp in the file
//!   name, but `:` is illegal in Windows file names. We use a fixed-length basic
//!   ISO form `YYYYMMDDTHHMMSS.fffffffffZ` — still lexicographically sortable
//!   (== chronological) and collision-resistant, but valid on every target FS.
//! - **`chrono`, not `jiff`.** We use `chrono` (as the rest of the crate does)
//!   for the timestamp; sub-second precision plus the 4-char random suffix gives
//!   the same practical collision-resistance §2.5 sought from nanosecond `jiff`.
//!
//! ## Canonical timestamps (READ_PATH_ROADMAP §3)
//!
//! **The file name is the comment's only timestamp** — the file holds the body
//! and nothing else — so the name is simultaneously the stored value, the
//! ordering key, and half of the uniqueness key. Canonicalization applies to the
//! *rendered* timestamp, not to the name:
//!
//! - **Rendered** (`ops::comments`, and so the CLI, the web API and the MCP tool
//!   alike) through [`clove_types::canonical_rfc3339`]: whole seconds, `Z`. This
//!   is the gap §3 named — comments used to render `to_rfc3339()` of a
//!   nanosecond `Utc::now()` (`2026-06-02T08:54:22.904816670+00:00`) while item
//!   timestamps a line above rendered `2026-06-02T08:54:22Z`.
//! - **Stored** at full precision, deliberately. The name is not RFC 3339 and
//!   has only ever had one spelling (fixed-length basic ISO, always `Z`, always
//!   nine fraction digits), so it has no spelling to canonicalize — and its
//!   fraction is *load-bearing*: comment files are append-only and never
//!   rewritten, so the name is the only record of the order of two comments
//!   added in the same second. Truncating it to seconds re-orders such a thread
//!   arbitrarily (`comments_limit_returns_most_recent_n` in
//!   `crates/clove/tests/cli_more.rs` catches exactly that), which is a worse
//!   bug than the one §3 set out to fix.
//!
//! Comments written by an older clove therefore need no migration of any kind:
//! nothing about the name format changed, they parse as they always did, and
//! they render through the same canonical path as new ones.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, NaiveDateTime, Utc};

use crate::error::CloveError;
use crate::id::CloveId;

/// Fixed-length basic-ISO UTC timestamp format used in comment file names.
/// Always 26 characters: `YYYYMMDDTHHMMSS.fffffffffZ`.
const FILENAME_TS_FORMAT: &str = "%Y%m%dT%H%M%S%.9fZ";
const FILENAME_TS_LEN: usize = 26;

/// A parsed comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub timestamp: DateTime<Utc>,
    pub author: String,
    pub body: String,
}

/// Append a comment to item `id`, returning the path of the new comment file.
/// Uses the current wall-clock time.
pub fn add_comment(
    issues_dir: &Utf8Path,
    id: &CloveId,
    author_email: &str,
    body: &str,
) -> Result<Utf8PathBuf, CloveError> {
    add_comment_at(issues_dir, id, author_email, body, Utc::now())
}

/// Like [`add_comment`] but with an explicit timestamp (for deterministic
/// tests). Two calls with the *same* timestamp still produce distinct files,
/// thanks to the random suffix.
pub fn add_comment_at(
    issues_dir: &Utf8Path,
    id: &CloveId,
    author_email: &str,
    body: &str,
    timestamp: DateTime<Utc>,
) -> Result<Utf8PathBuf, CloveError> {
    let dir = comments_dir(issues_dir, id);
    std::fs::create_dir_all(&dir).map_err(|source| CloveError::Io {
        path: dir.clone(),
        source,
    })?;

    let ts_str = timestamp.format(FILENAME_TS_FORMAT).to_string();
    let slug = author_slug(author_email);

    // Retry on the astronomically unlikely random-suffix collision.
    // `create_new` makes the check-and-create atomic: an `exists()` probe
    // followed by `write` (which truncates) would let two processes that
    // picked the same name silently overwrite one comment with the other.
    const MAX_ATTEMPTS: u32 = 5;
    for _ in 0..MAX_ATTEMPTS {
        let suffix = random_suffix();
        let path = dir.join(format!("{ts_str}-{slug}-{suffix}.md"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.as_std_path())
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(body.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|source| CloveError::Io {
                        path: path.clone(),
                        source,
                    })?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CloveError::Io {
                    path: path.clone(),
                    source,
                })
            }
        }
    }
    Err(CloveError::CommentConflict {
        attempts: MAX_ATTEMPTS,
    })
}

/// List all comments for item `id`, sorted chronologically. Returns an empty
/// list when the item has no comments. Files whose names don't parse are
/// skipped (defensively), never fatal.
///
/// Timestamps are read at whatever precision the name carries (a comment written
/// by an older clove keeps its nanoseconds *as an ordering key*); only the
/// rendering is canonicalized, by the surfaces that render it. The sort is
/// therefore over the full stored precision — see the total-order note on the
/// tiebreak below.
pub fn list_comments(issues_dir: &Utf8Path, id: &CloveId) -> Result<Vec<Comment>, CloveError> {
    let dir = comments_dir(issues_dir, id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let read_dir = std::fs::read_dir(&dir).map_err(|source| CloveError::Io {
        path: dir.clone(),
        source,
    })?;

    let mut comments = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| CloveError::Io {
            path: dir.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CloveError::Io {
            path: dir.clone(),
            source,
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let Some(name) = path.file_name() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let Some((timestamp, author)) = parse_comment_name(stem) else {
            continue; // not a recognizable comment file name
        };
        let body = std::fs::read_to_string(&path).map_err(|source| CloveError::Io {
            path: path.clone(),
            source,
        })?;
        comments.push((
            name.to_owned(),
            Comment {
                timestamp,
                author,
                body,
            },
        ));
    }

    // Chronological order (filename lex order already matches, but sort to be
    // robust against clock/format edge cases).
    //
    // The file name is the final tiebreak, which makes the order **total**.
    // Without it, two comments sharing a timestamp *and* an author fall back to
    // whatever order `read_dir` happened to yield — an order that can differ
    // between two reads of the same directory. That is reachable in practice,
    // not just in theory: `add_comment_at` takes an explicit timestamp, and the
    // GitHub comment pull stamps every pulled comment with the remote
    // `created_at`, which has *second* resolution. Names are unique within the
    // directory, so this never ties.
    comments.sort_by(|(a_name, a), (b_name, b)| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.author.cmp(&b.author))
            .then_with(|| a_name.cmp(b_name))
    });
    Ok(comments.into_iter().map(|(_, comment)| comment).collect())
}

/// The `comments/` directory for an item.
fn comments_dir(issues_dir: &Utf8Path, id: &CloveId) -> Utf8PathBuf {
    issues_dir.join(id.as_str()).join("comments")
}

/// Derive an author slug from an email: lowercase, non-alphanumeric → `-`,
/// truncated to 32 chars; empty results become `anon`.
fn author_slug(email: &str) -> String {
    let mut slug: String = email
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.truncate(32);
    if slug.chars().all(|c| c == '-') {
        return "anon".to_owned();
    }
    slug
}

/// Four random lowercase base-36 characters (lowercase keeps file names
/// distinct on case-insensitive filesystems such as macOS HFS+/APFS).
fn random_suffix() -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes).expect("getrandom: system entropy unavailable");
    bytes
        .iter()
        .map(|&b| ALPHABET[(b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Parse `<ts>-<author>-<rand4>` back into a timestamp and author. Returns
/// `None` if the name doesn't match the expected shape.
fn parse_comment_name(stem: &str) -> Option<(DateTime<Utc>, String)> {
    if stem.len() < FILENAME_TS_LEN + 2 {
        return None;
    }
    // Timestamp is the fixed-length prefix (contains no `-`), then a `-`.
    let ts_str = stem.get(0..FILENAME_TS_LEN)?;
    if stem.as_bytes().get(FILENAME_TS_LEN) != Some(&b'-') {
        return None;
    }
    let rest = stem.get(FILENAME_TS_LEN + 1..)?; // "<author>-<rand4>"
    let (author, _random) = rest.rsplit_once('-')?;

    let naive = NaiveDateTime::parse_from_str(ts_str, FILENAME_TS_FORMAT).ok()?;
    Some((naive.and_utc(), author.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (tempfile::TempDir, Utf8PathBuf, CloveId) {
        let tmp = tempfile::tempdir().unwrap();
        let issues = Utf8Path::from_path(tmp.path()).unwrap().join("issues");
        std::fs::create_dir_all(&issues).unwrap();
        (tmp, issues, CloveId::new("proj-7AF3K2MN").unwrap())
    }

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn add_then_list_roundtrips() {
        let (_tmp, issues, id) = temp();
        add_comment_at(
            &issues,
            &id,
            "Ege@Example.com",
            "First comment.",
            ts("2026-06-02T10:00:00Z"),
        )
        .unwrap();
        add_comment_at(
            &issues,
            &id,
            "ege@example.com",
            "Second comment.",
            ts("2026-06-02T11:00:00Z"),
        )
        .unwrap();

        let comments = list_comments(&issues, &id).unwrap();
        assert_eq!(comments.len(), 2);
        // Sorted chronologically.
        assert_eq!(comments[0].body, "First comment.");
        assert_eq!(comments[1].body, "Second comment.");
        assert_eq!(comments[0].timestamp, ts("2026-06-02T10:00:00Z"));
        assert_eq!(comments[0].author, "ege-example-com");
    }

    #[test]
    fn empty_when_no_comments() {
        let (_tmp, issues, id) = temp();
        assert!(list_comments(&issues, &id).unwrap().is_empty());
    }

    #[test]
    fn same_timestamp_produces_distinct_files() {
        let (_tmp, issues, id) = temp();
        let when = ts("2026-06-02T10:00:00Z");
        let a = add_comment_at(&issues, &id, "ege@example.com", "A", when).unwrap();
        let b = add_comment_at(&issues, &id, "ege@example.com", "B", when).unwrap();
        assert_ne!(a, b, "distinct files even at the same timestamp");
        assert_eq!(list_comments(&issues, &id).unwrap().len(), 2);
    }

    #[test]
    fn merge_simulation_keeps_both_branch_comments() {
        // Two branches each append a comment to the same item. Because each is a
        // distinct file (distinct random suffix), a git merge is a clean
        // two-file add — no conflict. We simulate that by writing both into the
        // same comments dir and confirming both survive.
        let (_tmp, issues, id) = temp();
        let when = ts("2026-06-02T10:00:00Z");
        add_comment_at(&issues, &id, "alice@example.com", "from branch A", when).unwrap();
        add_comment_at(&issues, &id, "bob@example.com", "from branch B", when).unwrap();

        let bodies: Vec<String> = list_comments(&issues, &id)
            .unwrap()
            .into_iter()
            .map(|c| c.body)
            .collect();
        assert_eq!(bodies.len(), 2);
        assert!(bodies.contains(&"from branch A".to_owned()));
        assert!(bodies.contains(&"from branch B".to_owned()));
    }

    #[test]
    fn author_slug_sanitizes_and_truncates() {
        assert_eq!(
            author_slug("Ege.Apak+test@Example.COM"),
            "ege-apak-test-example-com"
        );
        assert_eq!(author_slug(""), "anon");
        assert_eq!(author_slug("@@@"), "anon");
        assert_eq!(author_slug(&"x".repeat(100)).len(), 32);
    }

    #[test]
    fn filename_is_windows_safe() {
        let (_tmp, issues, id) = temp();
        let path = add_comment_at(
            &issues,
            &id,
            "ege@example.com",
            "hi",
            ts("2026-06-02T10:00:00Z"),
        )
        .unwrap();
        let name = path.file_name().unwrap();
        assert!(!name.contains(':'), "no colons in `{name}`");
        assert!(name.ends_with(".md"));
    }
}
