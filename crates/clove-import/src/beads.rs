//! Beads importer (T-M02): import a Beads `issues.jsonl` file (DESIGN.md §11.2).
//!
//! Beads stores one issue per line as a JSON object (NDJSON). Beads' field set
//! differs from clove's, so we deserialize into a dedicated, tolerant
//! [`BeadsIssue`] struct (no `deny_unknown_fields`) rather than clove's strict
//! [`ItemFrontmatter`], then map each field through the shared coercion helpers
//! in [`crate::map`].
//!
//! ## Field mapping (DESIGN §11.2)
//!
//! | beads | clove |
//! |---|---|
//! | `id` | idempotency key → `external_ref = "beads:<id>"`; clove mints a fresh `CloveId` |
//! | `title` | `title` |
//! | `description` | body |
//! | `status` (`deferred` → `open` **+ label `deferred`**) | `status` (via [`crate::map::coerce_status`]) |
//! | `priority` | `priority` (via [`crate::map::coerce_priority`]) |
//! | `issue_type` (`task` → `chore`) | `type` (via [`crate::map::beads_type`]) |
//! | `assignee` or `owner` | `assignee` (assignee wins) |
//! | `labels` | `labels` (normalized via [`crate::map::map_labels`]) |
//! | `dependencies[type=blocks]` | `deps` |
//! | `dependencies[type=parent-child]` | `parent` (first only) |
//! | `dependencies[type=related\|tracks\|…]` | `relates` |
//! | unmapped beads-internal fields | folded into `external_ref` as a `meta:<json>` blob |
//! | — | `source_system = "beads"` |
//!
//! ## external_ref / meta-blob rule
//!
//! The clove `external_ref` is the idempotency key (DESIGN §11.3: skip incoming
//! items whose `external_ref` matches an existing item). The encoding is:
//!
//! - If the incoming line already carries a clove-style `external_ref` (this is
//!   the case when re-importing clove's own `export jsonl` output), it is used
//!   **verbatim** as the key. This makes `export jsonl | import beads` idempotent.
//! - Otherwise the key is `"beads:<id>"`.
//! - If the issue has unmapped beads-internal fields, a compact, key-sorted JSON
//!   object of them is appended as `" meta:<json>"`, producing
//!   `"beads:<id> meta:{...}"`. The JSON keys are sorted (serde_json::Map under
//!   the `preserve_order` feature is *not* assumed) so the encoding is stable and
//!   re-import reproduces the identical string — idempotency holds even with a
//!   meta blob.
//!
//! ## Dual-shape tolerance (round-trip with clove's own export)
//!
//! [`BeadsIssue`] also reads clove's `export jsonl` output, which differs from
//! beads-native NDJSON: it uses `type` (not `issue_type`); flat `deps`/`relates`/
//! `duplicates`/`supersedes` id arrays + a `parent` scalar (not a structured
//! `dependencies[]`); and string `status`/`priority`. The struct accepts BOTH
//! shapes — `issue_type` OR `type`, structured `dependencies` OR the flat arrays,
//! and `priority` as int or string — so the JSONL round-trip is lossless on
//! mapped fields and idempotent on re-run.
//!
//! ## comment_count warning
//!
//! Beads' `issues.jsonl` carries `comment_count` but **not** comment bodies. Any
//! issue with `comment_count > 0` pushes a warning (surfaced to stderr by the CLI
//! and in the JSON envelope's `_meta.warnings`) naming the beads id and suggesting
//! `bd show --json <id>` — the import must not silently succeed with missing
//! comment data (DESIGN §11.2).
//!
//! ## Resilience: malformed lines, duplicate ids, dep caps
//!
//! A single malformed JSONL line never aborts the whole import: it is reported as
//! a `would_skip` with reason `"malformed_line:<n>"` and the remaining valid lines
//! still import (M2). Two lines sharing a source id do not collapse onto one
//! record — `apply` consumes staged issues POSITIONALLY and the later duplicate is
//! a `would_skip` with reason `"duplicate_id"` (C1). Over-long `deps`/`relates`
//! arrays are truncated to [`clove_types::limits::MAX_DEP_ARRAY_LEN`] with a
//! warning, and dependency targets absent from the store are reported as dangling
//! (M4, report-only).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use camino::Utf8Path;
use chrono::{DateTime, Utc};
use clove_core::write::write_item_file;
use clove_core::ItemStore;
use clove_types::id::new_id;
use clove_types::model::CURRENT_SCHEMA_VERSION;
use clove_types::{CloveId, Item, ItemFrontmatter, ItemStatus, ItemType, Priority};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ImportError;
use crate::map::{
    beads_type, cap_dep_array, coerce_priority, coerce_status, dangling_targets, map_labels,
    MAX_IMPORT_UNIT_BYTES,
};
use crate::plan::{ImportPlan, ImportReport, PlanItem, SkipItem};
use crate::{ImportCtx, Importer};

/// The set of top-level keys [`BeadsIssue`] maps explicitly. Any other key on an
/// incoming line is "unmapped beads-internal" and is folded into the meta blob.
const MAPPED_KEYS: &[&str] = &[
    // beads-native
    "id",
    "title",
    "description",
    "status",
    "priority",
    "issue_type",
    "assignee",
    "owner",
    "labels",
    "dependencies",
    "external_ref",
    "comment_count",
    // clove-export shape (also consumed directly, never re-stashed as meta)
    "type",
    "body",
    "deps",
    "relates",
    "duplicates",
    "supersedes",
    "parent",
    "source_system",
    // computed clove-export fields that carry no import-relevant data
    "ready",
    "blocked_by",
    "dangling_deps",
    "created",
    "updated",
    "closed",
];

/// A single structured beads dependency edge (`{ "id": "...", "type": "..." }`).
#[derive(Debug, Clone, Deserialize)]
struct BeadsDependency {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    dep_type: Option<String>,
}

/// Tolerant priority that accepts either a JSON number or a string (clove's
/// export emits a number, but hand-written/foreign JSONL may stringify it).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PriorityField {
    Int(i64),
    Str(String),
}

impl PriorityField {
    fn to_priority(&self) -> Priority {
        match self {
            PriorityField::Int(n) => coerce_priority(*n),
            PriorityField::Str(s) => match s.trim().parse::<i64>() {
                Ok(n) => coerce_priority(n),
                Err(_) => Priority::DEFAULT,
            },
        }
    }
}

/// The tolerant deserialization view of a single beads issue line.
///
/// Unknown fields are ignored (no `deny_unknown_fields`). Every field is
/// optional. Accepts both the beads-native shape and clove's own `export jsonl`
/// shape (see module docs).
#[derive(Debug, Clone, Default, Deserialize)]
struct BeadsIssue {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    /// beads body field.
    #[serde(default)]
    description: Option<String>,
    /// clove-export body field (used when `description` is absent).
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<PriorityField>,
    /// beads type field (`task` → `chore`).
    #[serde(default)]
    issue_type: Option<String>,
    /// clove-export type field (used when `issue_type` is absent).
    #[serde(default, rename = "type")]
    type_field: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    /// beads structured dependency edges.
    #[serde(default)]
    dependencies: Vec<BeadsDependency>,
    /// clove-export flat id arrays / scalar.
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    relates: Vec<String>,
    #[serde(default)]
    duplicates: Vec<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    parent: Option<String>,
    /// A pre-existing clove external_ref (round-trip idempotency key).
    #[serde(default)]
    external_ref: Option<String>,
}

/// A fully mapped issue, staged during [`BeadsImporter::plan`] and consumed by
/// [`BeadsImporter::apply`].
#[derive(Debug, Clone)]
struct StagedIssue {
    /// The idempotency key / clove `external_ref`.
    external_ref: String,
    /// The source id surfaced in the plan (`PlanItem.id` / `SkipItem.id`).
    source_id: String,
    title: String,
    status: ItemStatus,
    item_type: ItemType,
    priority: Priority,
    assignee: Option<String>,
    parent: Option<CloveId>,
    deps: Vec<CloveId>,
    relates: Vec<CloveId>,
    duplicates: Vec<CloveId>,
    supersedes: Vec<CloveId>,
    labels: Vec<String>,
    body: String,
}

/// The Beads importer.
///
/// Constructed with the id `prefix` (so [`apply`](BeadsImporter::apply) can mint
/// fresh [`CloveId`]s) and a clock for the `created`/`updated` stamps.
#[derive(Debug)]
pub struct BeadsImporter {
    prefix: String,
    now: DateTime<Utc>,
    staged: RefCell<Vec<StagedIssue>>,
    warnings: RefCell<Vec<String>>,
}

impl BeadsImporter {
    /// Build an importer that mints ids under `prefix` and stamps timestamps at
    /// `now`.
    pub fn new(prefix: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            prefix: prefix.into(),
            now,
            staged: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
        }
    }

    /// Drain the warnings collected during [`plan`](BeadsImporter::plan) (e.g. an
    /// issue with `comment_count > 0`). The CLI prints these to stderr.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }
}

impl Importer for BeadsImporter {
    fn plan(&self, src: &Utf8Path, ctx: &ImportCtx) -> Result<ImportPlan, ImportError> {
        let bytes = std::fs::read(src).map_err(|source| ImportError::Source {
            path: src.to_owned(),
            message: source.to_string(),
        })?;
        let text = String::from_utf8_lossy(&bytes);

        let mut plan = ImportPlan::new();
        let mut staged = self.staged.borrow_mut();
        staged.clear();

        // Source ids already seen in this run, to detect intra-source duplicates
        // (C1): the first occurrence is staged; later ones are skipped, never
        // silently overwriting the first staged record's data.
        let mut seen_ids: HashSet<String> = HashSet::new();

        for (lineno, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            // M5: cap the per-line byte size before parsing.
            if line.len() > MAX_IMPORT_UNIT_BYTES {
                return Err(ImportError::Record {
                    message: format!(
                        "{src}:{}: line is {} bytes, exceeding the import limit of {MAX_IMPORT_UNIT_BYTES}",
                        lineno + 1,
                        line.len()
                    ),
                });
            }

            // M2: a malformed line must not abort the whole import. Skip-and-report
            // it (with its 1-based line number) and continue with the valid lines.
            let mut staged_issue = match map_line(line) {
                Ok(issue) => issue,
                Err(_) => {
                    plan.would_skip.push(SkipItem {
                        id: format!("line {}", lineno + 1),
                        reason: format!("malformed_line:{}", lineno + 1),
                    });
                    continue;
                }
            };

            // comment_count > 0: comment bodies are not present in the JSONL, so
            // warn (must not silently succeed) — DESIGN §11.2.
            if let Some(count) = comment_count_of(line) {
                if count > 0 {
                    self.warnings.borrow_mut().push(format!(
                        "issue `{}` has {count} comment(s) not present in JSONL; run `bd show --json {}` to extract comment bodies",
                        staged_issue.source_id, staged_issue.source_id
                    ));
                }
            }

            // M4: enforce the per-array dep cap (truncate + warn).
            let source_id = staged_issue.source_id.clone();
            staged_issue.deps = cap_dep_array(
                std::mem::take(&mut staged_issue.deps),
                "deps",
                &source_id,
                &mut self.warnings.borrow_mut(),
            );
            staged_issue.relates = cap_dep_array(
                std::mem::take(&mut staged_issue.relates),
                "relates",
                &source_id,
                &mut self.warnings.borrow_mut(),
            );
            staged_issue.duplicates = cap_dep_array(
                std::mem::take(&mut staged_issue.duplicates),
                "duplicates",
                &source_id,
                &mut self.warnings.borrow_mut(),
            );
            staged_issue.supersedes = cap_dep_array(
                std::mem::take(&mut staged_issue.supersedes),
                "supersedes",
                &source_id,
                &mut self.warnings.borrow_mut(),
            );

            // M4: flag dangling dependency targets (ids absent from the store).
            let dangling = dangling_targets(
                &ctx.store_ids,
                staged_issue
                    .parent
                    .iter()
                    .chain(staged_issue.deps.iter())
                    .chain(staged_issue.relates.iter())
                    .chain(staged_issue.duplicates.iter())
                    .chain(staged_issue.supersedes.iter()),
            );
            if !dangling.is_empty() {
                let list = dangling
                    .iter()
                    .map(CloveId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                self.warnings.borrow_mut().push(format!(
                    "item `{source_id}`: dangling dependency target(s) not present in the store: {list}"
                ));
            }

            // C1: skip a second line that shares an already-seen source id.
            if !seen_ids.insert(staged_issue.source_id.clone()) {
                plan.would_skip.push(SkipItem {
                    id: staged_issue.source_id.clone(),
                    reason: "duplicate_id".to_owned(),
                });
                continue;
            }

            if ctx.is_imported(&staged_issue.external_ref) {
                // M3: report field-level divergences against the existing item.
                let conflicts = ctx.conflicts_for(
                    &staged_issue.external_ref,
                    &staged_issue.source_id,
                    staged_issue.status,
                    staged_issue.priority,
                    &staged_issue.title,
                );
                plan.conflicts.extend(conflicts);
                plan.would_skip.push(SkipItem {
                    id: staged_issue.source_id.clone(),
                    reason: "already_imported".to_owned(),
                });
            } else {
                plan.would_create.push(PlanItem {
                    id: staged_issue.source_id.clone(),
                    title: staged_issue.title.clone(),
                });
                staged.push(staged_issue);
            }
        }

        Ok(plan)
    }

    fn apply(&self, plan: ImportPlan, store: &ItemStore) -> Result<ImportReport, ImportError> {
        let staged = self.staged.borrow();
        let mut created = 0usize;

        // C1: pair plan ↔ staged POSITIONALLY by iterating the staged Vec
        // directly. `plan` (the create set) and `staged` are pushed together in
        // lock-step during `plan`, so each staged record is written exactly once
        // with its own data.
        for issue in staged.iter() {
            let id = new_id(&self.prefix, store.issues_dir())?;
            let closed = if issue.status == ItemStatus::Closed {
                Some(self.now)
            } else {
                None
            };
            let frontmatter = ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: id.clone(),
                title: issue.title.clone(),
                status: issue.status,
                item_type: issue.item_type,
                priority: issue.priority,
                created: self.now,
                updated: self.now,
                closed,
                assignee: issue.assignee.clone(),
                parent: issue.parent.clone(),
                labels: issue.labels.clone(),
                deps: issue.deps.clone(),
                relates: issue.relates.clone(),
                duplicates: issue.duplicates.clone(),
                supersedes: issue.supersedes.clone(),
                source_system: Some("beads".to_owned()),
                external_ref: Some(issue.external_ref.clone()),
            };
            let new_item = Item {
                frontmatter,
                body: issue.body.clone(),
            };
            write_item_file(&new_item, &store.path_for(&id))?;
            created += 1;
        }

        Ok(ImportReport {
            created,
            skipped: plan.would_skip.len(),
            conflicts: plan.conflicts.len(),
        })
    }
}

/// Parse and map a single JSONL line into a [`StagedIssue`], returning a
/// human-readable error message on malformed input.
fn map_line(line: &str) -> Result<StagedIssue, String> {
    let issue: BeadsIssue = serde_json::from_str(line).map_err(|err| err.to_string())?;

    let beads_id = issue
        .id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    // Title falls back to the id if absent (foreign data may omit it).
    let title = issue
        .title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| beads_id.clone());

    // Body: beads `description`, else clove-export `body`, else empty.
    let body = issue
        .description
        .clone()
        .or_else(|| issue.body.clone())
        .unwrap_or_default();

    // status: `deferred` → open + label `deferred`; else coerce.
    let raw_status = issue.status.as_deref().unwrap_or("open");
    let mut extra_label: Option<String> = None;
    let status = if raw_status.trim().eq_ignore_ascii_case("deferred") {
        extra_label = Some("deferred".to_owned());
        ItemStatus::Open
    } else {
        coerce_status(raw_status)
    };

    let item_type = issue
        .issue_type
        .as_deref()
        .or(issue.type_field.as_deref())
        .map(beads_type)
        .unwrap_or_default();

    let priority = issue
        .priority
        .as_ref()
        .map(PriorityField::to_priority)
        .unwrap_or(Priority::DEFAULT);

    // assignee wins over owner.
    let assignee = issue
        .assignee
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| issue.owner.clone().filter(|s| !s.trim().is_empty()));

    // Dependencies: structured beads edges take precedence; if absent, fall back
    // to clove-export flat arrays.
    let (mut deps_raw, mut relates_raw, mut parent_raw): (
        Vec<String>,
        Vec<String>,
        Option<String>,
    ) = (Vec::new(), Vec::new(), None);
    if issue.dependencies.is_empty() {
        deps_raw = issue.deps.clone();
        relates_raw = issue.relates.clone();
        parent_raw = issue.parent.clone();
    } else {
        for dep in &issue.dependencies {
            let Some(dep_id) = dep.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            match dep
                .dep_type
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_lowercase()
                .as_str()
            {
                "blocks" | "blocked-by" | "depends-on" | "dependency" => {
                    deps_raw.push(dep_id.to_owned())
                }
                "parent-child" | "parent" | "child" => {
                    if parent_raw.is_none() {
                        parent_raw = Some(dep_id.to_owned());
                    }
                }
                // related | tracks | discovered-from | anything else → relates.
                _ => relates_raw.push(dep_id.to_owned()),
            }
        }
    }

    let parent = match parent_raw {
        Some(p) => Some(parse_id(&p)?),
        None => None,
    };
    let deps = parse_ids(deps_raw.iter().map(String::as_str))?;
    let relates = parse_ids(relates_raw.iter().map(String::as_str))?;
    // `duplicates`/`supersedes` exist only in the clove-export shape (there is
    // no structured-beads-edge equivalent), so they are consumed directly —
    // they are in `MAPPED_KEYS` and must round-trip, not be dropped.
    let duplicates = parse_ids(issue.duplicates.iter().map(String::as_str))?;
    let supersedes = parse_ids(issue.supersedes.iter().map(String::as_str))?;

    let mut labels = map_labels(&issue.labels).map_err(|e| e.to_string())?;
    if let Some(label) = extra_label {
        labels.push(label);
    }
    labels.sort();
    labels.dedup();

    let external_ref = build_external_ref(&beads_id, issue.external_ref.as_deref(), line)?;

    Ok(StagedIssue {
        external_ref,
        source_id: beads_id,
        title,
        status,
        item_type,
        priority,
        assignee,
        parent,
        deps,
        relates,
        duplicates,
        supersedes,
        labels,
        body,
    })
}

/// Build the clove `external_ref` for an incoming issue (see module docs).
///
/// - A pre-existing clove `external_ref` on the line is used verbatim.
/// - Otherwise the key is `"beads:<id>"`, with a key-sorted compact JSON blob of
///   unmapped beads-internal fields appended as `" meta:<json>"` when any exist.
fn build_external_ref(
    beads_id: &str,
    existing: Option<&str>,
    line: &str,
) -> Result<String, String> {
    if let Some(existing) = existing.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(existing.to_owned());
    }

    let base = format!("beads:{beads_id}");
    let value: Value = serde_json::from_str(line).map_err(|err| err.to_string())?;
    let Value::Object(obj) = value else {
        return Ok(base);
    };

    // Collect unmapped keys into a sorted map for a stable, reproducible blob.
    let unmapped: BTreeMap<String, Value> = obj
        .into_iter()
        .filter(|(k, _)| !MAPPED_KEYS.contains(&k.as_str()))
        .collect();

    if unmapped.is_empty() {
        return Ok(base);
    }
    let meta = serde_json::to_string(&unmapped).map_err(|err| err.to_string())?;
    Ok(format!("{base} meta:{meta}"))
}

/// Read the `comment_count` of a raw JSONL line, tolerating its absence.
fn comment_count_of(line: &str) -> Option<i64> {
    let value: Value = serde_json::from_str(line).ok()?;
    value.get("comment_count")?.as_i64()
}

/// Parse a single beads JSONL line through the same tolerant path
/// [`BeadsImporter::plan`] uses — JSON deserialization + field mapping — without
/// touching the filesystem or writing anything. Returns the mapped issue's
/// idempotency `external_ref` on success.
///
/// This is the parse surface exercised by the `import_beads` fuzz target:
/// arbitrary bytes must only ever yield `Ok` or `Err`, never a panic.
pub fn parse_beads_line(line: &str) -> Result<(), ImportError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    map_line(trimmed).map_err(|message| ImportError::Record { message })?;
    Ok(())
}

/// Parse arbitrary bytes as a beads `issues.jsonl` document (line by line),
/// tolerating malformed lines rather than aborting (M2): a bad line is skipped so
/// the parser keeps exercising every subsequent line (mirroring
/// [`BeadsImporter::plan`], which surfaces each bad line as a `would_skip`). This
/// only ever returns `Ok`, never a panic — the property the `import_beads` fuzz
/// target checks.
pub fn parse_beads_bytes(bytes: &[u8]) -> Result<(), ImportError> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        // A malformed line is intentionally ignored here (the plan path reports
        // it); we keep parsing the remaining lines.
        let _ = parse_beads_line(line);
    }
    Ok(())
}

/// Parse a single raw id string into a [`CloveId`].
fn parse_id(raw: &str) -> Result<CloveId, String> {
    CloveId::new(raw.trim()).map_err(|err| format!("invalid id reference `{raw}`: {err}"))
}

/// Parse each raw id string into a [`CloveId`].
fn parse_ids<'a, I>(raw: I) -> Result<Vec<CloveId>, String>
where
    I: IntoIterator<Item = &'a str>,
{
    raw.into_iter().map(parse_id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_beads_native_shape() {
        let line = r#"{"id":"bd-1","title":"T","description":"body","status":"open","priority":1,"issue_type":"task","owner":"ege","labels":["Area:Core"],"dependencies":[{"id":"proj-AAAA1111","type":"blocks"},{"id":"proj-BBBB2222","type":"parent-child"},{"id":"proj-CCCC3333","type":"related"}]}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.source_id, "bd-1");
        assert_eq!(s.title, "T");
        assert_eq!(s.body, "body");
        assert_eq!(s.item_type, ItemType::Chore);
        assert_eq!(s.assignee.as_deref(), Some("ege"));
        assert_eq!(s.labels, vec!["area:core"]);
        assert_eq!(s.deps.len(), 1);
        assert!(s.parent.is_some());
        assert_eq!(s.relates.len(), 1);
        assert_eq!(s.external_ref, "beads:bd-1");
    }

    #[test]
    fn deferred_status_maps_to_open_plus_label() {
        let line = r#"{"id":"bd-2","title":"X","status":"deferred"}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.status, ItemStatus::Open);
        assert!(s.labels.contains(&"deferred".to_owned()));
    }

    #[test]
    fn duplicates_and_supersedes_are_mapped_not_dropped() {
        // Both keys are in MAPPED_KEYS (excluded from the meta blob), so they
        // must actually be consumed — the clove-export → beads-import
        // round-trip is documented as lossless on mapped fields.
        let line = r#"{"id":"bd-9","title":"X","duplicates":["proj-AAAA1111"],"supersedes":["proj-BBBB2222"]}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.duplicates.len(), 1);
        assert_eq!(s.duplicates[0].as_str(), "proj-AAAA1111");
        assert_eq!(s.supersedes.len(), 1);
        assert_eq!(s.supersedes[0].as_str(), "proj-BBBB2222");
        assert!(!s.external_ref.contains("meta:"), "{}", s.external_ref);
    }

    #[test]
    fn unmapped_fields_folded_into_meta_blob() {
        let line = r#"{"id":"bd-3","title":"X","epic":"e-9","sprint":3}"#;
        let s = map_line(line).unwrap();
        assert!(s.external_ref.starts_with("beads:bd-3 meta:"));
        assert!(s.external_ref.contains("\"epic\":\"e-9\""));
        assert!(s.external_ref.contains("\"sprint\":3"));
    }

    #[test]
    fn meta_blob_is_stable_across_calls() {
        let line = r#"{"id":"bd-3","title":"X","sprint":3,"epic":"e-9"}"#;
        let a = map_line(line).unwrap().external_ref;
        let b = map_line(line).unwrap().external_ref;
        assert_eq!(a, b, "meta blob must be deterministic for idempotency");
    }

    #[test]
    fn reads_clove_export_shape() {
        let line = r#"{"id":"proj-ZZZZ9999","title":"Exported","type":"bug","status":"in_progress","priority":0,"body":"hello","deps":["proj-AAAA1111"],"relates":["proj-BBBB2222"],"parent":"proj-CCCC3333","external_ref":"beads:bd-99","source_system":"beads","ready":true,"blocked_by":[]}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.item_type, ItemType::Bug);
        assert_eq!(s.status, ItemStatus::InProgress);
        assert_eq!(s.priority, Priority(0));
        assert_eq!(s.body, "hello");
        assert_eq!(s.deps.len(), 1);
        assert_eq!(s.relates.len(), 1);
        assert!(s.parent.is_some());
        // A pre-existing clove external_ref is used verbatim.
        assert_eq!(s.external_ref, "beads:bd-99");
    }

    #[test]
    fn priority_as_string_tolerated() {
        let line = r#"{"id":"bd-5","title":"X","priority":"3"}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.priority, Priority(3));
    }

    #[test]
    fn assignee_wins_over_owner() {
        let line = r#"{"id":"bd-6","title":"X","assignee":"a","owner":"o"}"#;
        let s = map_line(line).unwrap();
        assert_eq!(s.assignee.as_deref(), Some("a"));
    }

    #[test]
    fn malformed_line_is_error_not_panic() {
        assert!(parse_beads_line("{not json").is_err());
        assert!(parse_beads_line("").is_ok());
    }

    #[test]
    fn comment_count_extracted() {
        assert_eq!(comment_count_of(r#"{"id":"x","comment_count":4}"#), Some(4));
        assert_eq!(comment_count_of(r#"{"id":"x"}"#), None);
    }

    // M2: a malformed line in the middle must not abort parsing of the rest.
    #[test]
    fn parse_beads_bytes_tolerates_malformed_lines() {
        let doc = b"{\"id\":\"bd-1\"}\n{not json\n{\"id\":\"bd-3\"}\n";
        // Never errors / panics even with a bad middle line.
        assert!(parse_beads_bytes(doc).is_ok());
    }
}
