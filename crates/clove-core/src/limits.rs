//! Hard limits enforced at every entry point (DESIGN.md §4).
//!
//! These guard against pathological inputs (alias bombs, multi-megabyte
//! frontmatter, runaway dependency arrays) before any allocation or parse.

/// Maximum size of a YAML frontmatter block, in bytes. Files whose frontmatter
/// exceeds this are rejected before any allocation. 64 KiB.
pub const MAX_FRONTMATTER_BYTES: usize = 65_536;

/// Maximum size of an item body, in bytes. 4 MiB.
pub const MAX_BODY_BYTES: usize = 4_194_304;

/// Maximum number of entries allowed in any single dependency/relation list
/// field on one item (`deps`, `relates`, etc.). The total graph size is
/// uncapped — only the per-item array is bounded.
pub const MAX_DEP_ARRAY_LEN: usize = 1_000;

/// Maximum length of a full item ID (`<prefix>-<8 chars>`), in bytes.
pub const MAX_ID_LEN: usize = 32;

/// Maximum length of an ID prefix, in bytes.
pub const MAX_PREFIX_LEN: usize = 16;

/// Item count above which the CLI warns that an index is recommended. This is a
/// warning threshold, never a hard error.
pub const MAX_ITEMS_NO_INDEX_WARN: usize = 50_000;

// Compile-time invariants between the limits. These fail the build (not a test
// run) if a future edit makes the constants inconsistent.

const _: () = assert!(MAX_FRONTMATTER_BYTES < MAX_BODY_BYTES);
// prefix (<=MAX_PREFIX_LEN) + '-' + 8 Crockford chars must fit MAX_ID_LEN.
const _: () = assert!(MAX_PREFIX_LEN + 1 + 8 <= MAX_ID_LEN);
const _: () = assert!(MAX_DEP_ARRAY_LEN > 0);
const _: () = assert!(MAX_DEP_ARRAY_LEN <= MAX_ITEMS_NO_INDEX_WARN);
const _: () = assert!(MAX_ITEMS_NO_INDEX_WARN >= 1_000);
