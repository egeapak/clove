//! tk importer (T-M01): import a tk `.tickets/` directory (DESIGN.md §11.1).
//!
//! tk stores one ticket per `*.md` file as YAML frontmatter + a Markdown body.
//! tk's frontmatter field set differs from clove's, so we deserialize into a
//! dedicated, tolerant [`TkTicket`] struct (no `deny_unknown_fields`) rather than
//! clove's strict [`ItemFrontmatter`], then map each field through the shared
//! coercion helpers in [`crate::map`].
//!
//! ## Field mapping (DESIGN §11.1)
//!
//! | tk | clove |
//! |---|---|
//! | `id` | idempotency key → `external_ref = "tk:<id>"`; clove mints a fresh `CloveId` |
//! | first `# H1` in body | `title` (stripped from the body); filename stem fallback + warning |
//! | `status` | `status` (via [`crate::map::coerce_status`]) |
//! | `type` (`task` → `chore`) | `type` (via [`crate::map::tk_type`]) |
//! | `priority` | `priority` (via [`crate::map::coerce_priority`]) |
//! | `assignee` | `assignee` |
//! | `parent` | `parent` |
//! | `deps` | `deps` |
//! | `tags` | `labels` (normalized via [`crate::map::map_labels`]) |
//! | `links` | `relates` |
//! | `external-ref` | folded into `external_ref` as `"tk:<id> upstream:<value>"` |
//! | — | `source_system = "tk"` |
//!
//! ## external_ref rule
//!
//! The clove `external_ref` is always namespaced as `"tk:<tk-id>"`, which is the
//! idempotency key (DESIGN §11.3: skip incoming items whose `external_ref`
//! matches an existing item). Namespacing keeps tk ids collision-free against
//! other importers. If a tk ticket *also* carries its own upstream `external-ref`
//! field, it is preserved by appending `" upstream:<value>"` — re-importing the
//! same ticket reproduces the identical value, so idempotency is unaffected.
//!
//! ## plan / apply split
//!
//! [`TkImporter::plan`] is pure (no writes): it reads + maps every ticket, stashes
//! the fully-mapped [`StagedTicket`]s internally (so [`apply`](TkImporter::apply)
//! need not re-read the source), and returns the `{would_create, would_skip,
//! conflicts}` plan. [`apply`](TkImporter::apply) consumes the staged records
//! POSITIONALLY (one written file per staged record), so two tickets sharing a
//! source id can never collapse onto one — the later duplicate is reported as a
//! `would_skip` with reason `"duplicate_id"`. Title-fallback / dep-cap-truncation
//! / dangling-dependency warnings collected during planning are exposed via
//! [`TkImporter::take_warnings`] for the CLI to print to stderr *and* surface in
//! the JSON envelope's `_meta.warnings`.

use std::cell::RefCell;
use std::collections::HashSet;

use camino::Utf8Path;
use chrono::{DateTime, Utc};
use clove_core::contains_yaml_anchor_or_alias;
use clove_core::id::new_id;
use clove_core::model::CURRENT_SCHEMA_VERSION;
use clove_core::write::write_item_file;
use clove_core::{CloveId, Item, ItemFrontmatter, ItemStatus, ItemStore, ItemType, Priority};
use serde::Deserialize;

use crate::error::ImportError;
use crate::map::{
    cap_dep_array, coerce_priority, coerce_status, dangling_targets, map_labels, tk_type,
    MAX_IMPORT_UNIT_BYTES,
};
use crate::plan::{ImportPlan, ImportReport, PlanItem, SkipItem};
use crate::{ImportCtx, Importer};

/// The tolerant deserialization view of a tk ticket's YAML frontmatter.
///
/// Unknown fields are ignored (no `deny_unknown_fields`) so tk-specific keys
/// clove does not model never break the import. Every field is optional — tk
/// files in the wild omit many of them.
#[derive(Debug, Clone, Default, Deserialize)]
struct TkTicket {
    /// tk's own id (idempotency key; preserved only inside `external_ref`).
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// tk's ticket type (`task` maps to clove `chore`).
    #[serde(default, rename = "type")]
    ticket_type: Option<String>,
    /// tk priority as an integer (clamped into clove's 0..=4 range).
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    links: Vec<String>,
    /// tk's upstream external reference (hyphenated key on the tk side).
    #[serde(default, rename = "external-ref")]
    external_ref: Option<String>,
}

/// A fully mapped ticket, staged during [`TkImporter::plan`] and consumed by
/// [`TkImporter::apply`]. Holds canonical clove field values plus the idempotency
/// `external_ref` and source title used in the plan.
#[derive(Debug, Clone)]
struct StagedTicket {
    /// The idempotency key / clove `external_ref` (`"tk:<id>"`, possibly with an
    /// appended `" upstream:<value>"`).
    external_ref: String,
    title: String,
    status: ItemStatus,
    item_type: ItemType,
    priority: Priority,
    assignee: Option<String>,
    parent: Option<CloveId>,
    deps: Vec<CloveId>,
    relates: Vec<CloveId>,
    labels: Vec<String>,
    body: String,
}

/// The tk importer.
///
/// Constructed with the id `prefix` (so [`apply`](TkImporter::apply) can mint
/// fresh [`CloveId`]s) and a clock for the `created`/`updated` stamps. Planning
/// stashes mapped tickets in `staged` and any title-fallback `warnings`.
#[derive(Debug)]
pub struct TkImporter {
    prefix: String,
    now: DateTime<Utc>,
    staged: RefCell<Vec<StagedTicket>>,
    warnings: RefCell<Vec<String>>,
}

impl TkImporter {
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

    /// Drain the warnings collected during [`plan`](TkImporter::plan) (e.g. a
    /// ticket with no `# H1` whose title fell back to the filename stem). The CLI
    /// prints these to stderr.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }
}

impl Importer for TkImporter {
    fn plan(&self, src: &Utf8Path, ctx: &ImportCtx) -> Result<ImportPlan, ImportError> {
        let mut paths = ticket_paths(src)?;
        // Deterministic order so plans/warnings are stable across runs.
        paths.sort();

        let mut plan = ImportPlan::new();
        let mut staged = self.staged.borrow_mut();
        staged.clear();

        // Source ids already seen in this run, to detect intra-source duplicates
        // (C1): the first occurrence is staged; later ones are skipped, never
        // silently overwriting the first staged record's data.
        let mut seen_ids: HashSet<String> = HashSet::new();

        for path in paths {
            // M5: cap the per-file byte size before reading/parsing so a
            // pathologically large foreign file can never be slurped whole.
            let metadata = std::fs::metadata(&path).map_err(|source| ImportError::Source {
                path: path.clone(),
                message: source.to_string(),
            })?;
            if metadata.len() as usize > MAX_IMPORT_UNIT_BYTES {
                return Err(ImportError::Record {
                    message: format!(
                        "{path}: file is {} bytes, exceeding the import limit of {MAX_IMPORT_UNIT_BYTES}",
                        metadata.len()
                    ),
                });
            }
            let bytes = std::fs::read(&path).map_err(|source| ImportError::Source {
                path: path.clone(),
                message: source.to_string(),
            })?;
            let text = String::from_utf8_lossy(&bytes);
            let (frontmatter_text, body) = split_frontmatter(&text);

            // M5: reject YAML anchors/aliases (bomb guard) using clove-core's own
            // guard, exactly as the strict parser does, before deserializing.
            if contains_yaml_anchor_or_alias(frontmatter_text.as_bytes()) {
                return Err(ImportError::Record {
                    message: format!("{path}: YAML anchors/aliases are not allowed"),
                });
            }

            let ticket: TkTicket =
                serde_yaml_neo::from_str(frontmatter_text).map_err(|err| ImportError::Record {
                    message: format!("{path}: {err}"),
                })?;

            let stem = path.file_stem().unwrap_or("ticket").to_owned();
            let tk_id = ticket.id.clone().unwrap_or_else(|| stem.clone());

            // Title: first `# H1` in the body, stripped from the stored body; else
            // fall back to the filename stem with a warning.
            let (title, body) = match extract_h1(&body) {
                Some((heading, rest)) => (heading, rest),
                None => {
                    self.warnings.borrow_mut().push(format!(
                        "{path}: no `# H1` heading; using filename stem `{stem}` as title"
                    ));
                    (stem.clone(), body)
                }
            };

            // C1: a second ticket sharing the same source id would otherwise be
            // paired back to the first staged record. Skip it and report.
            if !seen_ids.insert(tk_id.clone()) {
                plan.would_skip.push(SkipItem {
                    id: tk_id,
                    reason: "duplicate_id".to_owned(),
                });
                continue;
            }

            let external_ref = match ticket.external_ref.as_deref() {
                Some(upstream) if !upstream.trim().is_empty() => {
                    format!("tk:{tk_id} upstream:{}", upstream.trim())
                }
                _ => format!("tk:{tk_id}"),
            };

            let status = ticket
                .status
                .as_deref()
                .map(coerce_status)
                .unwrap_or(ItemStatus::Open);
            let item_type = ticket
                .ticket_type
                .as_deref()
                .map(tk_type)
                .unwrap_or_default();
            let priority =
                coerce_priority(ticket.priority.unwrap_or(i64::from(Priority::DEFAULT.0)));
            let parent = parse_ids(&path, ticket.parent.as_deref())?
                .into_iter()
                .next();
            // M4: enforce the per-array dep cap (truncate + warn) so the written
            // file can never violate the store's own validation limit.
            let deps = cap_dep_array(
                parse_ids(&path, ticket.deps.iter().map(String::as_str))?,
                "deps",
                &tk_id,
                &mut self.warnings.borrow_mut(),
            );
            let relates = cap_dep_array(
                parse_ids(&path, ticket.links.iter().map(String::as_str))?,
                "relates",
                &tk_id,
                &mut self.warnings.borrow_mut(),
            );
            let mut labels = map_labels(&ticket.tags)?;
            labels.sort();
            labels.dedup();

            // M4: flag dangling dependency targets (ids absent from the store).
            // Report-only: the write still proceeds.
            let dangling = dangling_targets(
                &ctx.store_ids,
                parent.iter().chain(deps.iter()).chain(relates.iter()),
            );
            if !dangling.is_empty() {
                let list = dangling
                    .iter()
                    .map(CloveId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                self.warnings.borrow_mut().push(format!(
                    "item `{tk_id}`: dangling dependency target(s) not present in the store: {list}"
                ));
            }

            let staged_ticket = StagedTicket {
                external_ref: external_ref.clone(),
                title: title.clone(),
                status,
                item_type,
                priority,
                assignee: ticket.assignee.clone(),
                parent,
                deps,
                relates,
                labels,
                body,
            };

            if ctx.is_imported(&external_ref) {
                // M3: report field-level divergences against the existing item
                // (status, priority, title); the write is still skipped.
                let conflicts = ctx.conflicts_for(&external_ref, &tk_id, status, priority, &title);
                plan.conflicts.extend(conflicts);
                plan.would_skip.push(SkipItem {
                    id: tk_id,
                    reason: "already_imported".to_owned(),
                });
            } else {
                plan.would_create.push(PlanItem { id: tk_id, title });
                staged.push(staged_ticket);
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
        // with its own data — a shared source id can no longer collapse two
        // records onto the first one.
        for ticket in staged.iter() {
            let id = new_id(&self.prefix, store.issues_dir())?;
            let closed = if ticket.status == ItemStatus::Closed {
                Some(self.now)
            } else {
                None
            };
            let frontmatter = ItemFrontmatter {
                schema: CURRENT_SCHEMA_VERSION,
                id: id.clone(),
                title: ticket.title.clone(),
                status: ticket.status,
                item_type: ticket.item_type,
                priority: ticket.priority,
                created: self.now,
                updated: self.now,
                closed,
                assignee: ticket.assignee.clone(),
                parent: ticket.parent.clone(),
                labels: ticket.labels.clone(),
                deps: ticket.deps.clone(),
                relates: ticket.relates.clone(),
                duplicates: Vec::new(),
                supersedes: Vec::new(),
                source_system: Some("tk".to_owned()),
                external_ref: Some(ticket.external_ref.clone()),
            };
            let new_item = Item {
                frontmatter,
                body: ticket.body.clone(),
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

/// Parse a single tk ticket file's raw bytes through the same path
/// [`TkImporter::plan`] uses — frontmatter split, tolerant [`TkTicket`]
/// deserialization, and `# H1` extraction — without touching the filesystem or
/// writing anything. Returns the deserialized ticket on success.
///
/// This is the parse surface exercised by the `import_tk` fuzz target: arbitrary
/// bytes must only ever yield `Ok` or `Err`, never a panic.
pub fn parse_ticket_bytes(bytes: &[u8]) -> Result<(), ImportError> {
    let text = String::from_utf8_lossy(bytes);
    let (frontmatter_text, body) = split_frontmatter(&text);
    let _ticket: TkTicket =
        serde_yaml_neo::from_str(frontmatter_text).map_err(|err| ImportError::Record {
            message: err.to_string(),
        })?;
    let _ = extract_h1(&body);
    Ok(())
}

/// Collect `*.md` file paths directly inside `src` (non-recursive: skips
/// subdirectories, symlinks, and non-`.md` files).
fn ticket_paths(src: &Utf8Path) -> Result<Vec<camino::Utf8PathBuf>, ImportError> {
    let read_dir = std::fs::read_dir(src).map_err(|source| ImportError::Source {
        path: src.to_owned(),
        message: source.to_string(),
    })?;
    let mut paths = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| ImportError::Source {
            path: src.to_owned(),
            message: source.to_string(),
        })?;
        let file_type = entry.file_type().map_err(|source| ImportError::Source {
            path: src.to_owned(),
            message: source.to_string(),
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let Ok(path) = camino::Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if path.extension() != Some("md") {
            continue;
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Parse each raw id string into a [`CloveId`], surfacing a [`ImportError::Record`]
/// on a malformed reference.
fn parse_ids<'a, I>(path: &Utf8Path, raw: I) -> Result<Vec<CloveId>, ImportError>
where
    I: IntoIterator<Item = &'a str>,
{
    raw.into_iter()
        .map(|r| {
            CloveId::new(r.trim()).map_err(|err| ImportError::Record {
                message: format!("{path}: invalid id reference `{r}`: {err}"),
            })
        })
        .collect()
}

/// Split a file into (frontmatter text, body text). Tolerant of a missing
/// frontmatter block (whole file is then the body) and of LF/CRLF line endings.
/// Unlike clove's strict parser this never errors — tk files are foreign input.
fn split_frontmatter(text: &str) -> (&str, String) {
    // H1: a leading UTF-8 BOM otherwise defeats the `---` prefix check, making the
    // whole file parse as body and silently dropping id/status/deps. Strip it.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return ("", text.to_owned());
    };
    // Find a closing fence line consisting of exactly `---`.
    let mut search = 0usize;
    while let Some(rel) = rest[search..].find("---") {
        let fence_start = search + rel;
        let at_line_start = fence_start == 0 || rest.as_bytes()[fence_start - 1] == b'\n';
        let after = fence_start + 3;
        let body_start = if after == rest.len() {
            Some(after)
        } else if rest.as_bytes()[after] == b'\n' {
            Some(after + 1)
        } else if rest.as_bytes()[after] == b'\r' && rest[after..].starts_with("\r\n") {
            Some(after + 2)
        } else {
            None
        };
        match (at_line_start, body_start) {
            (true, Some(body_start)) => {
                let fm_end = fence_start.saturating_sub(1);
                let frontmatter = &rest[..fm_end];
                let body = rest[body_start..].to_owned();
                return (frontmatter, body);
            }
            _ => search = after,
        }
    }
    // No closing fence: treat the entire remainder as frontmatter, empty body.
    (rest, String::new())
}

/// Extract the first Markdown `# H1` heading from `body`, returning
/// `(heading_text, remaining_body)` with that heading line removed. Returns
/// `None` if no `# ` heading is present.
fn extract_h1(body: &str) -> Option<(String, String)> {
    let mut lines: Vec<&str> = body.lines().collect();
    // Track fenced-code state so a `# ` line inside a ``` (or ~~~) fence is not
    // mistaken for the H1 heading (Nit).
    let mut in_fence = false;
    let idx = lines.iter().position(|line| {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            return false;
        }
        if in_fence {
            return false;
        }
        t.strip_prefix("# ").is_some_and(|h| !h.trim().is_empty())
    })?;
    let heading = lines[idx].trim_start();
    let heading = heading
        .strip_prefix("# ")
        .map(str::trim)
        .unwrap_or(heading)
        .to_owned();
    lines.remove(idx);
    // Drop a single blank line left immediately after the removed heading so the
    // body does not start with stray blank lines.
    if lines.get(idx).is_some_and(|l| l.trim().is_empty()) {
        lines.remove(idx);
    }
    let trailing_newline = body.ends_with('\n');
    let mut rest = lines.join("\n");
    if trailing_newline && !rest.is_empty() {
        rest.push('\n');
    }
    Some((heading, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let (fm, body) = split_frontmatter("---\nid: t-1\n---\nhello\n");
        assert_eq!(fm, "id: t-1");
        assert_eq!(body, "hello\n");
    }

    #[test]
    fn split_tolerates_missing_frontmatter() {
        let (fm, body) = split_frontmatter("just a body\n");
        assert_eq!(fm, "");
        assert_eq!(body, "just a body\n");
    }

    #[test]
    fn extracts_h1_and_strips_it() {
        let (title, body) = extract_h1("# My Title\n\nThe body.\n").unwrap();
        assert_eq!(title, "My Title");
        assert_eq!(body, "The body.\n");
    }

    #[test]
    fn extract_h1_none_when_absent() {
        assert!(extract_h1("no heading here\n").is_none());
        // A code-comment `#foo` (no space) is not an H1.
        assert!(extract_h1("#nospace\n").is_none());
    }

    #[test]
    fn deserializes_tolerating_unknown_fields() {
        let yaml = "id: t-1\nstatus: open\ntype: task\nunknown_field: ignored\ntags: [a, b]\n";
        let t: TkTicket = serde_yaml_neo::from_str(yaml).unwrap();
        assert_eq!(t.id.as_deref(), Some("t-1"));
        assert_eq!(t.ticket_type.as_deref(), Some("task"));
        assert_eq!(t.tags, vec!["a", "b"]);
    }

    // H1: a leading UTF-8 BOM must not defeat frontmatter detection.
    #[test]
    fn split_strips_leading_bom_before_fence() {
        let (fm, body) = split_frontmatter("\u{FEFF}---\nid: t-1\nstatus: closed\n---\nhi\n");
        assert_eq!(fm, "id: t-1\nstatus: closed");
        assert_eq!(body, "hi\n");
        // The whole document still deserializes id/status (not dropped to body).
        let t: TkTicket = serde_yaml_neo::from_str(fm).unwrap();
        assert_eq!(t.id.as_deref(), Some("t-1"));
        assert_eq!(t.status.as_deref(), Some("closed"));
    }

    // Nit: a `# ` line inside a fenced code block is not the H1.
    #[test]
    fn extract_h1_ignores_headings_inside_code_fences() {
        let body = "```\n# not a heading\n```\n\n# Real Title\n\nBody.\n";
        let (title, rest) = extract_h1(body).unwrap();
        assert_eq!(title, "Real Title");
        assert!(
            rest.contains("# not a heading"),
            "fence content kept: {rest:?}"
        );
        // A document whose only `# ` line is fenced has no extractable H1.
        assert!(extract_h1("```\n# fenced only\n```\n").is_none());
    }
}
