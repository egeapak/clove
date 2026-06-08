//! M1 acceptance gate: the index read path returns the *same* items in the
//! *same* order as the file/graph path. The plan asks for a property test over
//! arbitrary corpora — we drive several deterministic seeds (and corpus sizes)
//! through the shared fixture generator and assert id-sequence equality for the
//! `ls`, `ready`, and a filtered query, against an independent file-derived
//! oracle built with `clove_core::GraphStore`.

use std::collections::HashMap;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::fixtures::write_fixtures;
use clove_core::{parse_frontmatter_file, GraphStore};
use clove_index::{reindex, Filter, Index, QueryMode};
use clove_types::{CloveId, ItemFrontmatter, ItemType};
use tempfile::TempDir;

/// Parse every `<id>.md` under `issues` into frontmatter (the oracle input).
fn read_frontmatters(issues: &Utf8Path) -> Vec<ItemFrontmatter> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(issues).unwrap() {
        let path = Utf8PathBuf::from_path_buf(entry.unwrap().path()).unwrap();
        if path.extension() == Some("md") {
            out.push(parse_frontmatter_file(&path).unwrap());
        }
    }
    out
}

/// The canonical list order shared by both paths: (priority, topo rank with
/// missing-rank-last, id).
fn sort_ids(mut fms: Vec<ItemFrontmatter>, ranks: &HashMap<CloveId, usize>) -> Vec<String> {
    fms.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| {
                let ra = ranks.get(&a.id).copied().unwrap_or(usize::MAX);
                let rb = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                ra.cmp(&rb)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    fms.into_iter().map(|fm| fm.id.to_string()).collect()
}

fn index_ids(index: &Index, filter: &Filter) -> Vec<String> {
    index
        .query_items(filter)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect()
}

fn setup(seed: u64, count: usize) -> (TempDir, Utf8PathBuf, Index) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, count, seed).unwrap();
    let db = root.join(".clove").join("index.db");
    reindex(&issues, &db).unwrap();
    let index = Index::open(&db).unwrap();
    (tmp, issues, index)
}

#[test]
fn index_ls_ready_and_filter_match_file_oracle() {
    for &(seed, count) in &[
        (1u64, 120usize),
        (7, 200),
        (42, 75),
        (2026, 300),
        (99991, 1),
    ] {
        let (_tmp, issues, index) = setup(seed, count);

        let fms = read_frontmatters(&issues);
        let (graph, _dangling) = GraphStore::build(&fms);
        let ranks = graph.topological_ranks();

        // ls: every item, identical order.
        let oracle_ls = sort_ids(fms.clone(), &ranks);
        let index_ls = index_ids(&index, &Filter::default());
        assert_eq!(
            index_ls, oracle_ls,
            "ls mismatch (seed {seed}, count {count})"
        );

        // ready: same set + order as GraphStore::ready_items().
        let oracle_ready: Vec<String> = graph
            .ready_items()
            .iter()
            .map(|id| id.to_string())
            .collect();
        let index_ready = index_ids(
            &index,
            &Filter {
                mode: QueryMode::Ready,
                ..Default::default()
            },
        );
        assert_eq!(
            index_ready, oracle_ready,
            "ready mismatch (seed {seed}, count {count})"
        );

        // filtered: type == bug.
        let oracle_bugs = sort_ids(
            fms.iter()
                .filter(|fm| fm.item_type == ItemType::Bug)
                .cloned()
                .collect(),
            &ranks,
        );
        let index_bugs = index_ids(
            &index,
            &Filter {
                item_type: Some(ItemType::Bug),
                ..Default::default()
            },
        );
        assert_eq!(
            index_bugs, oracle_bugs,
            "type filter mismatch (seed {seed}, count {count})"
        );
    }
}
