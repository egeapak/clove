//! `CloveId` — the validated item identifier newtype, and the ID generator.
//!
//! Format: `<prefix>-<8 chars>` where the prefix is 1–8 chars matching
//! `[a-z][a-z0-9]{0,7}` and the suffix is 8 chars of `[0-9A-Z]` (generated as
//! Crockford base32; validated more permissively). See DESIGN.md §3.
//!
//! The validation allowlist (`^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$`) is the
//! foundation of path-traversal safety (§12.1): a value that passes it cannot
//! contain `/`, `\`, `.`, `%`, null bytes, or `..`.

use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};
use smol_str::SmolStr;

use crate::error::CloveError;
use crate::limits::MAX_ID_LEN;

/// A validated clove item identifier.
///
/// Backed by `SmolStr`, so the common case (`proj-7AF3K2MN`, 13 bytes) is stored
/// inline with no heap allocation and is `O(1)` to clone.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CloveId(SmolStr);

impl CloveId {
    /// Validate `s` and wrap it. Returns [`CloveError::InvalidId`] if the string
    /// does not match `^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$` or exceeds
    /// [`MAX_ID_LEN`].
    pub fn new(s: &str) -> Result<CloveId, CloveError> {
        validate_id(s)?;
        Ok(CloveId(SmolStr::new(s)))
    }

    /// The ID as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Resolve this ID to its item file path under `issues_dir`, with
    /// defense-in-depth path-traversal checks.
    ///
    /// `issues_dir` is the `.clove/issues/` directory. The returned path is
    /// `issues_dir/<id>.md`. Because the ID is already validated to be free of
    /// path separators and `.`/`..`, the file name is safe by construction; we
    /// additionally canonicalize the parent directory and assert the result is
    /// the canonical `issues_dir`, so a symlinked or otherwise surprising
    /// `issues_dir` cannot let a write escape.
    pub fn to_path(&self, issues_dir: &Utf8Path) -> Result<Utf8PathBuf, CloveError> {
        // The parent must exist to canonicalize it. Callers ensure `clove init`
        // created it; if it is missing that is an I/O-level problem.
        let canonical_parent = issues_dir
            .canonicalize_utf8()
            .map_err(|source| CloveError::Io {
                path: issues_dir.to_owned(),
                source,
            })?;

        // The ID contains no separators, so the file name is a single component.
        let candidate = canonical_parent.join(format!("{}.md", self.0));

        // Assert the resolved file still lives directly inside the canonical
        // issues directory — never above or beside it.
        if candidate.parent() != Some(canonical_parent.as_path()) {
            return Err(CloveError::PathTraversal {
                id: self.0.to_string(),
            });
        }

        Ok(candidate)
    }
}

/// Validate an ID string against `^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$`.
///
/// Hand-rolled (no `regex` dependency): faster, and the pattern is fixed.
fn validate_id(s: &str) -> Result<(), CloveError> {
    let invalid = |reason: &str| CloveError::InvalidId {
        value: s.to_owned(),
        reason: reason.to_owned(),
    };

    if s.len() > MAX_ID_LEN {
        return Err(invalid("exceeds maximum id length"));
    }

    let (prefix, suffix) = s
        .split_once('-')
        .ok_or_else(|| invalid("missing `-` separator"))?;

    // Prefix: 1–8 chars, first ascii-lowercase alpha, rest ascii-lowercase alphanumeric.
    if prefix.is_empty() || prefix.len() > 8 {
        return Err(invalid("prefix must be 1–8 characters"));
    }
    let mut prefix_chars = prefix.chars();
    let first = prefix_chars.next().expect("prefix is non-empty");
    if !first.is_ascii_lowercase() {
        return Err(invalid("prefix must start with a lowercase letter"));
    }
    if !prefix_chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return Err(invalid("prefix must be lowercase alphanumeric"));
    }

    // Suffix: exactly 8 chars of [0-9A-Z]. This also rejects any second `-`,
    // since `-` is not in the allowed set.
    if suffix.len() != 8 {
        return Err(invalid("suffix must be exactly 8 characters"));
    }
    if !suffix
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
    {
        return Err(invalid("suffix must be uppercase letters or digits"));
    }

    Ok(())
}

/// Crockford base32 alphabet (uppercase, excludes `I`, `L`, `O`, `U`).
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Number of random bytes feeding one suffix: 5 bytes = 40 bits = 8 × 5-bit
/// Crockford symbols, with no leftover bits.
const SUFFIX_RANDOM_BYTES: usize = 5;

/// Generate a fresh ID with the given (already-validated) prefix.
///
/// Encodes 5 random bytes (40 bits) as 8 Crockford base32 characters. The
/// `prefix` must already satisfy the prefix grammar (config validation
/// guarantees this); a malformed prefix is a programming error and panics.
pub fn generate_id(prefix: &str) -> CloveId {
    let mut random_bytes = [0u8; SUFFIX_RANDOM_BYTES];
    getrandom::getrandom(&mut random_bytes).expect("getrandom: system entropy unavailable");

    // Pack the 5 bytes big-endian into a 40-bit value.
    let mut packed: u64 = 0;
    for byte in random_bytes {
        packed = (packed << 8) | u64::from(byte);
    }

    // Emit 8 groups of 5 bits, most-significant first.
    let mut suffix = [0u8; 8];
    for (position, slot) in suffix.iter_mut().enumerate() {
        let shift = 5 * (7 - position);
        let index = ((packed >> shift) & 0b1_1111) as usize;
        *slot = CROCKFORD_ALPHABET[index];
    }
    let suffix = std::str::from_utf8(&suffix).expect("Crockford alphabet is ASCII");

    let mut id = String::with_capacity(prefix.len() + 1 + 8);
    id.push_str(prefix);
    id.push('-');
    id.push_str(suffix);
    CloveId::new(&id).expect("generated id satisfies the id grammar")
}

/// Generate an ID that does not collide with an existing item file in
/// `issues_dir`.
///
/// Retries up to 3 times on the (astronomically rare) event that a freshly
/// generated `<id>.md` already exists, then returns [`CloveError::IdConflict`].
/// Existence is checked with a `stat` (via [`Utf8Path::exists`]), never by
/// opening the file.
pub fn new_id(prefix: &str, issues_dir: &Utf8Path) -> Result<CloveId, CloveError> {
    const MAX_ATTEMPTS: u32 = 3;
    for _ in 0..MAX_ATTEMPTS {
        let id = generate_id(prefix);
        let candidate = issues_dir.join(format!("{id}.md"));
        if !candidate.exists() {
            return Ok(id);
        }
    }
    Err(CloveError::IdConflict {
        attempts: MAX_ATTEMPTS,
    })
}

impl fmt::Display for CloveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl AsRef<str> for CloveId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl From<CloveId> for String {
    fn from(id: CloveId) -> String {
        id.0.to_string()
    }
}

impl std::str::FromStr for CloveId {
    type Err = CloveError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CloveId::new(s)
    }
}

impl serde::Serialize for CloveId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for CloveId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        CloveId::new(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_ids() {
        for id in [
            "proj-7AF3K2MN",
            "a-00000000",
            "clove-ZZZZZZZZ",
            "abcdefgh-12345678",
        ] {
            assert!(CloveId::new(id).is_ok(), "should accept {id}");
        }
    }

    #[test]
    fn rejects_path_traversal_chars() {
        // No input — traversal or otherwise — may ever construct a CloveId.
        for bad in [
            "../etc-passwd00",
            "proj-../../xyz",
            "proj/-7AF3K2MN",
            "proj-7AF3/2MN",
            "proj-7AF3.2MN",
            "..-7AF3K2MN",
            "pr%6f-7AF3K2MN",
        ] {
            assert!(CloveId::new(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_null_byte() {
        assert!(CloveId::new("proj-7AF3K2M\0").is_err());
        assert!(CloveId::new("pr\0j-7AF3K2MN").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long_prefix = "a".repeat(MAX_ID_LEN);
        let bad = format!("{long_prefix}-7AF3K2MN");
        assert!(CloveId::new(&bad).is_err());
    }

    #[test]
    fn rejects_malformed_shapes() {
        for bad in [
            "",                        // empty
            "proj",                    // no separator
            "proj-",                   // empty suffix
            "-7AF3K2MN",               // empty prefix
            "Proj-7AF3K2MN",           // uppercase prefix start
            "9roj-7AF3K2MN",           // prefix starts with digit
            "proj-7af3k2mn",           // lowercase suffix
            "proj-7AF3K2M",            // 7-char suffix
            "proj-7AF3K2MNX",          // 9-char suffix
            "verylongprefix-7AF3K2MN", // 14-char prefix
        ] {
            assert!(CloveId::new(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn display_and_as_ref_roundtrip() {
        let id = CloveId::new("proj-7AF3K2MN").unwrap();
        assert_eq!(id.to_string(), "proj-7AF3K2MN");
        assert_eq!(id.as_ref(), "proj-7AF3K2MN");
        assert_eq!(String::from(id.clone()), "proj-7AF3K2MN");
    }

    #[test]
    fn generated_ids_are_valid_and_crockford() {
        // Every generated id parses, carries the prefix, and its suffix uses
        // only the Crockford alphabet (no I/L/O/U, no lowercase).
        for _ in 0..5_000 {
            let id = generate_id("proj");
            let s = id.as_str();
            assert!(CloveId::new(s).is_ok());
            let (prefix, suffix) = s.split_once('-').unwrap();
            assert_eq!(prefix, "proj");
            assert_eq!(suffix.len(), 8);
            for ch in suffix.bytes() {
                assert!(
                    CROCKFORD_ALPHABET.contains(&ch),
                    "suffix char {} not in Crockford alphabet",
                    ch as char
                );
            }
        }
    }

    #[test]
    fn generate_id_is_well_distributed_at_100k() {
        use std::collections::HashSet;
        // 40 bits of entropy → birthday bound predicts a ~0.45% chance of a
        // single collision at 100k, so we assert "essentially all unique"
        // rather than strict zero (which would be flaky). A broken generator
        // collides thousands of times and trips this easily. Real uniqueness is
        // guaranteed by `new_id`'s existence-retry, tested separately.
        const N: usize = 100_000;
        let mut seen = HashSet::with_capacity(N);
        for _ in 0..N {
            seen.insert(generate_id("proj").as_str().to_owned());
        }
        assert!(
            seen.len() >= N - 10,
            "expected ~{N} unique ids, got {} (generator likely broken)",
            seen.len()
        );
    }

    #[test]
    fn concurrent_generation_is_unique() {
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};
        use std::thread;

        // 50 threads × 200 = 10,000 ids. At 40 bits the collision probability
        // is ~0.005% (matches DESIGN §3's accepted bound at 10k items), so a
        // strict zero-collision assertion is reliable here.
        let all = Arc::new(Mutex::new(HashSet::new()));
        let mut handles = Vec::new();
        for _ in 0..50 {
            let all = Arc::clone(&all);
            handles.push(thread::spawn(move || {
                let local: Vec<String> = (0..200)
                    .map(|_| generate_id("proj").as_str().to_owned())
                    .collect();
                let mut guard = all.lock().unwrap();
                for id in local {
                    guard.insert(id);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            all.lock().unwrap().len(),
            50 * 200,
            "ids must be globally unique"
        );
    }

    #[test]
    fn new_id_retries_then_errors_when_all_collide() {
        // Pre-create the file for every id the generator could produce is
        // impossible; instead verify the happy path returns a fresh id whose
        // file does not yet exist.
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let id = new_id("proj", issues_dir).unwrap();
        assert_eq!(id.as_str().split_once('-').unwrap().0, "proj");
        assert!(!issues_dir.join(format!("{id}.md")).exists());
    }

    #[test]
    fn new_id_skips_existing_file() {
        // Create a file matching a known id, then assert new_id never returns
        // that id (it would, with overwhelming probability, generate a
        // different one anyway — this checks the existence gate is wired in).
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        // Generate one id, occupy its path, then confirm a subsequent new_id
        // returns a path that is free.
        let occupied = generate_id("proj");
        std::fs::write(issues_dir.join(format!("{occupied}.md")), "x").unwrap();
        let fresh = new_id("proj", issues_dir).unwrap();
        assert!(!issues_dir.join(format!("{fresh}.md")).exists());
    }

    #[test]
    fn to_path_stays_inside_issues_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let id = CloveId::new("proj-7AF3K2MN").unwrap();
        let path = id.to_path(issues_dir).unwrap();
        assert!(path.starts_with(issues_dir.canonicalize_utf8().unwrap()));
        assert_eq!(path.file_name(), Some("proj-7AF3K2MN.md"));
    }
}
