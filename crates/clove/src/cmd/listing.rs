//! Shared filtering, ordering, pagination, and rendering for the list commands
//! (`ls`, `ready`, `blocked`, `query`).

use std::collections::HashMap;

use clove_core::{
    normalize_label, CloveError, CloveId, GraphStore, ItemFrontmatter, ItemStatus, ItemType,
    OutputFormat, Priority,
};
use serde_json::{json, Value};

use crate::item_json::{frontmatter_object, project};
use crate::output::{print_json_list, print_jsonl_items};
use crate::util::{parse_status, parse_type};

/// Parsed list filters. A `None` field does not constrain.
#[derive(Debug, Default, Clone)]
pub struct Filters {
    pub status: Option<ItemStatus>,
    pub item_type: Option<ItemType>,
    pub label: Option<String>,
    pub assignee: Option<String>,
    pub priority: Option<Priority>,
}

impl Filters {
    /// Build filters from raw CLI strings, validating each.
    pub fn parse(
        status: Option<&str>,
        item_type: Option<&str>,
        label: Option<&str>,
        assignee: Option<&str>,
        priority: Option<u8>,
    ) -> Result<Filters, CloveError> {
        Ok(Filters {
            status: status.map(parse_status).transpose()?,
            item_type: item_type.map(parse_type).transpose()?,
            label: label.map(normalize_label).transpose()?,
            assignee: assignee.map(str::to_owned),
            priority: priority.map(Priority::new).transpose()?,
        })
    }

    /// Whether `fm` satisfies every set constraint.
    pub fn matches(&self, fm: &ItemFrontmatter) -> bool {
        if let Some(s) = self.status {
            if fm.status != s {
                return false;
            }
        }
        if let Some(t) = self.item_type {
            if fm.item_type != t {
                return false;
            }
        }
        if let Some(p) = self.priority {
            if fm.priority != p {
                return false;
            }
        }
        if let Some(a) = &self.assignee {
            if fm.assignee.as_deref() != Some(a.as_str()) {
                return false;
            }
        }
        if let Some(l) = &self.label {
            if !fm.labels.iter().any(|x| x == l) {
                return false;
            }
        }
        true
    }
}

/// Sort frontmatter in place by `(priority, topological_rank, id)` — the canonical
/// list order shared with the index path.
pub fn sort_by_priority_topo(items: &mut [ItemFrontmatter], ranks: &HashMap<CloveId, usize>) {
    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| {
                let ra = ranks.get(&a.id).copied().unwrap_or(usize::MAX);
                let rb = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                ra.cmp(&rb)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Pagination, projection, and metadata options for [`emit`].
#[derive(Debug, Default)]
pub struct ListOpts<'a> {
    /// Match count before pagination.
    pub total: usize,
    pub offset: usize,
    pub limit: Option<usize>,
    pub fields: Option<&'a [String]>,
    /// `"files"` or `"index"`.
    pub source: &'a str,
    pub warnings: Vec<String>,
}

/// Emit a list: apply offset/limit, project fields, and render in `format`.
pub fn emit(format: OutputFormat, ordered: &[ItemFrontmatter], opts: ListOpts<'_>) {
    let page: Vec<&ItemFrontmatter> = ordered
        .iter()
        .skip(opts.offset)
        .take(opts.limit.unwrap_or(usize::MAX))
        .collect();

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let values: Vec<Value> = page
                .iter()
                .map(|fm| {
                    let obj = frontmatter_object(fm);
                    let obj = match opts.fields {
                        Some(f) => project(obj, f),
                        None => obj,
                    };
                    Value::Object(obj)
                })
                .collect();
            if matches!(format, OutputFormat::Jsonl) {
                print_jsonl_items(&values);
            } else {
                print_json_list(
                    values,
                    json!({
                        "total": opts.total,
                        "returned": page.len(),
                        "offset": opts.offset,
                        "source": opts.source,
                        "warnings": opts.warnings,
                    }),
                );
            }
        }
        OutputFormat::Human => {
            for fm in &page {
                println!(
                    "{}  [{}] p{} {}  {}",
                    fm.id.as_str(),
                    fm.status.as_str(),
                    fm.priority.get(),
                    fm.item_type.as_str(),
                    fm.title
                );
            }
        }
    }
}

/// Build the dependency graph and its topological ranks from a frontmatter set.
pub fn ranks_of(frontmatters: &[ItemFrontmatter]) -> (GraphStore, HashMap<CloveId, usize>) {
    let (graph, _dangling) = GraphStore::build(frontmatters);
    let ranks = graph.topological_ranks();
    (graph, ranks)
}
