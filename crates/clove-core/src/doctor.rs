//! Store health check (`clove doctor`, DESIGN §7.7 / T-CLI18).
//!
//! [`diagnose`] loads every item once, builds the dependency graph, and runs the
//! check suite, returning a [`DoctorReport`]. [`fix`] applies only the *safe*
//! repairs — label canonicalization, list sort/dedup, and orphaned comment-dir
//! removal — never structural changes (dangling refs, cycles, bad parents).

use std::collections::HashSet;

use crate::config::load_config;
use crate::error::CloveError;
use crate::graph::GraphStore;
use crate::model::{normalize_label, ItemFrontmatter};
use crate::store::ItemStore;
use crate::validate::validate_item;
use crate::write::write_item_file;

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

    // (4) per-item field validation.
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
    for orphan in orphan_comment_dirs(store, &items) {
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

    report
}

/// Apply the safe repairs (checks 8, 9, 10). Returns the number of fixes made.
pub fn fix(store: &ItemStore) -> Result<usize, CloveError> {
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

    for orphan in orphan_comment_dirs(store, &items) {
        let dir = store.issues_dir().join(&orphan);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|source| CloveError::Io {
                path: dir.clone(),
                source,
            })?;
            fixed += 1;
        }
    }

    Ok(fixed)
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
fn orphan_comment_dirs(store: &ItemStore, items: &[crate::model::Item]) -> Vec<String> {
    let existing: HashSet<String> = items.iter().map(|i| i.frontmatter.id.to_string()).collect();
    let mut orphans = Vec::new();
    let Ok(entries) = std::fs::read_dir(store.issues_dir()) else {
        return orphans;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !existing.contains(&name) {
            orphans.push(name);
        }
    }
    orphans.sort();
    orphans
}
