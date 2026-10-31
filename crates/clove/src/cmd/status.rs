//! `clove status`/`start`/`close` (T-CLI06).
//!
//! Each command takes one or more ids. With one id the output is the
//! single-item shape (the item object, or the standard error envelope). With
//! several ids every id is attempted, in order, and the output has one entry per
//! id; the exit code is non-zero if any id failed (see [`run`]).

use clove_core::{ops, OutputFormat};
use clove_plugin::outln;
use clove_types::{CloveError, CloveId, Item, ItemStatus};
use serde_json::{json, Map, Value};

use crate::context::Ctx;
use crate::exit::{classify, ExitCode};
use crate::item_json::{item_object, print_human, print_item};
use crate::output::ENVELOPE_VERSION;
use crate::util::{now_millis, parse_id};

/// Apply `status` to every id in `ids`.
///
/// - One id: prints the updated item (unchanged single-id output), or returns
///   the error so the caller emits the standard error envelope.
/// - Several ids: prints one entry per id and returns the exit code of the
///   first failed id (`Success` if none failed). In JSON the envelope `data` is
///   an array of `{ id, ok: true, item }` / `{ id, ok: false, error }` entries;
///   `ok` is `true` only when every id succeeded, and a failed run also carries
///   a top-level `error`. In JSONL each entry is its own `{ v, ok, data }` line.
///   In human mode each success prints the item line on stdout and each
///   failure prints `error: <id>: <message>` on stderr.
pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    ids: &[String],
    status: ItemStatus,
    quiet: bool,
) -> Result<ExitCode, CloveError> {
    if let [id] = ids {
        let saved = transition_one(ctx, id, status)?;
        warn_open_dependents(
            ctx,
            status,
            quiet,
            std::slice::from_ref(&saved.frontmatter.id),
        );
        print_item(format, &saved, Map::new());
        return Ok(ExitCode::Success);
    }

    let results: Vec<(&String, Result<Item, CloveError>)> = ids
        .iter()
        .map(|id| (id, transition_one(ctx, id, status)))
        .collect();

    let closed: Vec<CloveId> = results
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok().map(|item| item.frontmatter.id.clone()))
        .collect();
    warn_open_dependents(ctx, status, quiet, &closed);

    let first_failure = results
        .iter()
        .find_map(|(_, r)| r.as_ref().err().map(|e| classify(e).0));
    let failed = results.iter().filter(|(_, r)| r.is_err()).count();

    match format {
        OutputFormat::Json => {
            let entries: Vec<Value> = results.iter().map(|(id, r)| entry(id, r)).collect();
            let mut envelope = json!({
                "v": ENVELOPE_VERSION,
                "ok": failed == 0,
                "data": entries,
                "_meta": { "warnings": [] },
            });
            if let Some((_, Err(first))) = results.iter().find(|(_, r)| r.is_err()) {
                let (exit, code) = classify(first);
                envelope["error"] = json!({
                    "code": code,
                    "message": format!("{failed} of {} ids failed", results.len()),
                    "exit": exit.code(),
                });
            }
            outln!("{envelope}");
        }
        OutputFormat::Jsonl => {
            for (id, r) in &results {
                let line = json!({
                    "v": ENVELOPE_VERSION,
                    "ok": r.is_ok(),
                    "data": entry(id, r),
                });
                outln!("{line}");
            }
        }
        OutputFormat::Human => {
            for (id, r) in &results {
                match r {
                    Ok(item) => print_human(item),
                    Err(e) if !quiet => eprintln!("error: {id}: {e}"),
                    Err(_) => {}
                }
            }
        }
    }

    Ok(first_failure.unwrap_or(ExitCode::Success))
}

/// Transition one id through the unified write path ([`ops::transition_item`]).
fn transition_one(ctx: &Ctx, raw: &str, status: ItemStatus) -> Result<Item, CloveError> {
    let id = parse_id(raw)?;
    ops::transition_item(&ctx.store, &id, status, now_millis())
}

/// One per-id result entry: `{ id, ok, item }` or `{ id, ok, error }`.
fn entry(id: &str, result: &Result<Item, CloveError>) -> Value {
    match result {
        Ok(item) => json!({ "id": id, "ok": true, "item": Value::Object(item_object(item)) }),
        Err(e) => {
            let (exit, code) = classify(e);
            json!({
                "id": id,
                "ok": false,
                "error": { "code": code, "message": e.to_string(), "exit": exit.code() },
            })
        }
    }
}

/// Closing an item that others depend on is allowed, but warned about (stderr,
/// before the item output). This is best-effort and reads only *other* items'
/// `deps`, so it runs after the writes, outside the store lock, with a single
/// scan however many ids were closed.
fn warn_open_dependents(ctx: &Ctx, status: ItemStatus, quiet: bool, ids: &[CloveId]) {
    if status != ItemStatus::Closed || quiet || ids.is_empty() {
        return;
    }
    let (frontmatters, _errors) = ctx.store.scan_frontmatter().unwrap_or_default();
    for id in ids {
        let dependents: Vec<String> = frontmatters
            .iter()
            .filter(|fm| fm.status != ItemStatus::Closed && fm.deps.contains(id))
            .map(|fm| fm.id.to_string())
            .collect();
        if !dependents.is_empty() {
            eprintln!(
                "warning: {} still has open dependents: {}",
                id.as_str(),
                dependents.join(", ")
            );
        }
    }
}
