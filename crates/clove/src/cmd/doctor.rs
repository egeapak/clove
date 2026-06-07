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

    // T-S08 + integrity (M4): index health — schema version, internal
    // corruption, and files-vs-index divergence. Skipped with --no-index or when
    // no index file exists. Every index finding is repaired by a full rebuild.
    if !no_index && ctx.db_path.exists() {
        let issues = index_checks(ctx)?;
        if args.fix && !issues.is_empty() {
            clove_index::reindex(&ctx.issues_dir, &ctx.db_path)
                .map_err(|e| index_error(e, &ctx.db_path))?;
            fixed += issues.len();
            // Re-check; surface anything the rebuild did not resolve.
            report.issues.extend(index_checks(ctx)?);
        } else {
            report.issues.extend(issues);
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

/// Inspect the on-disk index **without healing it** — uses [`clove_index::Index::open`]
/// (not `open_or_create`), so a schema-version mismatch or corruption is reported
/// as a finding rather than silently rebuilt out from under the user. Returns
/// every index finding (zero or one); all are `fixable` via a `reindex`.
fn index_checks(ctx: &Ctx) -> Result<Vec<DoctorIssue>, CloveError> {
    use clove_index::{Index, IndexError};

    match Index::open(&ctx.db_path) {
        Ok(index) => {
            // Internal integrity first: a structurally-corrupt cache silently
            // serves wrong query results, so it is an error, not a warning.
            if let Some(reason) = index
                .integrity_check()
                .map_err(|e| index_error(e, &ctx.db_path))?
            {
                return Ok(vec![corrupt_issue(&reason)]);
            }
            // Healthy and current: the only remaining question is freshness.
            Ok(index_divergence(&index, ctx)?.into_iter().collect())
        }
        Err(IndexError::SchemaMismatch { found, expected }) => Ok(vec![DoctorIssue {
            severity: Severity::Warning,
            code: "INDEX_SCHEMA_MISMATCH",
            item: None,
            message: format!(
                "index schema is v{found} but this clove expects v{expected}; \
                 run `clove reindex`"
            ),
            fixable: true,
        }]),
        Err(IndexError::CorruptIndex(msg)) => Ok(vec![corrupt_issue(&msg)]),
        Err(IndexError::SqliteError(e)) if clove_index::db::is_corrupt(&e) => {
            Ok(vec![corrupt_issue(&e.to_string())])
        }
        Err(_) => Ok(vec![DoctorIssue {
            severity: Severity::Warning,
            code: "INDEX_UNREADABLE",
            item: None,
            message: "index is unreadable; run `clove reindex`".to_owned(),
            fixable: true,
        }]),
    }
}

/// An `INDEX_CORRUPT` error finding (fixable by a rebuild from the files).
fn corrupt_issue(detail: &str) -> DoctorIssue {
    DoctorIssue {
        severity: Severity::Error,
        code: "INDEX_CORRUPT",
        item: None,
        message: format!("index is corrupt ({detail}); run `clove reindex`"),
        fixable: true,
    }
}

/// Compare an already-opened, healthy index against the files (reusing the
/// staleness machinery). Returns a fixable warning when they diverge.
fn index_divergence(
    index: &clove_index::Index,
    ctx: &Ctx,
) -> Result<Option<DoctorIssue>, CloveError> {
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
