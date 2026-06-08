//! `clove stats` (M4): work-item analytics.
//!
//! Aggregates the store into a single report — counts by status / type /
//! priority / assignee / label, ready / blocked totals, dependency-cycle count,
//! epic completion rollups, and created/closed throughput — alongside daemon and
//! index telemetry. Analytics are computed from a single file scan + graph build
//! (files are always truth); the index/daemon are reported, not relied on.
//!
//! Snapshots persist to a `snapshots` table **inside the index database**
//! (`.clove/index.db`) so trends can be replayed with `--history`. The index
//! layer carries that table across its reindex/rebuild so history survives a
//! schema bump or `clove reindex` (only true file corruption loses it).

use chrono::Utc;
use clove_core::{compute_stats, GraphStore, OutputFormat, StatsOptions, StatsReport};
use clove_index::{Index, SCHEMA_VERSION};
use clove_ipc::DaemonClient;
use clove_types::CloveError;
use serde_json::{json, Map, Value};

use crate::cli::StatsArgs;
use crate::context::{index_error, Ctx};
use crate::output::{print_json_list, print_json_success};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: StatsArgs,
    no_index: bool,
) -> Result<(), CloveError> {
    if args.history {
        return show_history(ctx, format, &args);
    }

    let opts = StatsOptions {
        top: args.top.unwrap_or(10),
        include_epics: !args.no_epics,
    };

    // Compute analytics from the files (the single source of truth).
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let now = Utc::now();
    let report = compute_stats(&frontmatters, &graph, now, opts);

    // Optionally persist the snapshot into the index database's history table.
    if args.snapshot {
        let index =
            Index::open_or_create(&ctx.db_path).map_err(|e| index_error(e, &ctx.db_path))?;
        index
            .record_snapshot(now, &report)
            .map_err(|e| index_error(e, &ctx.db_path))?;
    }

    let daemon = daemon_telemetry(ctx, no_index);
    let index = index_telemetry(ctx, no_index);

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data = report_value(&report, &daemon, &index);
            print_json_success(
                data,
                json!({
                    "source": "files",
                    "generated_at": now.to_rfc3339(),
                    "snapshotted": args.snapshot,
                }),
            );
        }
        OutputFormat::Human => render_human(&report, &daemon, &index, args.snapshot),
    }
    Ok(())
}

/// Print the persisted snapshot series (`--history`).
fn show_history(ctx: &Ctx, format: OutputFormat, args: &StatsArgs) -> Result<(), CloveError> {
    if !ctx.db_path.exists() {
        // No index yet → no history. Empty series rather than an error.
        match format {
            OutputFormat::Json | OutputFormat::Jsonl => {
                print_json_list(Vec::new(), json!({ "total": 0, "source": "index" }))
            }
            OutputFormat::Human => {
                println!("no stats snapshots recorded yet (run `clove stats --snapshot`)")
            }
        }
        return Ok(());
    }

    let index = Index::open_or_create(&ctx.db_path).map_err(|e| index_error(e, &ctx.db_path))?;
    let snapshots = index
        .snapshot_history(args.since.as_deref(), args.limit)
        .map_err(|e| index_error(e, &ctx.db_path))?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let items: Vec<Value> = snapshots
                .iter()
                .map(|s| {
                    json!({
                        "captured_at": s.captured_at,
                        "stats": serde_json::to_value(&s.report).unwrap_or(Value::Null),
                    })
                })
                .collect();
            let total = items.len();
            print_json_list(items, json!({ "total": total, "source": "index" }));
        }
        OutputFormat::Human => {
            if snapshots.is_empty() {
                println!("no stats snapshots in range");
            } else {
                println!(
                    "captured_at              total  open  in_prog  closed  ready  blocked  cycles"
                );
                for s in &snapshots {
                    let r = &s.report;
                    println!(
                        "{:24} {:5}  {:4}  {:7}  {:6}  {:5}  {:7}  {:6}",
                        s.captured_at,
                        r.total,
                        r.by_status.open,
                        r.by_status.in_progress,
                        r.by_status.closed,
                        r.ready,
                        r.blocked,
                        r.cycles,
                    );
                }
            }
        }
    }
    Ok(())
}

/// Probe a running daemon for its operational telemetry (non-fatal).
fn daemon_telemetry(ctx: &Ctx, no_index: bool) -> Value {
    if no_index {
        return json!({ "running": false });
    }
    let Some(clove_dir) = ctx.issues_dir.parent() else {
        return json!({ "running": false });
    };
    let Some(mut client) = DaemonClient::probe(clove_dir) else {
        return json!({ "running": false });
    };
    match client.status() {
        Ok(s) => json!({
            "running": true,
            "uptime_s": s.uptime_s,
            "items_indexed": s.items_indexed,
            "watcher_state": s.watcher_state,
            "last_event_ms": s.last_event_ms,
            "batches_applied": s.batches_applied,
        }),
        Err(_) => json!({ "running": false }),
    }
}

/// Report the local index's presence and freshness (non-fatal).
fn index_telemetry(ctx: &Ctx, no_index: bool) -> Value {
    if no_index || !ctx.db_path.exists() {
        return json!({ "present": false });
    }
    let Ok(index) = Index::open_or_create(&ctx.db_path) else {
        return json!({ "present": false });
    };
    let items = index.item_count().unwrap_or(0);
    // A best-effort, side-effect-free freshness check.
    let stale = match index.check_staleness_fast(&ctx.issues_dir) {
        Ok(report) => !report.is_clean(),
        Err(_) => false,
    };
    json!({
        "present": true,
        "items_indexed": items,
        "schema_version": SCHEMA_VERSION,
        "stale": stale,
    })
}

/// Serialize the report into a JSON object and attach daemon/index telemetry.
fn report_value(report: &StatsReport, daemon: &Value, index: &Value) -> Value {
    let mut obj: Map<String, Value> = match serde_json::to_value(report) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    obj.insert("daemon".to_owned(), daemon.clone());
    obj.insert("index".to_owned(), index.clone());
    Value::Object(obj)
}

/// Render the report as a sectioned human-readable summary.
fn render_human(report: &StatsReport, daemon: &Value, index: &Value, snapshotted: bool) {
    println!("clove stats — {} items\n", report.total);

    println!(
        "Status     open {}  in_progress {}  closed {}",
        report.by_status.open, report.by_status.in_progress, report.by_status.closed
    );
    println!(
        "Type       bug {}  feature {}  chore {}  docs {}  epic {}",
        report.by_type.bug,
        report.by_type.feature,
        report.by_type.chore,
        report.by_type.docs,
        report.by_type.epic
    );
    println!(
        "Priority   p0 {}  p1 {}  p2 {}  p3 {}  p4 {}",
        report.by_priority[0],
        report.by_priority[1],
        report.by_priority[2],
        report.by_priority[3],
        report.by_priority[4]
    );
    println!(
        "Workflow   ready {}  blocked {}  excluded {}  dangling {}  cycles {}",
        report.ready, report.blocked, report.excluded, report.dangling, report.cycles
    );

    if !report.by_assignee.is_empty() || report.unassigned > 0 {
        let mut parts: Vec<String> = report
            .by_assignee
            .iter()
            .map(|kc| format!("{} {}", kc.key, kc.count))
            .collect();
        parts.push(format!("unassigned {}", report.unassigned));
        println!("Assignees  {}", parts.join("  "));
    }
    if !report.by_label.is_empty() {
        let parts: Vec<String> = report
            .by_label
            .iter()
            .map(|kc| format!("{} {}", kc.key, kc.count))
            .collect();
        println!("Labels     {}", parts.join("  "));
    }

    let t = &report.throughput;
    println!(
        "Throughput created 7d {} / 30d {} / all {}    closed 7d {} / 30d {} / all {}",
        t.created_7d, t.created_30d, t.created_total, t.closed_7d, t.closed_30d, t.closed_total
    );

    if !report.epics.is_empty() {
        println!("\nEpics");
        for e in &report.epics {
            let mark = if e.completable { " ✓" } else { "" };
            println!(
                "  {:14} {:<24} {}/{}  {}%{}",
                e.id, e.title, e.closed, e.total, e.pct, mark
            );
        }
    }

    println!();
    if daemon.get("running").and_then(Value::as_bool) == Some(true) {
        println!(
            "Daemon     running  uptime {}s  items {}  watcher {}",
            daemon.get("uptime_s").and_then(Value::as_u64).unwrap_or(0),
            daemon
                .get("items_indexed")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            daemon
                .get("watcher_state")
                .and_then(Value::as_str)
                .unwrap_or("?"),
        );
    } else {
        println!("Daemon     not running");
    }
    if index.get("present").and_then(Value::as_bool) == Some(true) {
        let fresh = if index.get("stale").and_then(Value::as_bool) == Some(true) {
            "stale"
        } else {
            "fresh"
        };
        println!(
            "Index      present  items {}  schema {}  {}",
            index
                .get("items_indexed")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            index
                .get("schema_version")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            fresh,
        );
    } else {
        println!("Index      not present");
    }

    if snapshotted {
        println!("\nsnapshot recorded to the index history (.clove/index.db)");
    }
}
