//! Pure rendering of import plans/reports to the CLI's JSON and human shapes.
//!
//! These functions carry **no** I/O: they return a [`serde_json::Value`] or a
//! `String` so the host and the `clove-import-<provider>` plugins share one
//! copy of the exact envelope/summary formatting. They reproduce, byte-for-byte,
//! what the former in-process `clove/src/cmd/import.rs` printed:
//!
//! - [`plan_json`] / [`report_json`] — the `data` payloads (`{would_create,
//!   would_skip, conflicts}` for a dry-run plan; `{created, skipped, conflicts}`
//!   for an applied report).
//! - [`plan_human`] / [`report_human`] — the human-format stdout lines.
//!
//! Warnings (stderr `warning: …` lines and the JSON `_meta.warnings` array) are
//! not part of these payloads; the caller surfaces them.

use serde_json::{json, Value};

use crate::{ImportPlan, ImportReport};

/// The dry-run plan `data` payload: the `ImportPlan` serialized verbatim
/// (`{ would_create, would_skip, conflicts }`, DESIGN §11.3). Falls back to an
/// empty object on the (unreachable) serialize failure, matching the host.
pub fn plan_json(plan: &ImportPlan) -> Value {
    serde_json::to_value(plan).unwrap_or_else(|_| json!({}))
}

/// The applied-report `data` payload: `{ created, skipped, conflicts }` (the
/// same object the host's `emit_report` built).
pub fn report_json(report: &ImportReport) -> Value {
    json!({
        "created": report.created,
        "skipped": report.skipped,
        "conflicts": report.conflicts,
    })
}

/// The human-format dry-run summary: a header line plus one line per would-create
/// / would-skip entry. Reproduces the host's `emit_plan` human branch exactly.
///
/// Returns the block **without** a trailing newline (lines joined by `\n`), so a
/// single `println!("{}", plan_human(&plan))` yields byte-identical output to the
/// host's sequence of per-line `println!`s.
pub fn plan_human(plan: &ImportPlan) -> String {
    let mut lines = vec![format!(
        "dry-run: would create {}, would skip {}, conflicts {}",
        plan.would_create.len(),
        plan.would_skip.len(),
        plan.conflicts.len()
    )];
    for item in &plan.would_create {
        lines.push(format!("  create  {}  {}", item.id, item.title));
    }
    for item in &plan.would_skip {
        lines.push(format!("  skip    {}  ({})", item.id, item.reason));
    }
    lines.join("\n")
}

/// The human-format applied summary line (no trailing newline). Reproduces the
/// host's `emit_report` human `println!` exactly when `println!`ed.
pub fn report_human(report: &ImportReport) -> String {
    format!(
        "imported: {} created, {} skipped, {} conflicts",
        report.created, report.skipped, report.conflicts
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ConflictItem, PlanItem, SkipItem};

    fn sample_plan() -> ImportPlan {
        ImportPlan {
            would_create: vec![PlanItem {
                id: "tk-1".to_owned(),
                title: "First".to_owned(),
            }],
            would_skip: vec![SkipItem {
                id: "tk-2".to_owned(),
                reason: "already_imported".to_owned(),
            }],
            conflicts: vec![ConflictItem {
                id: "tk-3".to_owned(),
                field: "status".to_owned(),
                existing: "open".to_owned(),
                incoming: "closed".to_owned(),
            }],
        }
    }

    #[test]
    fn plan_json_is_the_serialized_plan() {
        let plan = sample_plan();
        let v = plan_json(&plan);
        assert_eq!(v["would_create"].as_array().unwrap().len(), 1);
        assert_eq!(v["would_create"][0]["id"], "tk-1");
        assert_eq!(v["would_create"][0]["title"], "First");
        assert_eq!(v["would_skip"][0]["id"], "tk-2");
        assert_eq!(v["would_skip"][0]["reason"], "already_imported");
        assert_eq!(v["conflicts"][0]["field"], "status");
        assert_eq!(v["conflicts"][0]["existing"], "open");
        assert_eq!(v["conflicts"][0]["incoming"], "closed");
    }

    #[test]
    fn report_json_shape() {
        let report = ImportReport {
            created: 3,
            skipped: 1,
            conflicts: 2,
        };
        let v = report_json(&report);
        assert_eq!(v, json!({ "created": 3, "skipped": 1, "conflicts": 2 }));
    }

    #[test]
    fn plan_human_matches_cmd_import_lines() {
        let plan = sample_plan();
        let out = plan_human(&plan);
        assert_eq!(
            out,
            "dry-run: would create 1, would skip 1, conflicts 1\n\
             \x20 create  tk-1  First\n\
             \x20 skip    tk-2  (already_imported)"
        );
    }

    #[test]
    fn plan_human_empty_is_just_the_header() {
        let out = plan_human(&ImportPlan::default());
        assert_eq!(out, "dry-run: would create 0, would skip 0, conflicts 0");
    }

    #[test]
    fn report_human_matches_cmd_import_line() {
        let report = ImportReport {
            created: 2,
            skipped: 5,
            conflicts: 0,
        };
        assert_eq!(
            report_human(&report),
            "imported: 2 created, 5 skipped, 0 conflicts"
        );
    }
}
