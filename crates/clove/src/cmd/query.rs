//! `clove query` (T-CLI11): list via a JSON filter (flag or stdin).
//!
//! Same read path as `clove ls` — the JSON filter is just a second spelling of
//! the same `view::Filters` — so both are thin adapters over
//! [`clove_engine::Engine`].

use std::io::{IsTerminal, Read};

use clove_core::OutputFormat;
use clove_engine::Projection;
use clove_types::CloveError;
use serde::Deserialize;

use crate::cli::QueryArgs;
use crate::cmd::listing::{emit, lean_can_serve, objects_from_answer, window, Filters, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

/// One filter value or several — `"open"` and `["open","in_progress"]` are both
/// accepted, so every filter JSON written before multi-value existed still
/// parses to the same thing.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The values as a list; `None` (the field was absent) is unconstrained.
    fn values(this: &Option<OneOrMany>) -> Vec<String> {
        match this {
            None => Vec::new(),
            Some(OneOrMany::One(v)) => vec![v.clone()],
            Some(OneOrMany::Many(v)) => v.clone(),
        }
    }
}

/// One priority or several — `2` and `[0,1]` are both accepted.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Priorities {
    One(u8),
    Many(Vec<u8>),
}

impl Priorities {
    fn values(this: &Option<Priorities>) -> Vec<String> {
        match this {
            None => Vec::new(),
            Some(Priorities::One(p)) => vec![p.to_string()],
            Some(Priorities::Many(v)) => v.iter().map(u8::to_string).collect(),
        }
    }
}

/// The JSON filter object accepted on `--filter` or stdin.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct QueryFilter {
    /// `"open"` or `["open","in_progress"]` — any of them.
    status: Option<OneOrMany>,
    /// `"bug"` or `["bug","chore"]` — any of them.
    #[serde(rename = "type")]
    item_type: Option<OneOrMany>,
    /// `"area:core"` or `["area:core","area:ios"]` — **all** of them.
    label: Option<OneOrMany>,
    assignee: Option<String>,
    /// `2` or `[0,1]` — any of them. Stays numeric (it was `Option<u8>`), so a
    /// filter written before multi-value parses byte for byte as it did.
    priority: Option<Priorities>,
    /// Substring over id/title/labels, matching `--q`.
    q: Option<String>,
    /// `rank|priority|created|updated|id|status|type`, matching `--sort`.
    sort: Option<String>,
    /// Reverse the order, matching `--desc`.
    desc: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: QueryArgs,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    let raw = match args.filter {
        Some(text) => text,
        None => read_stdin_filter()?,
    };
    let qf: QueryFilter = if raw.trim().is_empty() {
        QueryFilter::default()
    } else {
        serde_json::from_str(&raw).map_err(|e| CloveError::InvalidField {
            field: "filter".to_owned(),
            reason: format!("invalid JSON filter: {e}"),
        })?
    };

    // The JSON filter and the flags describe the same filter set, so both go
    // through `Filters::parse_multi` rather than each growing its own decoding.
    let filters = Filters::parse_multi(
        &OneOrMany::values(&qf.status),
        &OneOrMany::values(&qf.item_type),
        &OneOrMany::values(&qf.label),
        qf.assignee.as_deref(),
        &Priorities::values(&qf.priority),
        qf.q.as_deref(),
    )?;

    // The flag wins over the JSON filter, exactly as `--limit`/`--offset` do.
    let order = crate::cli::order_of(
        args.sort.as_deref().or(qf.sort.as_deref()),
        args.desc || qf.desc.unwrap_or(false),
    )?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset.or(qf.offset), args.limit.or(qf.limit));

    let projection = match lean_can_serve(fields.as_deref()) {
        true => Projection::Lean,
        false => Projection::Files,
    };
    let answer = ctx
        .engine(no_index, deep)
        .list(&filters, order, window, projection)?;

    emit(
        format,
        objects_from_answer(&answer),
        ListOpts {
            total: answer.total,
            window,
            fields: fields.as_deref(),
            compact: args.compact,
            source: answer.source.as_str(),
            sort: order.field.as_str(),
            dir: order.dir_str(),
            filters: Some(&filters),
            warnings: answer.warnings,
        },
    );
    Ok(())
}

/// Read a JSON filter from stdin when it is piped; an interactive TTY yields no
/// filter (everything matches).
fn read_stdin_filter() -> Result<String, CloveError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(String::new());
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_to_string(&mut buf)
        .map_err(|source| CloveError::Io {
            path: camino::Utf8PathBuf::from("<stdin>"),
            source,
        })?;
    Ok(buf)
}
