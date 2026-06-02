//! `clove doctor` (T-CLI18, T-S08): store health check with optional safe
//! repairs, plus an index↔files divergence check when an index is present.

use clove_core::{
    diagnose, doctor_fix, CloveError, DoctorIssue, DoctorReport, OutputFormat, Severity,
};
use serde_json::{json, Value};

use crate::cli::DoctorArgs;
use crate::context::{index_error, Ctx};
use crate::exit::ExitCode;
use crate::output::print_json_success;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: DoctorArgs,
    no_index: bool,
) -> Result<ExitCode, CloveError> {
    let mut fixed = 0;
    if args.fix {
        fixed = doctor_fix(&ctx.store)?;
    }
    let mut report = diagnose(&ctx.store);

    // T-S08: index divergence check (skipped with --no-index or no index file).
    if !no_index && ctx.db_path.exists() {
        if let Some(issue) = index_divergence(ctx)? {
            if args.fix {
                clove_index::reindex(&ctx.issues_dir, &ctx.db_path)
                    .map_err(|e| index_error(e, &ctx.db_path))?;
                fixed += 1;
                // Re-check; report only if it is still diverged.
                if let Some(again) = index_divergence(ctx)? {
                    report.issues.push(again);
                }
            } else {
                report.issues.push(issue);
            }
        }
    }

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => emit_json(&report, fixed),
        OutputFormat::Human => emit_human(&report, fixed),
    }

    if args.strict && report.errors() > 0 {
        Ok(ExitCode::Validation)
    } else {
        Ok(ExitCode::Success)
    }
}

/// Compare the index against the files (reusing the staleness machinery).
/// Returns a fixable warning when they diverge.
fn index_divergence(ctx: &Ctx) -> Result<Option<DoctorIssue>, CloveError> {
    let index = match clove_index::Index::open_or_create(&ctx.db_path) {
        Ok(index) => index,
        Err(_) => {
            return Ok(Some(DoctorIssue {
                severity: Severity::Warning,
                code: "INDEX_UNREADABLE",
                item: None,
                message: "index is unreadable; run `clove reindex`".to_owned(),
                fixable: true,
            }))
        }
    };
    let staleness = index
        .check_staleness(&ctx.issues_dir)
        .map_err(|e| index_error(e, &ctx.db_path))?;
    if staleness.is_clean() {
        Ok(None)
    } else {
        Ok(Some(DoctorIssue {
            severity: Severity::Warning,
            code: "INDEX_DIVERGENCE",
            item: None,
            message: format!(
                "index differs from files ({} change(s)); run `clove reindex`",
                staleness.change_count()
            ),
            fixable: true,
        }))
    }
}

fn emit_json(report: &DoctorReport, fixed: usize) {
    let issues: Vec<Value> = report
        .issues
        .iter()
        .map(|i| {
            json!({
                "severity": i.severity.as_str(),
                "code": i.code,
                "item": i.item,
                "message": i.message,
                "fixable": i.fixable,
            })
        })
        .collect();
    print_json_success(
        json!({
            "issues": issues,
            "summary": {
                "errors": report.errors(),
                "warnings": report.warnings(),
                "fixed": fixed,
                "checked": report.checked,
            },
        }),
        json!({ "warnings": [] }),
    );
}

fn emit_human(report: &DoctorReport, fixed: usize) {
    for issue in &report.issues {
        let prefix = match issue.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match &issue.item {
            Some(item) => println!("{prefix}: [{}] {} ({})", issue.code, issue.message, item),
            None => println!("{prefix}: [{}] {}", issue.code, issue.message),
        }
    }
    println!(
        "checked {}, {} error(s), {} warning(s), {} fixed",
        report.checked,
        report.errors(),
        report.warnings(),
        fixed
    );
}
