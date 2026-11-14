//! Staleness detection and incremental resync (T-S03, DESIGN §6.2/§6.4).
//!
//! [`check_staleness`] (the `--deep` path) diffs the `.clove/issues/` directory
//! against the index file by file: it stats every item and compares content
//! hashes only for entries whose mtime or size changed (or that were touched in
//! the last 2s — the HFS+ granularity guard). [`check_staleness_fast`] adds a
//! directory-mtime + file-count fast path in front of it. [`apply_staleness`]
//! re-parses and upserts the changed/new items, removes the deleted ones, and
//! refreshes the stored mtime and size of files that moved without a content
//! change, in a single transaction. Both read the oracle from the `items` table.

use std::collections::HashMap;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::parse_item_bytes;
use clove_types::CloveId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::db::{Index, IndexError};
use crate::write::{content_hash8, write_row, RowMeta};

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
    /// The `(dir_mtime_ms, file_count)` of `issues_dir` observed when this
    /// report was computed. [`apply_staleness`] records it as the new `meta`
    /// oracle — the *check-time* snapshot, so a file added between check and
    /// apply leaves the stored mtime behind the directory's and the fast path
    /// falls through to a re-check instead of masking the file.
    pub observed: Option<(i64, i64)>,
    /// Indexed items whose file mtime or size moved while the content hash
    /// stayed the same (a `touch`, a git checkout round-trip). Not a change:
    /// [`StalenessReport::is_clean`] ignores them. [`apply_staleness`] stores
    /// their check-time mtime and size, so later checks do not hash them again.
    pub touched: Vec<TouchedFile>,
    /// True when `observed` differs from the stored `meta` row, so applying this
    /// report moves the fast path's oracle forward even when nothing changed.
    pub meta_outdated: bool,
}

/// A file whose mtime or size moved without a content change, with the values
/// observed at check time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedFile {
    /// The item.
    pub id: CloveId,
    /// File mtime, Unix epoch milliseconds.
    pub mtime_ms: i64,
    /// File mtime, Unix epoch nanoseconds (the `file_mtimes` column).
    pub mtime_ns: i64,
    /// File size in bytes.
    pub size: i64,
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

    /// True when [`apply_staleness`] would write anything: a change, or only
    /// bookkeeping (touched files, an outdated `meta` row). A caller that
    /// refreshes the index applies when this is true, not only when the report
    /// is dirty. Otherwise a clean outcome after mtimes moved is never recorded,
    /// and every later check repeats the per-file stat pass and the hashing.
    pub fn needs_apply(&self) -> bool {
        !self.is_clean() || !self.touched.is_empty() || self.meta_outdated
    }
}

/// One item file discovered on disk.
struct DiskEntry {
    id: CloveId,
    path: Utf8PathBuf,
    mtime_ms: i64,
    mtime_ns: i64,
    size: i64,
}

/// Scan `issues_dir` for `<id>.md` files.
/// Symlinks, directories (comment dirs), temp files, and names that are not
/// valid ids are skipped (mirrors the file store's scan rules, §12.3).
fn scan_dir(issues_dir: &Utf8Path) -> Result<Vec<DiskEntry>, IndexError> {
    let read_dir = std::fs::read_dir(issues_dir).map_err(|source| IndexError::IoError {
        path: issues_dir.to_owned(),
        source,
    })?;
    let mut entries = Vec::new();
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
        let mtime_ns = mtime_to_ns(&meta);
        entries.push(DiskEntry {
            id,
            path: Utf8PathBuf::from_path_buf(entry.path())
                .unwrap_or_else(|_| issues_dir.join(&name)),
            mtime_ms,
            mtime_ns,
            size: meta.len() as i64,
        });
    }
    Ok(entries)
}

/// File modification time as Unix epoch milliseconds (0 if unavailable).
fn mtime_to_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// File modification time as Unix epoch nanoseconds (0 if unavailable).
fn mtime_to_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
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

/// Detect which items differ between `issues_dir` and the index (T-S03) — the
/// thorough per-file pass behind `--deep`.
///
/// This **always** stats every item file and compares it with the `items`
/// oracle; it never trusts the directory mtime + file count. That short-circuit
/// belongs to [`check_staleness_fast`] only. An in-place (truncate-and-rewrite)
/// edit changes the file's mtime but not the directory's, so a directory-level
/// shortcut here made such an edit invisible to `--deep` as soon as it was older
/// than the recent-mtime window.
///
/// A file is hashed when its mtime or size differs from the stored one, or when
/// it was touched within the last 2 s (the coarse-mtime guard). The size check
/// catches a rewrite that kept (or restored) the old mtime but changed the
/// length. A rewrite that keeps both the mtime and the size is not detected
/// outside the recent window; only hashing every file would catch it.
pub fn check_staleness(
    conn: &Connection,
    issues_dir: &Utf8Path,
) -> Result<StalenessReport, IndexError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    // The directory mtime is read **before** the scan. It becomes the `meta`
    // oracle the fast path trusts, so it must not be newer than what the scan
    // saw: a file renamed into place after its stat but before a later read of
    // the directory mtime would be recorded as "seen" without being in the
    // report, and the fast path would then call the index clean. Read first,
    // such an edit leaves the stored mtime behind the directory's, and the next
    // check does the per-file pass.
    let cur_dir_mtime = dir_mtime_ms(issues_dir)?;
    let entries = scan_dir(issues_dir)?;
    #[cfg(test)]
    tests::after_scan_hook();
    let cur_count = entries.len() as i64;

    // Per-file comparison against the items oracle.
    let mut stored: HashMap<String, (i64, i64, Vec<u8>)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, file_mtime, file_size, content_hash FROM items")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, mtime, size, hash) = row?;
            stored.insert(id, (mtime, size, hash));
        }
    }

    let observed = (cur_dir_mtime, cur_count);
    let mut report = StalenessReport {
        observed: Some(observed),
        meta_outdated: read_meta(conn)? != Some(observed),
        ..StalenessReport::default()
    };
    let mut seen = HashMap::new();
    for entry in &entries {
        seen.insert(entry.id.as_str().to_owned(), ());
        match stored.get(entry.id.as_str()) {
            None => report.new_ids.push(entry.id.clone()),
            Some((stored_mtime, stored_size, stored_hash)) => {
                let recent = now_ms - entry.mtime_ms < RECENT_WINDOW_MS;
                let moved = entry.mtime_ms != *stored_mtime || entry.size != *stored_size;
                if moved || recent {
                    // Content-hash gate: only an actual content change is stale.
                    let bytes =
                        std::fs::read(&entry.path).map_err(|source| IndexError::IoError {
                            path: entry.path.clone(),
                            source,
                        })?;
                    if content_hash8(&bytes).as_slice() != stored_hash.as_slice() {
                        report.stale_ids.push(entry.id.clone());
                    } else if moved {
                        report.touched.push(TouchedFile {
                            id: entry.id.clone(),
                            mtime_ms: entry.mtime_ms,
                            mtime_ns: entry.mtime_ns,
                            size: entry.size,
                        });
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
/// removed ones, then recompute the derived graph columns — all in one
/// transaction (T-S03; M4 P1 made the derived columns exact).
///
/// After the row writes, [`crate::derive::recompute_derived`] rebuilds the
/// dependency graph from the index's own `items`/`edges` tables and writes back
/// exact `topological_rank`, `has_dangling_deps`, and `excluded` values (no file
/// re-scan). So an incremental apply now produces the **same** derived state a
/// full `reindex` would — list ordering and cycle/dangling exclusion no longer
/// drift between reindexes. Files that fail to parse are skipped (the
/// malformed-skip rule) so one bad file cannot wedge the sweep.
///
/// **Topology-change guard (M4):** the derived columns are a function of the
/// dependency *structure* only, so the global recompute runs only when this batch
/// actually changed it (an item added/deleted, or a changed item's edge/parent
/// set differs). A content-only edit — `status`, `title`, `assignee`, `priority`,
/// `labels` — preserves its existing exact derived values and skips the O(V+E)
/// recompute entirely.
pub fn apply_staleness(
    conn: &mut Connection,
    report: &StalenessReport,
    issues_dir: &Utf8Path,
) -> Result<(), IndexError> {
    apply_staleness_tracked(conn, report, issues_dir).map(|_| ())
}

/// The dependency-structure signature of an item: its outgoing edges (by kind)
/// plus its parent. Two items with equal signatures yield identical derived
/// columns, so a batch that does not change any signature can skip the recompute.
#[derive(PartialEq, Eq)]
struct Signature {
    edges: std::collections::BTreeSet<(u8, String)>,
    parent: Option<String>,
}

/// The derived columns of one row, snapshotted before an overwrite so a
/// content-only edit can write them straight back unchanged.
struct DerivedCols {
    topo_rank: i64,
    has_dangling: bool,
    excluded: bool,
}

/// Read an item's pre-write signature and derived columns from the index.
fn read_snapshot(conn: &Connection, id: &str) -> Result<(Signature, DerivedCols), IndexError> {
    let (topo_rank, has_dangling, excluded, parent): (i64, bool, bool, Option<String>) = conn
        .query_row(
            "SELECT topological_rank, has_dangling_deps, excluded, parent_id \
             FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let mut edges = std::collections::BTreeSet::new();
    {
        let mut stmt = conn.prepare_cached("SELECT to_id, kind FROM edges WHERE from_id = ?1")?;
        let rows = stmt.query_map([id], |r| Ok((r.get::<_, u8>(1)?, r.get::<_, String>(0)?)))?;
        for row in rows {
            let (kind, to) = row?;
            edges.insert((kind, to));
        }
    }
    Ok((
        Signature { edges, parent },
        DerivedCols {
            topo_rank,
            has_dangling,
            excluded,
        },
    ))
}

/// The structure signature of a freshly parsed item.
fn signature_of(fm: &clove_types::ItemFrontmatter) -> Signature {
    use clove_core::graph::EdgeKind;
    let mut edges = std::collections::BTreeSet::new();
    let mut add = |kind: EdgeKind, ids: &[CloveId]| {
        for to in ids {
            edges.insert((kind as u8, to.as_str().to_owned()));
        }
    };
    add(EdgeKind::DependsOn, &fm.deps);
    add(EdgeKind::Relates, &fm.relates);
    add(EdgeKind::Duplicates, &fm.duplicates);
    add(EdgeKind::Supersedes, &fm.supersedes);
    Signature {
        edges,
        parent: fm.parent.as_ref().map(|p| p.as_str().to_owned()),
    }
}

/// Like [`apply_staleness`], but returns whether the dependency topology changed
/// (i.e. whether the global derived-column recompute ran). Tests use the flag to
/// assert the content-only fast path.
pub(crate) fn apply_staleness_tracked(
    conn: &mut Connection,
    report: &StalenessReport,
    issues_dir: &Utf8Path,
) -> Result<bool, IndexError> {
    if !report.needs_apply() {
        return Ok(false);
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Snapshot each changed-content item's signature + derived columns BEFORE any
    // write overwrites them.
    let mut snapshot: std::collections::HashMap<String, (Signature, DerivedCols)> =
        std::collections::HashMap::new();
    for id in &report.stale_ids {
        snapshot.insert(id.as_str().to_owned(), read_snapshot(&tx, id.as_str())?);
    }

    // Adding or removing an item always changes the graph.
    let mut topology_changed = !report.new_ids.is_empty() || !report.deleted_ids.is_empty();

    // New items: a node addition always triggers the recompute below, so the
    // derived placeholders here are fixed up before commit.
    for id in &report.new_ids {
        let path = issues_dir.join(format!("{id}.md"));
        let Some((mtime_ms, mtime_ns, bytes)) = stat_then_read(&path) else {
            continue; // file vanished mid-sweep
        };
        let Ok(item) = parse_item_bytes(&bytes, &path, id) else {
            continue; // malformed-skip
        };
        let meta = RowMeta {
            file_mtime_ms: mtime_ms,
            file_size: bytes.len() as i64,
            content_hash: content_hash8(&bytes),
            topo_rank: None,
            has_dangling_deps: false,
            excluded: false,
        };
        write_row(&tx, &item, &meta)?;
        upsert_file_mtime(&tx, id, mtime_ns, &bytes)?;
    }

    // Changed-content items: preserve their exact derived columns. If the topology
    // turns out unchanged this is the final, correct value (and we skip recompute);
    // if it changed, the recompute overwrites it.
    for id in &report.stale_ids {
        let path = issues_dir.join(format!("{id}.md"));
        let Some((mtime_ms, mtime_ns, bytes)) = stat_then_read(&path) else {
            continue;
        };
        let Ok(item) = parse_item_bytes(&bytes, &path, id) else {
            continue;
        };
        let (old_sig, old_derived) = snapshot.get(id.as_str()).expect("snapshotted above");
        if !topology_changed && *old_sig != signature_of(&item.frontmatter) {
            topology_changed = true;
        }
        let meta = RowMeta {
            file_mtime_ms: mtime_ms,
            file_size: bytes.len() as i64,
            content_hash: content_hash8(&bytes),
            topo_rank: Some(old_derived.topo_rank),
            has_dangling_deps: old_derived.has_dangling,
            excluded: old_derived.excluded,
        };
        write_row(&tx, &item, &meta)?;
        upsert_file_mtime(&tx, id, mtime_ns, &bytes)?;
    }

    // Touched files: content unchanged, so only the stat columns move. The
    // check-time values are stored, not a fresh stat: a file rewritten since the
    // check then still differs from the stored values and is hashed next time.
    for touched in &report.touched {
        tx.execute(
            "UPDATE items SET file_mtime = ?1, file_size = ?2 WHERE id = ?3",
            params![touched.mtime_ms, touched.size, touched.id.as_str()],
        )?;
        tx.execute(
            "UPDATE file_mtimes SET mtime_ns = ?1 WHERE path = ?2",
            params![touched.mtime_ns, format!("{}.md", touched.id)],
        )?;
    }

    for id in &report.deleted_ids {
        delete_row(&tx, id.as_str())?;
        tx.execute(
            "DELETE FROM file_mtimes WHERE path = ?1",
            params![format!("{id}.md")],
        )?;
    }

    // Recompute the derived graph columns exactly — but only when this batch
    // changed the dependency structure. A content-only edit keeps the preserved
    // values and avoids the O(V+E) pass (M4 topology-change guard).
    if topology_changed {
        crate::derive::recompute_derived(&tx)?;
    }

    // Refresh the `meta` oracle to the check-time directory snapshot. Without
    // this an incremental apply left the last-reindex values in place, so the
    // O(readdir) clean fast path (`check_staleness_fast`) never matched again
    // and every later read paid the full per-file stat + table-scan pass until
    // the next full reindex. Using the *observed* (check-time) snapshot rather
    // than re-reading the directory now is what makes a file added between
    // check and apply safe: the stored mtime stays behind the directory's, the
    // fast path misses, and the next deep check picks the file up.
    if let Some((dir_mtime, file_count)) = report.observed {
        tx.execute(
            "INSERT OR REPLACE INTO meta (id, dir_mtime, file_count, last_git_head) \
             VALUES (1, ?1, ?2, (SELECT last_git_head FROM meta WHERE id = 1))",
            params![dir_mtime, file_count],
        )?;
    }

    tx.commit()?;
    Ok(topology_changed)
}

/// Upsert an item's `file_mtimes` row (preserving `synced_at`): the incremental
/// path must maintain this table just like a full reindex, or the git-sync
/// `synced_at` observability write becomes a silent no-op for any item first
/// seen after the last full reindex.
fn upsert_file_mtime(
    tx: &rusqlite::Transaction<'_>,
    id: &CloveId,
    mtime_ns: i64,
    bytes: &[u8],
) -> Result<(), IndexError> {
    tx.execute(
        "INSERT INTO file_mtimes (path, mtime_ns, content_hash) VALUES (?1, ?2, ?3) \
         ON CONFLICT(path) DO UPDATE SET \
           mtime_ns = excluded.mtime_ns, content_hash = excluded.content_hash",
        params![format!("{id}.md"), mtime_ns, &content_hash8(bytes)[..]],
    )?;
    Ok(())
}

/// Remove an item and its edges/labels.
fn delete_row(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), IndexError> {
    tx.execute("DELETE FROM items WHERE id = ?1", params![id])?;
    tx.execute("DELETE FROM edges WHERE from_id = ?1", params![id])?;
    tx.execute("DELETE FROM labels WHERE item_id = ?1", params![id])?;
    Ok(())
}

/// Stat an item file and **then** read it, returning `(mtime_ms, mtime_ns,
/// bytes)`, or `None` when the file is gone (it vanished mid-sweep; the next
/// check reports it deleted).
///
/// The order matters. The stored mtime must not be newer than the stored
/// content: if the file is rewritten between the two calls, stat-first stores
/// the *old* mtime with the new content, so the next check sees the mtime move,
/// hashes the file, and settles. Read-first stored the new mtime with the old
/// content, which no later check looks at again.
fn stat_then_read(path: &Utf8Path) -> Option<(i64, i64, Vec<u8>)> {
    let meta = std::fs::metadata(path).ok()?;
    let bytes = std::fs::read(path).ok()?;
    Some((mtime_to_ms(&meta), mtime_to_ns(&meta), bytes))
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
    thread_local! {
        /// Runs once, right after `check_staleness` has scanned the directory,
        /// so a test can change a file at the exact point a racing writer would.
        static AFTER_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn after_scan_hook() {
        if let Some(hook) = AFTER_SCAN.with(|h| h.borrow_mut().take()) {
            hook();
        }
    }

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

    /// An edit that lands while a staleness check is running must not be
    /// recorded as seen.
    ///
    /// The check used to read the directory mtime *after* it had stat-ed the
    /// files, and store it as the fast path's oracle. A file renamed into place
    /// in between was then missing from the report while the stored mtime
    /// already covered it, so every later fast check called the index clean and
    /// the old row was served until something else touched the directory.
    #[test]
    fn an_edit_during_the_scan_is_caught_by_the_next_fast_check() {
        let fx = fixture(5);
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();
        let mut index = Index::open(&fx.db).unwrap();

        // The racing writer: replace item 0 by rename, as every clove write
        // does. The mtimes are set to a fixed past instant so neither the file
        // nor the directory is inside the 2 s "recent" window, which would
        // hide the bug by forcing the per-file pass.
        let id = id_for(0);
        let issues = fx.issues.clone();
        let edited = id.clone();
        AFTER_SCAN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let path = issues.join(format!("{edited}.md"));
                let tmp = issues.join(".tmp-racing-write");
                std::fs::write(&tmp, item_md(&edited, "Edited during the scan", "new")).unwrap();
                std::fs::rename(&tmp, &path).unwrap();
                let later = filetime::FileTime::from_unix_time(1_600_000_100, 0);
                filetime::set_file_mtime(&path, later).unwrap();
                filetime::set_file_mtime(issues.as_std_path(), later).unwrap();
            }));
        });

        let report = index.check_staleness(&fx.issues).unwrap();
        assert!(
            report.is_clean(),
            "the scan ran before the edit, so it cannot contain it: {report:?}"
        );
        assert_eq!(
            report.observed.map(|(mtime, _)| mtime),
            Some(1_600_000_000_000),
            "the oracle must be the directory mtime from before the scan"
        );
        index.apply_staleness(&report, &fx.issues).unwrap();

        let next = index.check_staleness_fast(&fx.issues).unwrap();
        assert_eq!(
            next.stale_ids,
            vec![CloveId::new(&id).unwrap()],
            "the fast path must not call the index clean"
        );
    }

    /// A file that disappears between the report and the apply is skipped, and
    /// the rest of the batch still lands.
    #[test]
    fn apply_skips_a_file_that_vanished_after_the_check() {
        let fx = fixture(3);
        reindex(&fx.issues, &fx.db).unwrap();
        let mut index = Index::open(&fx.db).unwrap();
        for i in [0, 1] {
            let id = id_for(i);
            std::fs::write(
                fx.issues.join(format!("{id}.md")),
                item_md(&id, "Changed", "changed body"),
            )
            .unwrap();
        }
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(report.stale_ids.len(), 2);
        std::fs::remove_file(fx.issues.join(format!("{}.md", id_for(0)))).unwrap();

        index.apply_staleness(&report, &fx.issues).unwrap();
        let title: String = index
            .conn()
            .query_row("SELECT title FROM items WHERE id = ?1", [id_for(1)], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Changed");
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

    #[test]
    fn deep_catches_old_inplace_edit_with_unchanged_directory() {
        // An in-place (truncate-and-rewrite) edit older than the recent-mtime
        // window, with the directory mtime and file count still matching `meta`.
        // The fast path trusts the directory and misses it (its documented
        // tradeoff); the deep path must not take that shortcut.
        let fx = fixture(5);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap(); // meta + file mtimes = backdated

        let id = id_for(2);
        let path = fx.issues.join(format!("{id}.md"));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            f.write_all(item_md(&id, "Item 2", "MUTATED body").as_bytes())
                .unwrap();
        }
        // The edit is old (outside the recent window) but newer than the
        // indexed mtime; the directory looks untouched.
        let edited = filetime::FileTime::from_unix_time(1_600_000_100, 0);
        filetime::set_file_mtime(&path, edited).unwrap();
        backdate_dir(&fx.issues);

        let mut index = Index::open(&fx.db).unwrap();
        assert!(
            index.check_staleness_fast(&fx.issues).unwrap().is_clean(),
            "fast path trusts the unchanged directory"
        );
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(
            report.stale_ids,
            vec![CloveId::new(&id).unwrap()],
            "{report:?}"
        );
        assert!(report.new_ids.is_empty() && report.deleted_ids.is_empty());

        // After the apply, the deep path is clean again.
        index.apply_staleness(&report, &fx.issues).unwrap();
        assert!(index.check_staleness(&fx.issues).unwrap().is_clean());
    }

    #[test]
    fn deep_catches_same_mtime_rewrite_when_the_size_changed() {
        // A rewrite that restores the indexed mtime exactly (`touch -r` from a
        // saved copy). The mtime matches, so only the stored size reveals it.
        let fx = fixture(3);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();

        let id = id_for(1);
        let path = fx.issues.join(format!("{id}.md"));
        let indexed = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        std::fs::write(&path, item_md(&id, "Item 1", "a longer MUTATED body")).unwrap();
        filetime::set_file_mtime(&path, indexed).unwrap();
        backdate_dir(&fx.issues);

        let index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(
            report.stale_ids,
            vec![CloveId::new(&id).unwrap()],
            "{report:?}"
        );
    }

    #[test]
    fn touched_files_are_recorded_so_the_next_check_does_not_hash_them() {
        // Every file's mtime moves with no content change (a `touch`, a git
        // checkout round-trip). The report is clean but needs an apply; after
        // it, the stored mtimes match and the `meta` row is current.
        let fx = fixture(4);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();

        let later = filetime::FileTime::from_unix_time(1_600_000_500, 0);
        for entry in std::fs::read_dir(&fx.issues).unwrap() {
            filetime::set_file_mtime(entry.unwrap().path(), later).unwrap();
        }
        filetime::set_file_mtime(fx.issues.as_std_path(), later).unwrap();

        let mut index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(report.touched.len(), 4, "{report:?}");
        assert!(report.meta_outdated, "the directory mtime moved");
        assert!(report.needs_apply());

        let recomputed = apply_staleness_tracked(index.conn_mut(), &report, &fx.issues).unwrap();
        assert!(!recomputed, "a touch changes no topology");

        let after = index.check_staleness(&fx.issues).unwrap();
        assert!(after.touched.is_empty(), "{after:?}");
        assert!(!after.meta_outdated, "{after:?}");
        assert!(!after.needs_apply(), "{after:?}");
        // And the fast path's oracle matches again: O(readdir) clean.
        assert_eq!(
            index.check_staleness_fast(&fx.issues).unwrap(),
            StalenessReport::default()
        );
    }

    #[test]
    fn a_clean_directory_bump_refreshes_meta() {
        // A temp file created and removed bumps the directory mtime only. The
        // fast path falls through to the per-file pass; applying the clean
        // report must move `meta` forward so the next fast check is O(readdir).
        let fx = fixture(2);
        reindex(&fx.issues, &fx.db).unwrap();
        backdate(&fx.issues);
        reindex(&fx.issues, &fx.db).unwrap();
        filetime::set_file_mtime(
            fx.issues.as_std_path(),
            filetime::FileTime::from_unix_time(1_600_000_900, 0),
        )
        .unwrap();

        let mut index = Index::open(&fx.db).unwrap();
        let report = index.check_staleness_fast(&fx.issues).unwrap();
        assert!(report.is_clean() && report.touched.is_empty(), "{report:?}");
        assert!(report.meta_outdated && report.needs_apply(), "{report:?}");
        index.apply_staleness(&report, &fx.issues).unwrap();

        let next = index.check_staleness_fast(&fx.issues).unwrap();
        assert_eq!(next.observed, None, "fast path short-circuits: {next:?}");
    }

    // ---- topology-change guard (M4) ----------------------------------------

    /// Write an item with explicit status + hard deps.
    fn item_with(id: &str, status: &str, deps: &[&str]) -> String {
        let mut s = format!(
            "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: {status}\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n"
        );
        if status == "closed" {
            s.push_str("closed: 2026-06-02T11:00:00Z\n");
        }
        if !deps.is_empty() {
            s.push_str("deps:\n");
            for d in deps {
                s.push_str(&format!("  - {d}\n"));
            }
        }
        s.push_str("---\nbody\n");
        s
    }

    fn derived(index: &Index) -> Vec<(String, i64, bool, bool)> {
        let mut stmt = index
            .conn()
            .prepare(
                "SELECT id, topological_rank, has_dangling_deps, excluded \
                 FROM items ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Fresh reindex of the same files → the "gold" derived state.
    fn gold(issues: &Utf8Path) -> Vec<(String, i64, bool, bool)> {
        let dir = tempfile::tempdir().unwrap();
        let db = Utf8PathBuf::from_path_buf(dir.path().join("gold.db")).unwrap();
        reindex(issues, &db).unwrap();
        derived(&Index::open(&db).unwrap())
    }

    #[test]
    fn status_only_edit_skips_recompute_and_stays_exact() {
        let fx = fixture(0);
        std::fs::write(
            fx.issues.join("proj-AAAAAAAA.md"),
            item_with("proj-AAAAAAAA", "open", &[]),
        )
        .unwrap();
        std::fs::write(
            fx.issues.join("proj-BBBBBBBB.md"),
            item_with("proj-BBBBBBBB", "open", &["proj-AAAAAAAA"]),
        )
        .unwrap();
        reindex(&fx.issues, &fx.db).unwrap();

        let mut index = Index::open(&fx.db).unwrap();
        let before = derived(&index);

        // Content-only edit: flip B's status, deps unchanged.
        std::fs::write(
            fx.issues.join("proj-BBBBBBBB.md"),
            item_with("proj-BBBBBBBB", "in_progress", &["proj-AAAAAAAA"]),
        )
        .unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        assert_eq!(report.stale_ids.len(), 1);

        let recomputed = apply_staleness_tracked(index.conn_mut(), &report, &fx.issues).unwrap();
        assert!(
            !recomputed,
            "a status-only edit must skip the derived recompute"
        );

        // Derived columns are unchanged and still exactly match a fresh reindex.
        assert_eq!(derived(&index), before);
        assert_eq!(derived(&index), gold(&fx.issues));
    }

    #[test]
    fn dep_edit_triggers_recompute_and_matches_reindex() {
        let fx = fixture(0);
        std::fs::write(
            fx.issues.join("proj-AAAAAAAA.md"),
            item_with("proj-AAAAAAAA", "open", &[]),
        )
        .unwrap();
        std::fs::write(
            fx.issues.join("proj-BBBBBBBB.md"),
            item_with("proj-BBBBBBBB", "open", &["proj-AAAAAAAA"]),
        )
        .unwrap();
        reindex(&fx.issues, &fx.db).unwrap();
        let mut index = Index::open(&fx.db).unwrap();

        // Structural edit: drop B's dependency on A.
        std::fs::write(
            fx.issues.join("proj-BBBBBBBB.md"),
            item_with("proj-BBBBBBBB", "open", &[]),
        )
        .unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        let recomputed = apply_staleness_tracked(index.conn_mut(), &report, &fx.issues).unwrap();
        assert!(recomputed, "a dependency edit must trigger the recompute");
        assert_eq!(derived(&index), gold(&fx.issues));
    }

    #[test]
    fn new_item_triggers_recompute() {
        let fx = fixture(0);
        std::fs::write(
            fx.issues.join("proj-AAAAAAAA.md"),
            item_with("proj-AAAAAAAA", "open", &[]),
        )
        .unwrap();
        reindex(&fx.issues, &fx.db).unwrap();
        let mut index = Index::open(&fx.db).unwrap();

        std::fs::write(
            fx.issues.join("proj-BBBBBBBB.md"),
            item_with("proj-BBBBBBBB", "open", &["proj-AAAAAAAA"]),
        )
        .unwrap();
        let report = index.check_staleness(&fx.issues).unwrap();
        let recomputed = apply_staleness_tracked(index.conn_mut(), &report, &fx.issues).unwrap();
        assert!(recomputed, "adding an item must trigger the recompute");
        assert_eq!(derived(&index), gold(&fx.issues));
    }
}
