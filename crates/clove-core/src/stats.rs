//! Work-item analytics (`clove stats`, M4).
//!
//! [`StatsReport`] is the aggregate view of a store: counts by status / type /
//! priority / assignee / label, ready / blocked totals, dependency-cycle count,
//! epic completion rollups, and created/closed throughput over rolling windows.
//!
//! This module is pure (no SQLite, no clock): [`compute`] takes the already-scanned
//! frontmatters, the built [`GraphStore`], and an explicit `now` reference, and
//! returns a fully serializable report. Persistence (the optional `.clove/stats.db`
//! history store) and rendering live above it, in `clove-index` and the CLI.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::graph::GraphStore;
use crate::model::{ItemFrontmatter, ItemStatus, ItemType, Priority};

/// Knobs for [`compute`].
#[derive(Debug, Clone, Copy)]
pub struct StatsOptions {
    /// Cap on the `by_assignee` / `by_label` breakdowns (the highest-count keys).
    pub top: usize,
    /// Whether to compute the per-epic completion rollup (can be skipped on very
    /// large stores where it is not wanted).
    pub include_epics: bool,
}

impl Default for StatsOptions {
    fn default() -> Self {
        StatsOptions {
            top: 10,
            include_epics: true,
        }
    }
}

/// Counts of items in each lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusCounts {
    pub open: u64,
    pub in_progress: u64,
    pub closed: u64,
}

/// Counts of items of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TypeCounts {
    pub bug: u64,
    pub feature: u64,
    pub chore: u64,
    pub docs: u64,
    pub epic: u64,
}

/// A single `key → count` row in a breakdown (assignees, labels). Ordered by
/// descending count, then key, so a `--top N` slice is the N most common.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCount {
    pub key: String,
    pub count: u64,
}

/// Per-epic completion roll-up over its direct children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicRollup {
    pub id: String,
    pub title: String,
    pub total: u32,
    pub closed: u32,
    /// Percent complete (`closed / total`), 0 when the epic has no children.
    pub pct: u8,
    /// True when every child is closed (and there is at least one).
    pub completable: bool,
}

/// Created/closed counts over rolling windows and all-time. Lets a snapshot
/// series show throughput trends without re-deriving them from raw timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Throughput {
    pub created_7d: u64,
    pub closed_7d: u64,
    pub created_30d: u64,
    pub closed_30d: u64,
    pub created_total: u64,
    pub closed_total: u64,
}

/// The complete analytics view of a store at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsReport {
    /// Total number of items.
    pub total: u64,
    pub by_status: StatusCounts,
    pub by_type: TypeCounts,
    /// Counts for priorities 0..=4 (index 0 = highest).
    pub by_priority: [u64; 5],
    /// Items with no assignee.
    pub unassigned: u64,
    /// Top assignees by item count (capped by [`StatsOptions::top`]).
    pub by_assignee: Vec<KeyCount>,
    /// Top labels by item count (capped by [`StatsOptions::top`]).
    pub by_label: Vec<KeyCount>,
    /// Active items ready to work on (all hard deps closed, no dangling).
    pub ready: u64,
    /// Active items blocked by an open or dangling hard dependency.
    pub blocked: u64,
    /// Active items excluded from ready/blocked (in a cycle or malformed parent).
    pub excluded: u64,
    /// Distinct dangling hard-dependency references across the store.
    pub dangling: u64,
    /// Number of hard-dependency cycles.
    pub cycles: u64,
    /// Per-epic completion roll-ups (empty when [`StatsOptions::include_epics`]
    /// is false), ordered by id.
    pub epics: Vec<EpicRollup>,
    pub throughput: Throughput,
}

/// Compute the full analytics report from a scanned store.
///
/// `frontmatters` is the parsed item set, `graph` the dependency graph built from
/// it, and `now` the reference instant for the throughput windows. The two inputs
/// must describe the same set of items.
pub fn compute(
    frontmatters: &[ItemFrontmatter],
    graph: &GraphStore,
    now: DateTime<Utc>,
    opts: StatsOptions,
) -> StatsReport {
    let mut by_status = StatusCounts::default();
    let mut by_type = TypeCounts::default();
    let mut by_priority = [0u64; 5];
    let mut unassigned = 0u64;
    let mut assignee_counts: HashMap<&str, u64> = HashMap::new();
    let mut label_counts: HashMap<&str, u64> = HashMap::new();
    let mut throughput = Throughput::default();

    let window_7d = now - Duration::days(7);
    let window_30d = now - Duration::days(30);

    for fm in frontmatters {
        match fm.status {
            ItemStatus::Open => by_status.open += 1,
            ItemStatus::InProgress => by_status.in_progress += 1,
            ItemStatus::Closed => by_status.closed += 1,
        }
        match fm.item_type {
            ItemType::Bug => by_type.bug += 1,
            ItemType::Feature => by_type.feature += 1,
            ItemType::Chore => by_type.chore += 1,
            ItemType::Docs => by_type.docs += 1,
            ItemType::Epic => by_type.epic += 1,
        }
        // Out-of-range priorities are representable (validate/doctor report them);
        // bucket them under the nearest valid slot so the array stays bounded.
        let p = fm.priority.get().min(Priority::MAX) as usize;
        by_priority[p] += 1;

        match fm.assignee.as_deref() {
            Some(who) if !who.is_empty() => *assignee_counts.entry(who).or_insert(0) += 1,
            _ => unassigned += 1,
        }
        for label in &fm.labels {
            *label_counts.entry(label.as_str()).or_insert(0) += 1;
        }

        throughput.created_total += 1;
        if fm.created >= window_7d {
            throughput.created_7d += 1;
        }
        if fm.created >= window_30d {
            throughput.created_30d += 1;
        }
        if let Some(closed) = fm.closed {
            throughput.closed_total += 1;
            if closed >= window_7d {
                throughput.closed_7d += 1;
            }
            if closed >= window_30d {
                throughput.closed_30d += 1;
            }
        }
    }

    let by_assignee = top_counts(&assignee_counts, opts.top);
    let by_label = top_counts(&label_counts, opts.top);

    let ready = graph.ready_items().len() as u64;
    let blocked_items = graph.blocked_items();
    let blocked = blocked_items.len() as u64;
    let excluded = graph.excluded_items().len() as u64;
    let dangling = graph.dangling_ids().len() as u64;
    let cycles = graph.all_cycles().len() as u64;

    let epics = if opts.include_epics {
        epic_rollups(frontmatters, graph)
    } else {
        Vec::new()
    };

    StatsReport {
        total: frontmatters.len() as u64,
        by_status,
        by_type,
        by_priority,
        unassigned,
        by_assignee,
        by_label,
        ready,
        blocked,
        excluded,
        dangling,
        cycles,
        epics,
        throughput,
    }
}

/// Order a count map by descending count (ties broken by key) and keep the top
/// `n` (all when `n == 0`).
fn top_counts(counts: &HashMap<&str, u64>, n: usize) -> Vec<KeyCount> {
    let mut rows: Vec<KeyCount> = counts
        .iter()
        .map(|(&key, &count)| KeyCount {
            key: key.to_owned(),
            count,
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    if n > 0 && rows.len() > n {
        rows.truncate(n);
    }
    rows
}

/// Build the per-epic completion roll-up, ordered by epic id.
fn epic_rollups(frontmatters: &[ItemFrontmatter], graph: &GraphStore) -> Vec<EpicRollup> {
    let mut epics: Vec<EpicRollup> = frontmatters
        .iter()
        .filter(|fm| fm.item_type == ItemType::Epic)
        .filter_map(|fm| {
            let summary = graph.epic_children_summary(&fm.id)?;
            let pct = if summary.total > 0 {
                ((summary.closed as u64 * 100) / summary.total as u64) as u8
            } else {
                0
            };
            Some(EpicRollup {
                id: fm.id.to_string(),
                title: fm.title.clone(),
                total: summary.total,
                closed: summary.closed,
                pct,
                completable: summary.completable,
            })
        })
        .collect();
    epics.sort_by(|a, b| a.id.cmp(&b.id));
    epics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::CloveId;

    fn fm(
        id: &str,
        status: ItemStatus,
        item_type: ItemType,
        priority: u8,
        created: &str,
    ) -> ItemFrontmatter {
        ItemFrontmatter {
            schema: 1,
            id: CloveId::new(id).unwrap(),
            title: id.to_owned(),
            status,
            item_type,
            priority: Priority(priority),
            created: created.parse().unwrap(),
            updated: created.parse().unwrap(),
            closed: None,
            assignee: None,
            parent: None,
            labels: Vec::new(),
            deps: Vec::new(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-06-04T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn counts_by_dimension() {
        let items = vec![
            fm(
                "proj-AAAAAAAA",
                ItemStatus::Open,
                ItemType::Bug,
                0,
                "2026-06-01T00:00:00Z",
            ),
            fm(
                "proj-BBBBBBBB",
                ItemStatus::InProgress,
                ItemType::Feature,
                2,
                "2026-06-01T00:00:00Z",
            ),
            fm(
                "proj-CCCCCCCC",
                ItemStatus::Closed,
                ItemType::Feature,
                2,
                "2026-06-01T00:00:00Z",
            ),
        ];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());

        assert_eq!(report.total, 3);
        assert_eq!(report.by_status.open, 1);
        assert_eq!(report.by_status.in_progress, 1);
        assert_eq!(report.by_status.closed, 1);
        assert_eq!(report.by_type.feature, 2);
        assert_eq!(report.by_type.bug, 1);
        assert_eq!(report.by_priority[0], 1);
        assert_eq!(report.by_priority[2], 2);
        assert_eq!(report.unassigned, 3);
    }

    #[test]
    fn ready_blocked_and_cycles() {
        // a (closed) <- b (ready); c (open) <- d (blocked).
        let mut a = fm(
            "proj-AAAAAAAA",
            ItemStatus::Closed,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        a.closed = Some("2026-06-02T00:00:00Z".parse().unwrap());
        let mut b = fm(
            "proj-BBBBBBBB",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        b.deps = vec![CloveId::new("proj-AAAAAAAA").unwrap()];
        let c = fm(
            "proj-CCCCCCCC",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        let mut d = fm(
            "proj-DDDDDDDD",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        d.deps = vec![CloveId::new("proj-CCCCCCCC").unwrap()];

        let items = vec![a, b, c, d];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());

        // b and c are ready (c has no deps); d is blocked.
        assert_eq!(report.ready, 2, "{report:?}");
        assert_eq!(report.blocked, 1, "{report:?}");
        assert_eq!(report.cycles, 0);
    }

    #[test]
    fn throughput_windows() {
        let mut recent = fm(
            "proj-AAAAAAAA",
            ItemStatus::Closed,
            ItemType::Bug,
            2,
            "2026-06-03T00:00:00Z",
        );
        recent.closed = Some("2026-06-03T12:00:00Z".parse().unwrap());
        let old = fm(
            "proj-BBBBBBBB",
            ItemStatus::Open,
            ItemType::Bug,
            2,
            "2026-01-01T00:00:00Z",
        );

        let items = vec![recent, old];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());

        assert_eq!(report.throughput.created_total, 2);
        assert_eq!(report.throughput.created_7d, 1);
        assert_eq!(report.throughput.created_30d, 1);
        assert_eq!(report.throughput.closed_total, 1);
        assert_eq!(report.throughput.closed_7d, 1);
    }

    #[test]
    fn epic_rollup_pct() {
        let epic = fm(
            "proj-EEEEEEEE",
            ItemStatus::Open,
            ItemType::Epic,
            2,
            "2026-06-01T00:00:00Z",
        );
        let mut child_closed = fm(
            "proj-CCCCCCCC",
            ItemStatus::Closed,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        child_closed.parent = Some(CloveId::new("proj-EEEEEEEE").unwrap());
        let mut child_open = fm(
            "proj-DDDDDDDD",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        child_open.parent = Some(CloveId::new("proj-EEEEEEEE").unwrap());

        let items = vec![epic, child_closed, child_open];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());

        assert_eq!(report.epics.len(), 1);
        assert_eq!(report.epics[0].total, 2);
        assert_eq!(report.epics[0].closed, 1);
        assert_eq!(report.epics[0].pct, 50);
        assert!(!report.epics[0].completable);
    }

    #[test]
    fn top_counts_orders_and_caps() {
        let mut items = Vec::new();
        for (i, who) in ["alice", "alice", "alice", "bob", "bob", "carol"]
            .iter()
            .enumerate()
        {
            let mut it = fm(
                &format!("proj-{:08X}", i),
                ItemStatus::Open,
                ItemType::Bug,
                2,
                "2026-06-01T00:00:00Z",
            );
            it.assignee = Some((*who).to_owned());
            items.push(it);
        }
        let (graph, _) = GraphStore::build(&items);
        let report = compute(
            &items,
            &graph,
            now(),
            StatsOptions {
                top: 2,
                include_epics: true,
            },
        );

        assert_eq!(report.by_assignee.len(), 2);
        assert_eq!(report.by_assignee[0].key, "alice");
        assert_eq!(report.by_assignee[0].count, 3);
        assert_eq!(report.by_assignee[1].key, "bob");
        assert_eq!(report.unassigned, 0);
    }

    #[test]
    fn counts_dangling_excluded_and_cycles() {
        // R: ready. D: depends on a missing id (dangling). A↔B: a hard cycle.
        let r = fm(
            "proj-RRRRRRRR",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        let mut d = fm(
            "proj-DDDDDDDD",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        d.deps = vec![CloveId::new("proj-MISSING0").unwrap()];
        let mut a = fm(
            "proj-AAAAAAAA",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        a.deps = vec![CloveId::new("proj-BBBBBBBB").unwrap()];
        let mut b = fm(
            "proj-BBBBBBBB",
            ItemStatus::Open,
            ItemType::Feature,
            2,
            "2026-06-01T00:00:00Z",
        );
        b.deps = vec![CloveId::new("proj-AAAAAAAA").unwrap()];

        let items = vec![r, d, a, b];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());

        assert_eq!(report.dangling, 1, "one missing referenced id: {report:?}");
        assert_eq!(report.cycles, 1, "one hard cycle: {report:?}");
        assert_eq!(report.excluded, 2, "A and B are cycle members: {report:?}");
        assert_eq!(report.ready, 1, "only R is ready: {report:?}");
        assert_eq!(
            report.blocked, 1,
            "only D is blocked (dangling): {report:?}"
        );
    }

    #[test]
    fn report_round_trips_through_json() {
        let items = vec![fm(
            "proj-AAAAAAAA",
            ItemStatus::Open,
            ItemType::Bug,
            2,
            "2026-06-01T00:00:00Z",
        )];
        let (graph, _) = GraphStore::build(&items);
        let report = compute(&items, &graph, now(), StatsOptions::default());
        let json = serde_json::to_string(&report).unwrap();
        let back: StatsReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
