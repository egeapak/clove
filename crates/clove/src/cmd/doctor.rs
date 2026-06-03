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

    // T-D07: daemon-health check (independent of the index; runs even with
    // --no-index, since it inspects socket/pid state, not the index).
    if let Some(issue) = daemon_health(ctx) {
        if args.fix {
            clove_ipc::client::cleanup_stale(daemon_dir(ctx));
            fixed += 1;
        } else {
            report.issues.push(issue);
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

/// The `.clove/` directory (parent of `issues/`).
fn daemon_dir(ctx: &Ctx) -> &camino::Utf8Path {
    ctx.issues_dir.parent().unwrap_or(&ctx.issues_dir)
}

/// T-D07: detect a dead-daemon footprint. When `daemon.sock`/`daemon.pid` are
/// present but no daemon answers, they are corpse files from a crash; `--fix`
/// removes them (the DESIGN §8.3 cleanup as an explicit repair). A live daemon —
/// or a lone leftover `daemon.lock` (normal, reused on next start) — is no finding.
fn daemon_health(ctx: &Ctx) -> Option<DoctorIssue> {
    let dir = daemon_dir(ctx);
    let sock = clove_ipc::sock_path(dir).exists();
    let pid = clove_ipc::pid_path(dir).exists();
    if (sock || pid) && !clove_ipc::DaemonClient::is_alive(dir) {
        return Some(DoctorIssue {
            severity: Severity::Warning,
            code: "DAEMON_STALE_SOCKET",
            item: None,
            message: "stale daemon socket/pid from a crashed daemon; \
                      run `clove doctor --fix` to remove them"
                .to_owned(),
            fixable: true,
        });
    }
    None
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
