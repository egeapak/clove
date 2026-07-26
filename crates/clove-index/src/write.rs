//! The single item write path (T-S02).
//!
//! [`upsert_item`] is the only public mutator of the `items` table. It updates
//! `items`, `edges`, and `labels` inside one `BEGIN IMMEDIATE` transaction so
//! the side tables can never drift from the row data (DESIGN §6.3). Bulk
//! loading ([`crate::reindex`]) and incremental
//! resync ([`crate::stale`]) reuse the lower-level [`write_row`] under their own
//! transactions, supplying the file `mtime`, content hash, dangling flag, and
//! topological rank that a lone [`Item`] does not carry.

use clove_core::graph::EdgeKind;
use clove_types::Item;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};

use crate::db::{Index, IndexError};

/// First 8 bytes of the BLAKE3 hash of `bytes` — the index's content
/// fingerprint (DESIGN §6.1: BLAKE3, not xxHash3; stored as `BLOB(8)`).
pub(crate) fn content_hash8(bytes: &[u8]) -> [u8; 8] {
    let hash = blake3::hash(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&hash.as_bytes()[..8]);
    out
}

/// Sentinel `topological_rank` for items whose rank is unknown (incremental
/// write-through, or a cyclic hard-dependency graph). Stored instead of `NULL`
/// so the list query can order by a plain `(priority, topological_rank, id)` and
/// use the `idx_items_list` covering index; the large value sorts these items
/// last, matching the file path's `usize::MAX` treatment.
pub(crate) const UNRANKED_TOPO: i64 = i64::MAX;

/// Per-row index metadata that is not derivable from an [`Item`] alone.
pub(crate) struct RowMeta {
    /// File modification time, Unix epoch milliseconds.
    pub file_mtime_ms: i64,
    /// First 8 bytes of BLAKE3 over the file's bytes.
    pub content_hash: [u8; 8],
    /// Topological rank over the hard-dependency graph (`None` when unknown — it
    /// is persisted as [`UNRANKED_TOPO`], never `NULL`).
    pub topo_rank: Option<i64>,
    /// Whether the item references at least one missing hard dependency.
    pub has_dangling_deps: bool,
    /// Whether the item is in a hard-dependency cycle or has a malformed parent
    /// (excluded from `ready`/`blocked`). Like `topo_rank`, this is a derived
    /// graph property; the incremental path fixes it via `recompute_derived`.
    pub excluded: bool,
}

/// The single public write path: upsert one item and its relationships.
///
/// Best-effort metadata is used for the incremental write-through case: the file
/// mtime is set to "now" and the content hash is taken over the item body. The
/// authoritative `mtime`/`content_hash` (over the actual file bytes) are written
/// by [`crate::reindex`] and reconciled by [`crate::stale::apply_staleness`];
/// any discrepancy only costs a redundant re-parse on the next staleness sweep.
pub fn upsert_item(conn: &mut Connection, item: &Item) -> Result<(), IndexError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let meta = RowMeta {
        file_mtime_ms: now_ms,
        content_hash: content_hash8(item.body.as_bytes()),
        topo_rank: None,
        has_dangling_deps: false,
        excluded: false,
    };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    write_row(&tx, item, &meta)?;
    tx.commit()?;
    Ok(())
}

/// Write one item's rows within an existing transaction. The shared core of the
/// write-through, reindex, and resync paths.
///
/// ## Why the timestamp columns keep `to_rfc3339()` (READ_PATH_ROADMAP §3)
///
/// `created_at`/`updated_at`/`closed_at` are written in chrono's `+00:00`
/// spelling rather than the canonical `Z` one, deliberately. They are an
/// **internal ordering key**: nothing selects them into a public payload (the
/// list projection is [`crate::ItemListRow`], which carries no timestamps at
/// all), so their spelling is never user-visible. Re-spelling them would change
/// the bytes of every row in every existing index and therefore require a
/// `SCHEMA_VERSION` bump — a full rebuild for every user — to buy nothing. What
/// *is* canonical is the value they are rendered from: `ItemFrontmatter`
/// normalizes on deserialize, so a hand-edited `+02:00` or a sub-second fraction
/// is already truncated to the canonical instant before it reaches this row.
pub(crate) fn write_row(
    tx: &Transaction<'_>,
    item: &Item,
    meta: &RowMeta,
) -> Result<(), IndexError> {
    let fm = &item.frontmatter;
    let id = fm.id.as_str();
    let labels_json = serde_json::to_string(&fm.labels).expect("Vec<String> serializes");

    // (1) items
    tx.execute(
        "INSERT OR REPLACE INTO items
            (id, title, status, item_type, priority, assignee, parent_id,
             topological_rank, has_dangling_deps, excluded, labels, created_at, updated_at,
             closed_at, file_mtime, content_hash, source_system, external_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            id,
            fm.title,
            fm.status.as_str(),
            fm.item_type.as_str(),
            fm.priority.get(),
            fm.assignee,
            fm.parent.as_ref().map(|p| p.as_str()),
            meta.topo_rank.unwrap_or(UNRANKED_TOPO),
            meta.has_dangling_deps,
            meta.excluded,
            labels_json,
            fm.created.to_rfc3339(),
            fm.updated.to_rfc3339(),
            fm.closed.map(|c| c.to_rfc3339()),
            meta.file_mtime_ms,
            &meta.content_hash[..],
            fm.source_system,
            fm.external_ref,
        ],
    )?;

    // (2,3) edges: replace this item's outgoing edges. ParentOf is intentionally
    // not stored here — the parent link lives in items.parent_id, and the ready
    // query (DESIGN §6.5) only consults DependsOn edges.
    tx.execute("DELETE FROM edges WHERE from_id = ?1", params![id])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO edges (from_id, to_id, kind) VALUES (?1, ?2, ?3)",
        )?;
        for dep in &fm.deps {
            stmt.execute(params![id, dep.as_str(), EdgeKind::DependsOn as u8])?;
        }
        for rel in &fm.relates {
            stmt.execute(params![id, rel.as_str(), EdgeKind::Relates as u8])?;
        }
        for dup in &fm.duplicates {
            stmt.execute(params![id, dup.as_str(), EdgeKind::Duplicates as u8])?;
        }
        for sup in &fm.supersedes {
            stmt.execute(params![id, sup.as_str(), EdgeKind::Supersedes as u8])?;
        }
    }

    // (4,5) labels
    tx.execute("DELETE FROM labels WHERE item_id = ?1", params![id])?;
    {
        let mut stmt =
            tx.prepare_cached("INSERT OR IGNORE INTO labels (item_id, label) VALUES (?1, ?2)")?;
        for label in &fm.labels {
            stmt.execute(params![id, label])?;
        }
    }

    Ok(())
}

impl Index {
    /// Upsert one item through the single encapsulated write path (T-S02).
    pub fn upsert_item(&mut self, item: &Item) -> Result<(), IndexError> {
        upsert_item(self.conn_mut(), item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Index;
    use camino::Utf8PathBuf;
    use clove_types::{CloveId, ItemFrontmatter, ItemStatus, ItemType, Priority};

    fn index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("index.db")).unwrap();
        (dir, Index::open(&path).unwrap())
    }

    fn item(id: &str, title: &str, body: &str) -> Item {
        Item {
            frontmatter: ItemFrontmatter {
                schema: 1,
                id: CloveId::new(id).unwrap(),
                title: title.to_owned(),
                status: ItemStatus::Open,
                item_type: ItemType::Feature,
                priority: Priority::DEFAULT,
                created: "2026-06-02T10:00:00Z".parse().unwrap(),
                updated: "2026-06-02T10:00:00Z".parse().unwrap(),
                closed: None,
                assignee: None,
                parent: None,
                labels: Vec::new(),
                deps: Vec::new(),
                relates: Vec::new(),
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: None,
                external_ref: None,
            },
            body: body.to_owned(),
        }
    }

    /// The timestamp columns keep the `+00:00` spelling `to_rfc3339()` renders,
    /// deliberately (READ_PATH_ROADMAP §3 — see the `write_row` note).
    ///
    /// Canonicalizing them would rewrite the bytes of every row in every
    /// existing index, which is only safe behind a [`crate::SCHEMA_VERSION`]
    /// bump — a full rebuild for every user, to change a string no surface ever
    /// renders. **If this test starts failing, that change needs a schema bump.**
    #[test]
    fn timestamp_columns_keep_their_stored_spelling() {
        let (_d, mut index) = index();
        let mut it = item("proj-AAAA1111", "t", "b");
        it.frontmatter.closed = Some("2026-06-02T11:00:00Z".parse().unwrap());
        it.frontmatter.status = ItemStatus::Closed;
        index.upsert_item(&it).unwrap();

        let row: (String, String, Option<String>) = index
            .conn()
            .query_row(
                "SELECT created_at, updated_at, closed_at FROM items",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "2026-06-02T10:00:00+00:00".to_owned(),
                "2026-06-02T10:00:00+00:00".to_owned(),
                Some("2026-06-02T11:00:00+00:00".to_owned())
            )
        );
    }

    /// ...but the *values* they are written from are canonical even when the
    /// file was not, so the index and the file path cannot rank the same two
    /// items differently. Parsed from raw bytes, not built in memory: an
    /// in-memory `ItemFrontmatter` never carries a foreign spelling.
    #[test]
    fn a_sub_second_file_timestamp_is_normalized_before_it_reaches_a_row() {
        let (_d, mut index) = index();
        let raw = concat!(
            "---\nschema: 1\nid: proj-AAAA1111\ntitle: Hand edited\nstatus: open\n",
            "type: feature\npriority: 2\n",
            "created: 2026-06-02T10:00:00.904816670+00:00\n",
            "updated: 2026-06-02T10:00:00.904816670+00:00\n---\nbody\n"
        )
        .as_bytes();
        let it = clove_core::parse_item_bytes(
            raw,
            camino::Utf8Path::new("proj-AAAA1111.md"),
            &CloveId::new("proj-AAAA1111").unwrap(),
        )
        .unwrap();
        index.upsert_item(&it).unwrap();

        let created: String = index
            .conn()
            .query_row("SELECT created_at FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(created, "2026-06-02T10:00:00+00:00", "no stored fraction");
    }

    /// Bulk upserts stay internally consistent: one `items` row and one
    /// `labels` row per item, with no orphans left behind.
    ///
    /// This replaces an FTS-shadow-consistency test that schema 6 deleted along
    /// with `items_fts`; the invariant it was really guarding — the side tables
    /// track `items` exactly across many writes — now rides on `labels`.
    #[test]
    fn side_tables_stay_consistent_over_many_items() {
        let (_d, mut index) = index();
        for i in 0..100 {
            let id = format!("proj-{:0>8}", radix(i));
            let mut it = item(&id, &format!("title {i}"), &format!("body keyword{i}"));
            it.frontmatter.labels = vec![format!("n:{i}"), "shared".to_owned()];
            index.upsert_item(&it).unwrap();
        }
        assert_eq!(index.item_count().unwrap(), 100);
        let labels: i64 = index
            .conn()
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(labels, 200, "two labels per item, none duplicated");
        assert_eq!(index.integrity_check().unwrap(), None);
    }

    #[test]
    fn edges_and_labels_replaced_on_reupsert() {
        let (_d, mut index) = index();
        let mut it = item("proj-AAAA1111", "t", "b");
        it.frontmatter.deps = vec![CloveId::new("proj-BBBB2222").unwrap()];
        it.frontmatter.labels = vec!["area:core".to_owned()];
        index.upsert_item(&it).unwrap();

        let edge_count = |idx: &Index| -> i64 {
            idx.conn()
                .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
                .unwrap()
        };
        let label_count = |idx: &Index| -> i64 {
            idx.conn()
                .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(edge_count(&index), 1);
        assert_eq!(label_count(&index), 1);

        // Replace deps/labels: counts reflect the new set, not the union.
        it.frontmatter.deps.clear();
        it.frontmatter.labels = vec!["area:ui".to_owned(), "p:high".to_owned()];
        index.upsert_item(&it).unwrap();
        assert_eq!(edge_count(&index), 0);
        assert_eq!(label_count(&index), 2);
    }

    /// Encode a small integer into a valid 8-char Crockford-ish suffix (digits
    /// then uppercase letters) for synthetic ids in tests.
    fn radix(mut n: u32) -> String {
        const ALPH: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let mut buf = [b'0'; 8];
        let mut i = 8;
        while n > 0 && i > 0 {
            i -= 1;
            buf[i] = ALPH[(n % 32) as usize];
            n /= 32;
        }
        String::from_utf8(buf.to_vec()).unwrap()
    }
}
