//! `clove doctor` (T-CLI18): store health check with optional safe repairs.

use clove_core::{diagnose, doctor_fix, CloveError, DoctorReport, OutputFormat, Severity};
use serde_json::{json, Value};

use crate::cli::DoctorArgs;
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::output::print_json_success;

pub fn run(ctx: &Ctx, format: OutputFormat, args: DoctorArgs) -> Result<ExitCode, CloveError> {
    let mut fixed = 0;
    if args.fix {
        fixed = doctor_fix(&ctx.store)?;
    }
    let report = diagnose(&ctx.store);

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
