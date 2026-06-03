//! Staleness detection and incremental resync (T-S03, DESIGN §6.2/§6.4).
//!
//! [`check_staleness`] diffs the `.clove/issues/` directory against the index
//! cheaply: a directory-mtime + file-count fast path, falling back to a per-file
//! content-hash comparison only for entries whose mtime changed (or that were
//! touched in the last 2s — the HFS+ granularity guard). [`apply_staleness`]
//! re-parses and upserts the changed/new items and removes the deleted ones in a
//! single transaction. Both read the oracle from the `items` table.

use std::collections::HashMap;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{parse_item_bytes, CloveId};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::db::{Index, IndexError};
use crate::write::{content_hash8, fts_rowid, write_row, RowMeta};

/// Files modified within this window are always hash-checked regardless of the
/// stored mtime — mtime granularity on some filesystems (HFS+) is too coarse to
/// distinguish a fast in-place rewrite (DESIGN §6.2).
const RECENT_WINDOW_MS: i64 = 2_000;

/// The set of items that differ between the files and the index.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StalenessReport {
    /// Indexed items whose file content changed.
    pub stale_ids: Vec<CloveId>,
    /// Item files with no corresponding index row.
    pub new_ids: Vec<CloveId>,
    /// Index rows whose file no longer exists.
    pub deleted_ids: Vec<CloveId>,
}

impl StalenessReport {
    /// Total number of items needing resync — the threshold the read-path
    /// wrapper (T-S06) checks before choosing incremental vs full file-scan.
    pub fn change_count(&self) -> usize {
        self.stale_ids.len() + self.new_ids.len() + self.deleted_ids.len()
    }

    /// True when the index is already in sync with the files.
    pub fn is_clean(&self) -> bool {
        self.change_count() == 0
    }
}

/// One item file discovered on disk.
struct DiskEntry {
    id: CloveId,
    path: Utf8PathBuf,
    mtime_ms: i64,
}

/// Scan `issues_dir` for `<id>.md` files, returning `(entries, any_recent)`.
/// Symlinks, directories (comment dirs), temp files, and names that are not
/// valid ids are skipped (mirrors the file store's scan rules, §12.3).
fn scan_dir(issues_dir: &Utf8Path, now_ms: i64) -> Result<(Vec<DiskEntry>, bool), IndexError> {
    let read_dir = std::fs::read_dir(issues_dir).map_err(|source| IndexError::IoError {
        path: issues_dir.to_owned(),
        source,
    })?;
    let mut entries = Vec::new();
    let mut any_recent = false;
    for entry in read_dir {
        let entry = entry.map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        if !file_type.is_file() {
            continue; // skip symlinks and comment directories
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue; // non-UTF-8 name
        };
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let Ok(id) = CloveId::new(stem) else {
            continue; // not a valid item id
        };
        let meta = entry.metadata().map_err(|source| IndexError::IoError {
            path: issues_dir.join(&name),
            source,
        })?;
        let mtime_ms = mtime_to_ms(&meta);
        if now_ms - mtime_ms < RECENT_WINDOW_MS {
            any_recent = true;
        }
        entries.push(DiskEntry {
            id,
            path: Utf8PathBuf::from_path_buf(entry.path())
                .unwrap_or_else(|_| issues_dir.join(&name)),
            mtime_ms,
        });
    }
    Ok((entries, any_recent))
}

/// File modification time as Unix epoch milliseconds (0 if unavailable).
fn mtime_to_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Directory mtime as Unix epoch milliseconds.
fn dir_mtime_ms(issues_dir: &Utf8Path) -> Result<i64, IndexError> {
    let meta = std::fs::metadata(issues_dir).map_err(|source| IndexError::IoError {
        path: issues_dir.to_owned(),
        source,
    })?;
    Ok(mtime_to_ms(&meta))
}

/// Detect which items differ between `issues_dir` and the index (T-S03).
pub fn check_staleness(
    conn: &Connection,
    issues_dir: &Utf8Path,
) -> Result<StalenessReport, IndexError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (entries, any_recent) = scan_dir(issues_dir, now_ms)?;
    let cur_dir_mtime = dir_mtime_ms(issues_dir)?;
    let cur_count = entries.len() as i64;

    // Level 1 — directory fast path. Trust it only when nothing was touched very
    // recently (coarse-mtime guard): a clean tree skips all hashing.
    let meta: Option<(i64, i64)> = conn
        .query_row(
            "SELECT dir_mtime, file_count FROM meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((dir_mtime, file_count)) = meta {
        if !any_recent && dir_mtime == cur_dir_mtime && file_count == cur_count {
            return Ok(StalenessReport::default());
        }
    }

    // Level 2 — per-file comparison against the items oracle.
    let mut stored: HashMap<String, (i64, Vec<u8>)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, file_mtime, content_hash FROM items")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        for row in rows {
            let (id, mtime, hash) = row?;
            stored.insert(id, (mtime, hash));
        }
    }

    let mut report = StalenessReport::default();
    let mut seen = HashMap::new();
    for entry in &entries {
        seen.insert(entry.id.as_str().to_owned(), ());
        match stored.get(entry.id.as_str()) {
            None => report.new_ids.push(entry.id.clone()),
            Some((stored_mtime, stored_hash)) => {
                let recent = now_ms - entry.mtime_ms < RECENT_WINDOW_MS;
                if entry.mtime_ms != *stored_mtime || recent {
                    // Content-hash gate: only an actual content change is stale.
                    let bytes =
                        std::fs::read(&entry.path).map_err(|source| IndexError::IoError {
                            path: entry.path.clone(),
                            source,
                        })?;
                    if content_hash8(&bytes).as_slice() != stored_hash.as_slice() {
                        report.stale_ids.push(entry.id.clone());
                    }
                }
            }
        }
    }
    for id in stored.keys() {
        if !seen.contains_key(id) {
            if let Ok(parsed) = CloveId::new(id) {
                report.deleted_ids.push(parsed);
            }
        }
    }
    Ok(report)
}

/// Read the `meta` staleness oracle row, if present.
fn read_meta(conn: &Connection) -> Result<Option<(i64, i64)>, IndexError> {
    let meta = conn
        .query_row(
            "SELECT dir_mtime, file_count FROM meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(meta)
}

/// Count item files **without** stat-ing each one (readdir + name validation
/// only). The cheap directory-level signal for the fast staleness path.
fn count_item_files(issues_dir: &Utf8Path) -> Result<i64, IndexError> {
    let read_dir = std::fs::read_dir(issues_dir).map_err(|source| IndexError::IoError {
        path: issues_dir.to_owned(),
        source,
    })?;
    let mut count = 0i64;
    for entry in read_dir {
        let entry = entry.map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        if !file_type.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name
            .strip_suffix(".md")
            .and_then(|stem| CloveId::new(stem).ok())
            .is_some()
        {
            count += 1;
        }
    }
    Ok(count)
}

/// Like [`check_staleness`] but O(readdir): when the directory mtime and file
/// count still match the `meta` oracle (and the directory was not touched within
/// the last 2 s), return "clean" without stat-ing or hashing any file. Only on a
/// directory-level mismatch does it fall back to the full per-file pass.
///
/// **Tradeoff (the `--deep` escape hatch exists for this):** a content rewrite
/// that changes neither the directory mtime nor the file count — i.e. an
/// *in-place* edit not done through clove's atomic rename — is not detected until
/// the next add/delete/rename, `git checkout`, or `reindex`. clove's own writes
/// go through an atomic rename, which bumps the directory mtime, so they are
/// always detected; use [`check_staleness`] (deep) when out-of-band in-place
/// edits must be caught.
pub fn check_staleness_fast(
    conn: &Connection,
    issues_dir: &Utf8Path,
) -> Result<StalenessReport, IndexError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let cur_dir_mtime = dir_mtime_ms(issues_dir)?;
    let cur_count = count_item_files(issues_dir)?;
    if let Some((dir_mtime, file_count)) = read_meta(conn)? {
        if dir_mtime == cur_dir_mtime
            && file_count == cur_count
            && now_ms - cur_dir_mtime >= RECENT_WINDOW_MS
        {
            return Ok(StalenessReport::default());
        }
    }
    // Something changed at the directory level (or the dir is freshly touched):
    // do the authoritative per-file pass to identify exactly what.
    check_staleness(conn, issues_dir)
}

/// Apply a [`StalenessReport`]: re-parse and upsert new/changed items, delete
/// removed ones, all in one transaction (T-S03).
///
/// Incremental sync is approximate by design: `topological_rank` is left unset
/// for resynced items and `has_dangling_deps` is computed against the current
/// on-disk id set. A periodic `reindex` restores exact ranks. Files that fail to
/// parse are skipped (the malformed-skip rule) so one bad file cannot wedge the
/// sweep.
pub fn apply_staleness(
    conn: &mut Connection,
    report: &StalenessReport,
    issues_dir: &Utf8Path,
) -> Result<(), IndexError> {
    if report.is_clean() {
        return Ok(());
    }
    // The set of ids that exist on disk now, to resolve dangling hard deps.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (entries, _) = scan_dir(issues_dir, now_ms)?;
    let live: std::collections::HashSet<String> =
        entries.iter().map(|e| e.id.as_str().to_owned()).collect();

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for id in report.new_ids.iter().chain(report.stale_ids.iter()) {
        let path = issues_dir.join(format!("{id}.md"));
        let Ok(bytes) = std::fs::read(&path) else {
            continue; // file vanished mid-sweep
        };
        let Ok(item) = parse_item_bytes(&bytes, &path, id) else {
            continue; // malformed-skip
        };
        let has_dangling_deps = item
            .frontmatter
            .deps
            .iter()
            .any(|d| !live.contains(d.as_str()));
        let meta = RowMeta {
            file_mtime_ms: file_mtime_ms(&path)?,
            content_hash: content_hash8(&bytes),
            topo_rank: None,
            has_dangling_deps,
        };
        write_row(&tx, &item, &meta)?;
    }
    for id in &report.deleted_ids {
        delete_row(&tx, id.as_str())?;
    }
    tx.commit()?;
    Ok(())
}

/// Remove an item and its edges/labels/FTS shadow row.
fn delete_row(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), IndexError> {
    tx.execute("DELETE FROM items WHERE id = ?1", params![id])?;
    tx.execute("DELETE FROM edges WHERE from_id = ?1", params![id])?;
    tx.execute("DELETE FROM labels WHERE item_id = ?1", params![id])?;
    tx.execute(
        "DELETE FROM items_fts WHERE rowid = ?1",
        params![fts_rowid(id)],
    )?;
    tx.execute(
        "DELETE FROM fts_map WHERE fts_rowid = ?1",
        params![fts_rowid(id)],
    )?;
    Ok(())
}

fn file_mtime_ms(path: &Utf8Path) -> Result<i64, IndexError> {
    let meta = std::fs::metadata(path).map_err(|source| IndexError::IoError {
        path: path.to_owned(),
        source,
    })?;
    Ok(mtime_to_ms(&meta))
}

impl Index {
    /// Detect items that differ between the files and the index — the thorough
    /// per-file pass (the `--deep` path); T-S03.
    pub fn check_staleness(&self, issues_dir: &Utf8Path) -> Result<StalenessReport, IndexError> {
        check_staleness(self.conn(), issues_dir)
    }

    /// Fast staleness check (O(readdir)): trusts the directory mtime + file count
    /// for the common clean case. See [`check_staleness_fast`] for the tradeoff.
    pub fn check_staleness_fast(
        &self,
        issues_dir: &Utf8Path,
    ) -> Result<StalenessReport, IndexError> {
        check_staleness_fast(self.conn(), issues_dir)
    }

    /// Apply a staleness report, resyncing the index in one transaction (T-S03).
    pub fn apply_staleness(
        &mut self,
        report: &StalenessReport,
        issues_dir: &Utf8Path,
    ) -> Result<(), IndexError> {
        apply_staleness(self.conn_mut(), report, issues_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reindex::reindex;
    use camino::Utf8PathBuf;

    /// A minimal valid item file body.
    fn item_md(id: &str, title: &str, body: &str) -> String {
        format!(
            "---\nschema: 1\nid: {id}\ntitle: {title}\nstatus: open\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n{body}\n"
        )
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        issues: Utf8PathBuf,
        db: Utf8PathBuf,
    }

    fn fixture(n: usize) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        for i in 0..n {
            let id = id_for(i);
            std::fs::write(
                issues.join(format!("{id}.md")),
                item_md(&id, &format!("Item {i}"), &format!("body number {i}")),
            )
            .unwrap();
        }
        let db = root.join(".clove/index.db");
        Fixture {
            _dir: dir,
            issues,
            db,
        }
    }

    fn id_for(i: usize) -> String {
        const ALPH: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let mut n = i as u32;
        let mut buf = [b'0'; 8];
        let mut p = 8;
        while n > 0 && p > 0 {
            p -= 1;
            buf[p] = ALPH[(n % 32) as usize];
            n /= 32;
        }
        format!("proj-{}", String::from_utf8(buf.to_vec()).unwrap())
    }

    /// Backdate every item file's mtime so the "recent file" guard does not
    /// force hashing — lets us exercise the genuine fast path / mtime logic.
    fn backdate(issues: &Utf8Path) {
        let past = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        for entry in std::fs::read_dir(issues).unwrap() {
            let p = entry.unwrap().path();
            filetime::set_file_mtime(&p, past).unwrap();
        }
        // Also backdate the directory itself.
        filetime::set_file_mtime(issues.as_std_path(), past).unwrap();
    }

    #[test]
    fn clean_tree_reports_no_changes() {
        let fx = fixture(20);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        // Re-sync meta dir_mtime to the backdated value via a fresh reindex.
        reindex(&fx.issues, &fx.db).unwrap();

        let index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert!(report.is_clean(), "unexpected changes: {report:?}");
    }

    #[test]
    fn detects_new_and_deleted() {
        let fx = fixture(5);
        reindex(&fx.issues, &fx.db).unwrap();
        // Add one, delete one.
        let new_id = id_for(999);
        std::fs::write(
            fx.issues.join(format!("{new_id}.md")),
            item_md(&new_id, "New", "fresh body"),
        )
        .unwrap();
        std::fs::remove_file(fx.issues.join(format!("{}.md", id_for(0)))).unwrap();

        let index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(report.new_ids.len(), 1, "{report:?}");
        assert_eq!(report.deleted_ids.len(), 1, "{report:?}");
        assert_eq!(report.new_ids[0].as_str(), new_id);
    }

    #[test]
    fn detects_modified_content_with_preserved_mtime() {
        // The HFS+ correctness case: replace files' *content* while keeping their
        // mtime, simulated by writing then restoring the original mtime. The
        // freshly-written files fall inside the recent-window guard, so the
        // content-hash gate detects all of them even though mtime is unchanged.
        let fx = fixture(10);
        reindex(&fx.issues, &fx.db).unwrap();

        for i in 0..10 {
            let id = id_for(i);
            let path = fx.issues.join(format!("{id}.md"));
            let original_mtime =
                filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap());
            std::fs::write(&path, item_md(&id, &format!("Item {i}"), "MUTATED body")).unwrap();
            // Preserve the original mtime ("cp -p" semantics).
            filetime::set_file_mtime(&path, original_mtime).unwrap();
        }

        let index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(report.stale_ids.len(), 10, "{report:?}");
    }

    #[test]
    fn apply_resyncs_index() {
        let fx = fixture(5);
        reindex(&fx.issues, &fx.db).unwrap();
        let new_id = id_for(999);
        std::fs::write(
            fx.issues.join(format!("{new_id}.md")),
            item_md(&new_id, "New", "fresh keyword body"),
        )
        .unwrap();
        std::fs::remove_file(fx.issues.join(format!("{}.md", id_for(0)))).unwrap();

        let mut index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        index.apply_staleness(&report, &fx.issues).unwrap();

        assert_eq!(index.item_count().unwrap(), 5);
        // A subsequent check is clean for the resynced rows.
        let after = index.check_staleness(&fx.issues).unwrap();
        assert!(
            after.stale_ids.is_empty() && after.deleted_ids.is_empty(),
            "{after:?}"
        );
    }

    /// Set the issues directory's mtime to a fixed point in the past.
    fn backdate_dir(issues: &camino::Utf8Path) {
        let past = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(issues.as_std_path(), past).unwrap();
    }

    #[test]
    fn fast_clean_when_directory_unchanged() {
        let fx = fixture(10);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap(); // meta.dir_mtime = backdated

        let index = Index::open(&fx.db).unwrap();
        assert!(index.check_staleness_fast(&fx.issues).unwrap().is_clean());
    }

    #[test]
    fn fast_detects_added_and_deleted() {
        let fx = fixture(5);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();

        // Adding a file changes the directory mtime + count → fast path falls to
        // the full pass and reports it.
        let new_id = id_for(999);
        std::fs::write(
            fx.issues.join(format!("{new_id}.md")),
            item_md(&new_id, "New", "body"),
        )
        .unwrap();
        let index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness_fast(&fx.issues).unwrap();
        assert_eq!(report.new_ids.len(), 1, "{report:?}");
    }

    #[test]
    fn fast_misses_inplace_edit_that_deep_catches() {
        // The documented tradeoff: an in-place content rewrite that does not
        // change the directory entry list (no add/delete/rename) is invisible to
        // the fast path but caught by the deep (`--deep`) path.
        let fx = fixture(5);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();

        let id = id_for(0);
        let path = fx.issues.join(format!("{id}.md"));
        // In-place rewrite: file mtime becomes "now", count unchanged.
        std::fs::write(&path, item_md(&id, "Item 0", "MUTATED body")).unwrap();
        // Pin the directory mtime back to the past to isolate the "directory
        // unchanged" condition the fast path trusts (some filesystems may have
        // bumped it; the point is the fast path believes nothing changed).
        backdate_dir(&fx.issues);

        let index = Index::open(&fx.db).unwrap();
        assert!(
            index.check_staleness_fast(&fx.issues).unwrap().is_clean(),
            "fast path trusts the unchanged directory and misses the in-place edit"
        );
        assert_eq!(
            index.check_staleness(&fx.issues).unwrap().stale_ids.len(),
            1,
            "deep path stats the file and catches the change"
        );
    }
}
