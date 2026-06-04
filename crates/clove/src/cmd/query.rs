//! `clove query` (T-CLI11): list via a JSON filter (flag or stdin).

use std::io::{IsTerminal, Read};

use clove_core::{CloveError, OutputFormat};
use clove_index::QueryMode;
use serde::Deserialize;

use crate::cli::QueryArgs;
use crate::cmd::index_read::{list_via_daemon, list_via_index};
use crate::cmd::listing::{
    effective_limit, emit, objects_from_frontmatters, objects_from_lean_rows, ranks_of,
    sort_by_priority_topo, Filters, ListOpts,
};
use crate::context::Ctx;
use crate::item_json::parse_fields;

/// The JSON filter object accepted on `--filter` or stdin.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct QueryFilter {
    status: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    label: Option<String>,
    assignee: Option<String>,
    priority: Option<u8>,
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

    let filters = Filters::parse(
        qf.status.as_deref(),
        qf.item_type.as_deref(),
        qf.label.as_deref(),
        qf.assignee.as_deref(),
        qf.priority,
    )?;

    let fields = args.fields.as_deref().map(parse_fields);
    let offset = args.offset.or(qf.offset).unwrap_or(0);
    let limit = effective_limit(args.limit.or(qf.limit));

    if let Some((objects, total, warnings)) =
        list_via_daemon(ctx, no_index, QueryMode::List, &filters, offset, limit)
    {
        emit(
            format,
            objects,
            ListOpts {
                total,
                offset,
                limit,
                fields: fields.as_deref(),
                source: "daemon",
                warnings,
            },
        );
        return Ok(());
    }

    if let Some((rows, total, warnings)) = list_via_index(
        ctx,
        no_index,
        deep,
        QueryMode::List,
        &filters,
        offset,
        limit,
    )? {
        emit(
            format,
            objects_from_lean_rows(&rows),
            ListOpts {
                total,
                offset,
                limit,
                fields: fields.as_deref(),
                source: "index",
                warnings,
            },
        );
        return Ok(());
    }

    let (mut frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (_graph, ranks) = ranks_of(&frontmatters);
    frontmatters.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut frontmatters, &ranks);

    let objects = objects_from_frontmatters(&frontmatters);
    let total = objects.len();
    emit(
        format,
        objects,
        ListOpts {
            total,
            offset,
            limit,
            fields: fields.as_deref(),
            source: "files",
            warnings: Vec::new(),
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
