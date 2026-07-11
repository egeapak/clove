//! Store health check (`clove doctor`, DESIGN §7.7 / T-CLI18).
//!
//! [`diagnose`] loads every item once, builds the dependency graph, and runs the
//! check suite, returning a [`DoctorReport`]. [`fix`] applies only the *safe*
//! repairs — label canonicalization, list sort/dedup, and orphaned comment-dir
//! removal — never structural changes (dangling refs, cycles, bad parents).

use std::collections::HashMap;
use std::collections::HashSet;

use camino::Utf8PathBuf;
use chrono::{DateTime, Duration, Utc};

use crate::config::{load_config, GITIGNORE_ENTRIES};
use crate::error::CloveError;
use crate::graph::GraphStore;
use crate::model::{normalize_label, ItemFrontmatter};
use crate::store::ItemStore;
use crate::validate::validate_item;
use crate::write::write_item_file;

/// How far a timestamp may run ahead of "now" before it is flagged as future-
/// dated. Generous, to absorb clock skew between machines that share a repo.
const FUTURE_SKEW: Duration = Duration::hours(24);

/// The severity of a doctor finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A single finding from the check suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorIssue {
    pub severity: Severity,
    pub code: &'static str,
    pub item: Option<String>,
    pub message: String,
    pub fixable: bool,
}

/// The result of a [`diagnose`] run.
#[derive(Debug, Clone, Default)]
pub struct DoctorReport {
    pub issues: Vec<DoctorIssue>,
    /// Number of item files examined.
    pub checked: usize,
}

impl DoctorReport {
    pub fn errors(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .count()
    }

    fn push(
        &mut self,
        severity: Severity,
        code: &'static str,
        item: Option<String>,
        message: String,
        fixable: bool,
    ) {
        self.issues.push(DoctorIssue {
            severity,
            code,
            item,
            message,
            fixable,
        });
    }
}

/// Run the full check suite over `store`.
pub fn diagnose(store: &ItemStore) -> DoctorReport {
    let mut report = DoctorReport::default();

    // (1) parse failures (includes id/filename mismatch).
    let (items, scan_errors) = match store.scan() {
        Ok(pair) => pair,
        Err(e) => {
            report.push(Severity::Error, "IO_ERROR", None, e.to_string(), false);
            return report;
        }
    };
    report.checked = items.len() + scan_errors.len();
    for err in &scan_errors {
        let code = match err {
            crate::store::ScanError::ParseFailed { source, .. } => {
                if matches!(source, CloveError::IdMismatch { .. }) {
                    "ID_MISMATCH"
                } else {
                    "PARSE_ERROR"
                }
            }
        };
        report.push(
            Severity::Error,
            code,
            Some(err.path().to_string()),
            err.to_string(),
            false,
        );
    }

    // (3) duplicate ids across files (DESIGN §7.7 #3). On a case-sensitive
    // filesystem the id/filename-stem check makes exact duplicates unreachable,
    // but a case-insensitive volume (macOS/Windows) can surface two files that
    // parse to the same id, which would silently shadow one another.
    let now = Utc::now();
    for dup in duplicate_ids(items.iter().map(|i| i.frontmatter.id.to_string())) {
        report.push(
            Severity::Error,
            "DUPLICATE_ID",
            Some(dup.clone()),
            format!("id `{dup}` is used by more than one item file"),
            false,
        );
    }

    // (4) per-item field validation + (4b) timestamp coherence.
    for item in &items {
        for v in validate_item(&item.frontmatter) {
            report.push(
                Severity::Error,
                validation_code(&v),
                Some(item.frontmatter.id.to_string()),
                v.to_string(),
                false,
            );
        }
        if let Some(reason) = timestamp_incoherence(&item.frontmatter, now) {
            report.push(
                Severity::Warning,
                "TIMESTAMP_INCOHERENT",
                Some(item.frontmatter.id.to_string()),
                reason,
                false,
            );
        }
    }

    // Build the graph once for structural checks (5,6,7).
    let frontmatters: Vec<ItemFrontmatter> = items.iter().map(|i| i.frontmatter.clone()).collect();
    let (graph, dangling) = GraphStore::build(&frontmatters);

    // (5) dangling references.
    for d in &dangling {
        report.push(
            Severity::Error,
            "DANGLING_REF",
            Some(d.from.to_string()),
            format!("references missing item `{}`", d.to),
            false,
        );
    }

    // (6) hard-dependency cycles.
    for cycle in graph.all_cycles() {
        let ids: Vec<String> = cycle.iter().map(|c| c.to_string()).collect();
        report.push(
            Severity::Error,
            "CYCLE_DETECTED",
            None,
            format!("dependency cycle: {}", ids.join(" → ")),
            false,
        );
    }

    // (7) invalid parent (self / cyclic), as flagged by the graph.
    for item in &items {
        if graph
            .meta(&item.frontmatter.id)
            .map(|m| m.malformed_parent)
            .unwrap_or(false)
        {
            report.push(
                Severity::Error,
                "INVALID_PARENT",
                Some(item.frontmatter.id.to_string()),
                "item has a self- or cyclic parent reference".to_owned(),
                false,
            );
        }
    }

    // (8,9) label canonicalization + list order/dedup (both fixable warnings).
    for item in &items {
        let fm = &item.frontmatter;
        if !labels_canonical(fm) {
            report.push(
                Severity::Warning,
                "NONCANONICAL_LABELS",
                Some(fm.id.to_string()),
                "labels are not canonical (case/whitespace) or contain duplicates".to_owned(),
                true,
            );
        }
        if !lists_tidy(fm) {
            report.push(
                Severity::Warning,
                "UNSORTED_LISTS",
                Some(fm.id.to_string()),
                "a dependency/relation list is unsorted or has duplicates".to_owned(),
                true,
            );
        }
    }

    // (10) orphaned comment directories (fixable warning).
    for orphan in orphan_comment_dirs(store) {
        report.push(
            Severity::Warning,
            "ORPHAN_COMMENTS",
            Some(orphan.clone()),
            format!("comment directory `{orphan}` has no matching item file"),
            true,
        );
    }

    // (11) config validity.
    if let Err(e) = load_config(store.repo_root()) {
        report.push(Severity::Error, "CONFIG_ERROR", None, e.to_string(), false);
    }

    // (12) `.clove/.gitignore` drift: the file must keep the rebuildable cache
    // and the daemon's socket/pid/lock files out of git (DESIGN §2.1 / §8.2). A
    // missing file or a missing required entry would let `index.db`/`daemon.*`
    // get committed — derived state leaking into the source of truth.
    if let Some(missing) = gitignore_missing_entries(store) {
        let detail = if missing.is_empty() {
            "`.clove/.gitignore` is missing".to_owned()
        } else {
            format!(
                "`.clove/.gitignore` is missing entr{}: {}",
                if missing.len() == 1 { "y" } else { "ies" },
                missing.join(", ")
            )
        };
        report.push(Severity::Warning, "GITIGNORE_DRIFT", None, detail, true);
    }

    report
}

/// Apply the safe repairs (checks 8, 9, 10). Returns the number of fixes made.
pub fn fix(store: &ItemStore) -> Result<usize, CloveError> {
    // One read-modify-write window under the store-wide write lock: without
    // it, re-writing an item from our earlier scan could silently revert a
    // concurrent edit made between the scan and the write (lost update).
    let mut lock = store.write_lock()?;
    let _guard = lock.lock()?;

    let mut fixed = 0;

    let (items, _scan_errors) = store.scan()?;
    for item in &items {
        let mut next = item.clone();
        let mut changed = false;

        // Canonicalize labels (best-effort: skip any that won't normalize).
        let mut labels: Vec<String> = next
            .frontmatter
            .labels
            .iter()
            .filter_map(|l| normalize_label(l).ok())
            .collect();
        labels.sort();
        labels.dedup();
        if labels != next.frontmatter.labels {
            next.frontmatter.labels = labels;
            changed = true;
        }

        for list in [
            &mut next.frontmatter.deps,
            &mut next.frontmatter.relates,
            &mut next.frontmatter.duplicates,
            &mut next.frontmatter.supersedes,
        ] {
            let before = list.clone();
            list.sort();
            list.dedup();
            if *list != before {
                changed = true;
            }
        }

        if changed {
            write_item_file(&next, &store.path_for(&next.frontmatter.id))?;
            fixed += 1;
        }
    }

    for orphan in orphan_comment_dirs(store) {
        let dir = store.issues_dir().join(&orphan);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|source| CloveError::Io {
                path: dir.clone(),
                source,
            })?;
            fixed += 1;
        }
    }

    // GITIGNORE_DRIFT: append any missing canonical entries, preserving whatever
    // else the user has added (we only guarantee the required set is present, we
    // never rewrite the whole file out from under them).
    if let Some(missing) = gitignore_missing_entries(store) {
        repair_gitignore(store, &missing)?;
        fixed += 1;
    }

    Ok(fixed)
}

/// Ids that appear more than once in `ids`, each reported once (sorted).
fn duplicate_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for id in ids {
        *counts.entry(id).or_insert(0) += 1;
    }
    let mut dups: Vec<String> = counts
        .into_iter()
        .filter_map(|(id, n)| (n > 1).then_some(id))
        .collect();
    dups.sort();
    dups
}

/// A human-readable reason when an item's timestamps are out of order or run
/// into the future, or `None` when they are coherent. Pure over an injected
/// `now` so it is deterministic in tests.
fn timestamp_incoherence(fm: &ItemFrontmatter, now: DateTime<Utc>) -> Option<String> {
    if fm.updated < fm.created {
        return Some(format!(
            "updated ({}) is before created ({})",
            fm.updated, fm.created
        ));
    }
    if let Some(closed) = fm.closed {
        if closed < fm.created {
            return Some(format!(
                "closed ({}) is before created ({})",
                closed, fm.created
            ));
        }
    }
    let horizon = now + FUTURE_SKEW;
    for (field, ts) in [
        ("created", Some(fm.created)),
        ("updated", Some(fm.updated)),
        ("closed", fm.closed),
    ] {
        if let Some(ts) = ts {
            if ts > horizon {
                return Some(format!("{field} timestamp ({ts}) is in the future"));
            }
        }
    }
    None
}

/// The `.clove/.gitignore` path (sibling of `issues/`).
fn gitignore_path(store: &ItemStore) -> Utf8PathBuf {
    let clove_dir = store
        .issues_dir()
        .parent()
        .unwrap_or_else(|| store.issues_dir());
    clove_dir.join(".gitignore")
}

/// `Some(missing)` when `.clove/.gitignore` is absent (empty vec) or is missing
/// some required entries (the missing ones, in canonical order); `None` when
/// every [`GITIGNORE_ENTRIES`] line is present. Extra user lines are ignored.
fn gitignore_missing_entries(store: &ItemStore) -> Option<Vec<String>> {
    let path = gitignore_path(store);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        // Absent (or unreadable) → the whole canonical set is "missing".
        return Some(Vec::new());
    };
    let present: HashSet<&str> = contents.lines().map(str::trim).collect();
    let missing: Vec<String> = GITIGNORE_ENTRIES
        .iter()
        .filter(|e| !present.contains(**e))
        .map(|e| (*e).to_owned())
        .collect();
    (!missing.is_empty()).then_some(missing)
}

/// Bring `.clove/.gitignore` back into compliance by appending the missing
/// canonical entries (creating the file if absent). Existing content is kept.
fn repair_gitignore(store: &ItemStore, missing: &[String]) -> Result<(), CloveError> {
    let path = gitignore_path(store);
    let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
    // The canonical full set when the file is absent / empty; otherwise just the
    // gaps, each on its own LF-terminated line after the existing content.
    let to_add: Vec<&str> = if contents.trim().is_empty() {
        GITIGNORE_ENTRIES.to_vec()
    } else {
        if !contents.ends_with('\n') {
            contents.push('\n');
        }
        missing.iter().map(String::as_str).collect()
    };
    for entry in to_add {
        contents.push_str(entry);
        contents.push('\n');
    }
    std::fs::write(&path, contents).map_err(|source| CloveError::Io { path, source })
}

fn validation_code(v: &crate::validate::ValidationError) -> &'static str {
    use crate::validate::ValidationError::*;
    match v {
        PriorityOutOfRange(_) => "INVALID_PRIORITY",
        ClosedWithoutTimestamp | ClosedTimestampOnNonClosed(_) => "CLOSED_INVARIANT",
        UnsupportedSchema { .. } => "UNSUPPORTED_SCHEMA",
        ListTooLong { .. } => "LIST_TOO_LONG",
    }
}

/// True when every label is already canonical and the set is duplicate-free.
fn labels_canonical(fm: &ItemFrontmatter) -> bool {
    let mut seen = HashSet::new();
    for label in &fm.labels {
        match normalize_label(label) {
            Ok(canon) if &canon == label => {
                if !seen.insert(canon) {
                    return false; // duplicate
                }
            }
            _ => return false, // non-canonical or un-normalizable
        }
    }
    is_sorted(&fm.labels)
}

/// True when every dependency/relation list is sorted and duplicate-free.
fn lists_tidy(fm: &ItemFrontmatter) -> bool {
    [&fm.deps, &fm.relates, &fm.duplicates, &fm.supersedes]
        .into_iter()
        .all(|list| {
            let mut sorted = list.clone();
            sorted.sort();
            sorted.dedup();
            sorted.len() == list.len() && &sorted == list
        })
}

fn is_sorted<T: Ord>(v: &[T]) -> bool {
    v.windows(2).all(|w| w[0] <= w[1])
}

/// Comment directory names (`<id>`) under `issues/` with no matching `<id>.md`.
///
/// The "existing item" set is built from the on-disk `<id>.md` file *stems*,
/// deliberately NOT from successfully parsed items: an item whose file merely
/// fails to parse (conflict markers, a bad hand-edit) still owns its comments,
/// and treating it as absent would let `fix` permanently DELETE real comment
/// history behind a repairable parse error.
fn orphan_comment_dirs(store: &ItemStore) -> Vec<String> {
    let mut item_stems: HashSet<String> = HashSet::new();
    let mut dirs: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(store.issues_dir()) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if file_type.is_dir() {
            dirs.push(name);
        } else if let Some(stem) = name.strip_suffix(".md") {
            item_stems.insert(stem.to_owned());
        }
    }
    let mut orphans: Vec<String> = dirs
        .into_iter()
        .filter(|name| !item_stems.contains(name))
        .collect();
    orphans.sort();
    orphans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemStatus, ItemType, Priority};
    use crate::CloveId;

    fn fm(created: &str, updated: &str, closed: Option<&str>) -> ItemFrontmatter {
        ItemFrontmatter {
            schema: 1,
            id: CloveId::new("proj-7AF3K2MN").unwrap(),
            title: "T".to_owned(),
            status: if closed.is_some() {
                ItemStatus::Closed
            } else {
                ItemStatus::Open
            },
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
            created: created.parse().unwrap(),
            updated: updated.parse().unwrap(),
            closed: closed.map(|c| c.parse().unwrap()),
            assignee: None,
            parent: None,
            labels: Vec::new(),
            deps: Vec::new(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
    }

    #[test]
    fn duplicate_ids_reports_only_repeats_sorted() {
        let ids = ["b", "a", "b", "c", "a", "a"].into_iter().map(String::from);
        assert_eq!(duplicate_ids(ids), vec!["a".to_owned(), "b".to_owned()]);
        let unique = ["x", "y", "z"].into_iter().map(String::from);
        assert!(duplicate_ids(unique).is_empty());
    }

    #[test]
    fn coherent_timestamps_pass() {
        let now: DateTime<Utc> = "2026-06-07T00:00:00Z".parse().unwrap();
        let item = fm("2026-06-01T10:00:00Z", "2026-06-02T10:00:00Z", None);
        assert!(timestamp_incoherence(&item, now).is_none());
        let closed_ok = fm(
            "2026-06-01T10:00:00Z",
            "2026-06-03T10:00:00Z",
            Some("2026-06-03T10:00:00Z"),
        );
        assert!(timestamp_incoherence(&closed_ok, now).is_none());
    }

    #[test]
    fn updated_before_created_flagged() {
        let now: DateTime<Utc> = "2026-06-07T00:00:00Z".parse().unwrap();
        let item = fm("2026-06-05T10:00:00Z", "2026-06-01T10:00:00Z", None);
        let reason = timestamp_incoherence(&item, now).unwrap();
        assert!(reason.contains("updated"), "{reason}");
    }

    #[test]
    fn closed_before_created_flagged() {
        let now: DateTime<Utc> = "2026-06-07T00:00:00Z".parse().unwrap();
        let item = fm(
            "2026-06-05T10:00:00Z",
            "2026-06-05T10:00:00Z",
            Some("2026-06-01T10:00:00Z"),
        );
        let reason = timestamp_incoherence(&item, now).unwrap();
        assert!(reason.contains("closed"), "{reason}");
    }

    #[test]
    fn future_dated_flagged_beyond_skew() {
        let now: DateTime<Utc> = "2026-06-07T00:00:00Z".parse().unwrap();
        // 48h ahead of `now` (beyond the 24h skew window).
        let item = fm("2026-06-09T10:00:00Z", "2026-06-09T10:00:00Z", None);
        let reason = timestamp_incoherence(&item, now).unwrap();
        assert!(reason.contains("future"), "{reason}");
        // Within the skew window: not flagged.
        let near = fm("2026-06-07T06:00:00Z", "2026-06-07T06:00:00Z", None);
        assert!(timestamp_incoherence(&near, now).is_none());
    }
}
