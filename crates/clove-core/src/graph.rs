//! The dependency-graph engine (DESIGN.md §5).
//!
//! A [`GraphStore`] is built from a slice of [`ItemFrontmatter`] (bodies are not
//! needed, keeping the `ls`/`ready`/`blocked` path body-free — §13.3). It backs
//! `ready`/`blocked` classification, cycle detection, dependency-tree rendering,
//! and epic roll-ups.
//!
//! Only `DependsOn` edges are "hard" (blocking). Soft relations
//! (`Relates`/`Duplicates`/`Supersedes`) and the `ParentOf` hierarchy never
//! affect readiness — enforced by routing every blocking computation through the
//! hard-dependency edge view (`is_hard_dep`).

use std::collections::{HashMap, HashSet};

use petgraph::algo::{has_path_connecting, kosaraju_scc, toposort};
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use smol_str::SmolStr;

use crate::id::CloveId;
use crate::model::{ItemFrontmatter, ItemStatus, ItemType, Priority};

/// The kind of relationship an edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EdgeKind {
    /// Hard dependency: `from` depends on `to` (blocking). `from → to`.
    DependsOn = 1,
    /// Hierarchy: `from` is the parent of `to`. `parent → child`.
    ParentOf = 2,
    /// Soft, symmetric relation.
    Relates = 3,
    /// Soft, directional: `from` duplicates `to`.
    Duplicates = 4,
    /// Soft, directional: `from` supersedes `to`.
    Supersedes = 5,
}

/// Whether an edge participates in blocking (`ready`/`blocked`) computation.
pub fn is_hard_dep(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::DependsOn)
}

/// Add an edge `from_node → to_id` of `kind` if `to_id` resolves to a node;
/// otherwise record it as dangling. Returns whether the edge was added.
#[allow(clippy::too_many_arguments)]
fn add_edge_or_dangle(
    graph: &mut StableDiGraph<ItemMeta, EdgeKind>,
    id_to_node: &HashMap<CloveId, NodeIndex>,
    dangling_ids: &mut HashSet<CloveId>,
    dangling_refs: &mut Vec<DanglingRef>,
    from_node: NodeIndex,
    from_id: &CloveId,
    to_id: &CloveId,
    kind: EdgeKind,
) -> bool {
    match id_to_node.get(to_id) {
        Some(&to_node) => {
            graph.add_edge(from_node, to_node, kind);
            true
        }
        None => {
            dangling_ids.insert(to_id.clone());
            dangling_refs.push(DanglingRef {
                from: from_id.clone(),
                to: to_id.clone(),
                kind,
            });
            false
        }
    }
}

/// Per-node metadata stored in the graph.
#[derive(Debug, Clone)]
pub struct ItemMeta {
    pub id: CloveId,
    pub status: ItemStatus,
    pub title: SmolStr,
    pub item_type: ItemType,
    pub priority: Priority,
    /// Missing `DependsOn` targets referenced by this item (no backing file).
    pub dangling_deps: Vec<CloveId>,
    /// Set when this item participates in a self- or cyclic-parent relationship.
    pub malformed_parent: bool,
}

impl ItemMeta {
    pub fn has_dangling_deps(&self) -> bool {
        !self.dangling_deps.is_empty()
    }
}

/// A reference (`from` → `to`, of `kind`) whose target has no backing item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanglingRef {
    pub from: CloveId,
    pub to: CloveId,
    pub kind: EdgeKind,
}

/// A blocked item and the reasons it is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedItem {
    pub id: CloveId,
    /// Existing hard-dependency targets that are not yet closed.
    pub blocking_deps: Vec<CloveId>,
    /// Referenced hard-dependency targets with no backing item.
    pub dangling_deps: Vec<CloveId>,
}

/// Direct-children roll-up for an epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildrenSummary {
    pub total: u32,
    pub closed: u32,
    pub completable: bool,
}

/// A node in a rendered dependency tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepTreeNode {
    pub id: CloveId,
    pub title: String,
    pub status: ItemStatus,
    pub ready: bool,
    /// True when this node repeats an ancestor (a cycle), and is not expanded.
    pub cycle_ref: bool,
    pub children: Vec<DepTreeNode>,
}

/// The in-memory dependency graph.
pub struct GraphStore {
    graph: StableDiGraph<ItemMeta, EdgeKind>,
    id_to_node: HashMap<CloveId, NodeIndex>,
    node_to_id: Vec<CloveId>,
    dangling_ids: HashSet<CloveId>,
}

impl GraphStore {
    /// Build the graph from item frontmatter. Returns the store plus every
    /// dangling reference found (for `clove doctor`).
    ///
    /// Two passes: insert all nodes first, then all edges, so forward references
    /// resolve and unknown targets are recorded as dangling.
    pub fn build(frontmatters: &[ItemFrontmatter]) -> (GraphStore, Vec<DanglingRef>) {
        let mut graph = StableDiGraph::<ItemMeta, EdgeKind>::new();
        let mut id_to_node = HashMap::with_capacity(frontmatters.len());
        let mut node_to_id = Vec::with_capacity(frontmatters.len());

        // Pass 1: nodes.
        for fm in frontmatters {
            let node = graph.add_node(ItemMeta {
                id: fm.id.clone(),
                status: fm.status,
                title: SmolStr::new(&fm.title),
                item_type: fm.item_type,
                priority: fm.priority,
                dangling_deps: Vec::new(),
                malformed_parent: false,
            });
            id_to_node.insert(fm.id.clone(), node);
            node_to_id.push(fm.id.clone());
        }

        let mut dangling_ids = HashSet::new();
        let mut dangling_refs = Vec::new();

        // Pass 2: edges.
        for fm in frontmatters {
            let from_node = id_to_node[&fm.id];

            for dep in &fm.deps {
                let resolved = add_edge_or_dangle(
                    &mut graph,
                    &id_to_node,
                    &mut dangling_ids,
                    &mut dangling_refs,
                    from_node,
                    &fm.id,
                    dep,
                    EdgeKind::DependsOn,
                );
                if !resolved {
                    graph
                        .node_weight_mut(from_node)
                        .expect("node exists")
                        .dangling_deps
                        .push(dep.clone());
                }
            }

            if let Some(parent) = &fm.parent {
                if parent == &fm.id {
                    // Self-parent: malformed; do not add an edge.
                    graph
                        .node_weight_mut(from_node)
                        .expect("node exists")
                        .malformed_parent = true;
                } else if let Some(&parent_node) = id_to_node.get(parent) {
                    // Edge points parent → child.
                    graph.add_edge(parent_node, from_node, EdgeKind::ParentOf);
                } else {
                    dangling_ids.insert(parent.clone());
                    dangling_refs.push(DanglingRef {
                        from: fm.id.clone(),
                        to: parent.clone(),
                        kind: EdgeKind::ParentOf,
                    });
                }
            }

            for related in &fm.relates {
                // Symmetric: record both directions so the relation holds even
                // if only one side lists it.
                if add_edge_or_dangle(
                    &mut graph,
                    &id_to_node,
                    &mut dangling_ids,
                    &mut dangling_refs,
                    from_node,
                    &fm.id,
                    related,
                    EdgeKind::Relates,
                ) {
                    let to_node = id_to_node[related];
                    graph.add_edge(to_node, from_node, EdgeKind::Relates);
                }
            }
            for dup in &fm.duplicates {
                add_edge_or_dangle(
                    &mut graph,
                    &id_to_node,
                    &mut dangling_ids,
                    &mut dangling_refs,
                    from_node,
                    &fm.id,
                    dup,
                    EdgeKind::Duplicates,
                );
            }
            for sup in &fm.supersedes {
                add_edge_or_dangle(
                    &mut graph,
                    &id_to_node,
                    &mut dangling_ids,
                    &mut dangling_refs,
                    from_node,
                    &fm.id,
                    sup,
                    EdgeKind::Supersedes,
                );
            }
        }

        let mut store = GraphStore {
            graph,
            id_to_node,
            node_to_id,
            dangling_ids,
        };
        store.mark_malformed_parents();
        (store, dangling_refs)
    }

    /// Number of items in the graph.
    pub fn len(&self) -> usize {
        self.node_to_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node_to_id.is_empty()
    }

    /// All dangling referenced ids (any edge kind).
    pub fn dangling_ids(&self) -> &HashSet<CloveId> {
        &self.dangling_ids
    }

    /// Metadata for an item, if present.
    pub fn meta(&self, id: &CloveId) -> Option<&ItemMeta> {
        self.id_to_node
            .get(id)
            .and_then(|&n| self.graph.node_weight(n))
    }

    // ---- Ready / blocked ----------------------------------------------------

    /// Items eligible to be worked on now: `open`/`in_progress`, all hard
    /// dependencies closed, no dangling deps, and not excluded by a cycle or a
    /// malformed parent. Sorted by `(priority, topological_rank, id)`.
    pub fn ready_items(&self) -> Vec<CloveId> {
        let excluded = self.excluded_node_set();
        let ranks = self.topological_ranks_internal();

        let mut ready: Vec<NodeIndex> = self
            .graph
            .node_indices()
            .filter(|&node| {
                let meta = &self.graph[node];
                meta.status.is_active()
                    && !meta.has_dangling_deps()
                    && !excluded.contains(&node)
                    && self.hard_deps_all_closed(node)
            })
            .collect();

        ready.sort_by(|&a, &b| {
            let ma = &self.graph[a];
            let mb = &self.graph[b];
            ma.priority
                .get()
                .cmp(&mb.priority.get())
                .then_with(|| {
                    let ra = ranks.get(&a).copied().unwrap_or(usize::MAX);
                    let rb = ranks.get(&b).copied().unwrap_or(usize::MAX);
                    ra.cmp(&rb)
                })
                .then_with(|| ma.id.cmp(&mb.id))
        });

        ready
            .into_iter()
            .map(|n| self.graph[n].id.clone())
            .collect()
    }

    /// Active items that are not ready: they have an unclosed hard dependency or
    /// a dangling dependency. Excludes items in a cycle or with a malformed
    /// parent. Sorted by id.
    pub fn blocked_items(&self) -> Vec<BlockedItem> {
        let excluded = self.excluded_node_set();

        let mut blocked: Vec<BlockedItem> = self
            .graph
            .node_indices()
            .filter(|&node| {
                let meta = &self.graph[node];
                meta.status.is_active() && !excluded.contains(&node)
            })
            .filter_map(|node| {
                let meta = &self.graph[node];
                let blocking_deps = self.open_hard_dep_targets(node);
                if blocking_deps.is_empty() && meta.dangling_deps.is_empty() {
                    return None; // ready, not blocked
                }
                Some(BlockedItem {
                    id: meta.id.clone(),
                    blocking_deps,
                    dangling_deps: meta.dangling_deps.clone(),
                })
            })
            .collect();

        blocked.sort_by(|a, b| a.id.cmp(&b.id));
        blocked
    }

    /// Active items excluded from both `ready` and `blocked` because they are in
    /// a hard-dependency cycle or have a malformed parent. Sorted by id.
    pub fn excluded_items(&self) -> Vec<CloveId> {
        let mut ids: Vec<CloveId> = self
            .excluded_node_set()
            .into_iter()
            .filter(|&node| self.graph[node].status.is_active())
            .map(|node| self.graph[node].id.clone())
            .collect();
        ids.sort();
        ids
    }

    // ---- Cycles -------------------------------------------------------------

    /// Whether adding a `DependsOn` edge `from → to` would create a cycle, i.e.
    /// a path `to → from` already exists over hard-dependency edges. Does not
    /// consider the self-loop case (`from == to`), which callers reject earlier
    /// as a bad argument (DESIGN §5.4).
    pub fn check_would_cycle(&self, from: &CloveId, to: &CloveId) -> bool {
        let (Some(&from_node), Some(&to_node)) =
            (self.id_to_node.get(from), self.id_to_node.get(to))
        else {
            return false;
        };
        let hard = EdgeFiltered::from_fn(&self.graph, |e| is_hard_dep(*e.weight()));
        has_path_connecting(&hard, to_node, from_node, None)
    }

    /// All hard-dependency cycles, each as the list of member ids. Includes
    /// single-node self-loops.
    pub fn all_cycles(&self) -> Vec<Vec<CloveId>> {
        let hard = EdgeFiltered::from_fn(&self.graph, |e| is_hard_dep(*e.weight()));
        let mut cycles = Vec::new();
        for scc in kosaraju_scc(&hard) {
            let is_cycle = scc.len() > 1 || (scc.len() == 1 && self.has_hard_self_loop(scc[0]));
            if is_cycle {
                let mut members: Vec<CloveId> =
                    scc.iter().map(|&n| self.graph[n].id.clone()).collect();
                members.sort();
                cycles.push(members);
            }
        }
        cycles.sort();
        cycles
    }

    /// Whether the graph contains any hard-dependency cycle.
    pub fn has_any_cycle(&self) -> bool {
        !self.all_cycles().is_empty()
    }

    // ---- Dep tree -----------------------------------------------------------

    /// Render the hard-dependency tree rooted at `root`, bounded to `max_depth`
    /// levels of children. Repeated ancestors are marked `cycle_ref` and not
    /// expanded, so cyclic graphs terminate.
    pub fn dep_tree(&self, root: &CloveId, max_depth: usize) -> Option<DepTreeNode> {
        let &root_node = self.id_to_node.get(root)?;
        let ready: HashSet<CloveId> = self.ready_items().into_iter().collect();
        let mut path = HashSet::new();
        Some(self.dep_tree_node(root_node, max_depth, &ready, &mut path))
    }

    fn dep_tree_node(
        &self,
        node: NodeIndex,
        remaining_depth: usize,
        ready: &HashSet<CloveId>,
        path: &mut HashSet<NodeIndex>,
    ) -> DepTreeNode {
        let meta = &self.graph[node];
        let base = DepTreeNode {
            id: meta.id.clone(),
            title: meta.title.to_string(),
            status: meta.status,
            ready: ready.contains(&meta.id),
            cycle_ref: false,
            children: Vec::new(),
        };

        // Already on the current path → cycle; mark and stop.
        if path.contains(&node) {
            return DepTreeNode {
                cycle_ref: true,
                ..base
            };
        }
        if remaining_depth == 0 {
            return base;
        }

        path.insert(node);
        let mut child_targets: Vec<NodeIndex> = self
            .graph
            .edges(node)
            .filter(|e| is_hard_dep(*e.weight()))
            .map(|e| e.target())
            .collect();
        child_targets.sort_by(|&a, &b| self.graph[a].id.cmp(&self.graph[b].id));

        let children = child_targets
            .into_iter()
            .map(|child| self.dep_tree_node(child, remaining_depth - 1, ready, path))
            .collect();
        path.remove(&node);

        DepTreeNode { children, ..base }
    }

    // ---- Epics --------------------------------------------------------------

    /// Roll-up of an epic's *direct* children. `None` if `epic_id` is unknown or
    /// not an [`ItemType::Epic`].
    pub fn epic_children_summary(&self, epic_id: &CloveId) -> Option<ChildrenSummary> {
        let &node = self.id_to_node.get(epic_id)?;
        if self.graph[node].item_type != ItemType::Epic {
            return None;
        }
        let mut total = 0u32;
        let mut closed = 0u32;
        for edge in self.graph.edges(node) {
            if matches!(edge.weight(), EdgeKind::ParentOf) {
                total += 1;
                if matches!(self.graph[edge.target()].status, ItemStatus::Closed) {
                    closed += 1;
                }
            }
        }
        Some(ChildrenSummary {
            total,
            closed,
            completable: total > 0 && closed == total,
        })
    }

    // ---- Internals ----------------------------------------------------------

    /// Mark every node that participates in a parent cycle (a `ParentOf` SCC of
    /// size > 1). Self-parents are already marked during construction.
    fn mark_malformed_parents(&mut self) {
        let cyclic_nodes: Vec<NodeIndex> = {
            let parents =
                EdgeFiltered::from_fn(&self.graph, |e| matches!(e.weight(), EdgeKind::ParentOf));
            kosaraju_scc(&parents)
                .into_iter()
                .filter(|scc| scc.len() > 1)
                .flatten()
                .collect()
        };
        for node in cyclic_nodes {
            if let Some(meta) = self.graph.node_weight_mut(node) {
                meta.malformed_parent = true;
            }
        }
    }

    /// Nodes excluded from ready/blocked: in a hard-dep cycle or malformed parent.
    fn excluded_node_set(&self) -> HashSet<NodeIndex> {
        let mut excluded = HashSet::new();

        // Hard-dependency cycle members.
        let hard = EdgeFiltered::from_fn(&self.graph, |e| is_hard_dep(*e.weight()));
        for scc in kosaraju_scc(&hard) {
            if scc.len() > 1 || (scc.len() == 1 && self.has_hard_self_loop(scc[0])) {
                excluded.extend(scc);
            }
        }
        // Malformed-parent nodes.
        for node in self.graph.node_indices() {
            if self.graph[node].malformed_parent {
                excluded.insert(node);
            }
        }
        excluded
    }

    /// Topological rank of each item over the hard-dependency subgraph, keyed by
    /// id. Empty when the hard-dependency graph contains a cycle (ranks are then
    /// unknown). The SQLite index persists these as `topological_rank` so the
    /// index-path `ready`/`ls` queries can order by `(priority, topo rank)`
    /// without rebuilding the graph (DESIGN.md §6.5, IMPLEMENTATION_PLAN T-S07).
    pub fn topological_ranks(&self) -> HashMap<CloveId, usize> {
        self.topological_ranks_internal()
            .into_iter()
            .filter_map(|(node, rank)| self.graph.node_weight(node).map(|m| (m.id.clone(), rank)))
            .collect()
    }

    /// Topological rank of each node over hard-dependency edges. Empty if the
    /// hard-dependency graph has a cycle (ranks are then treated as unknown).
    fn topological_ranks_internal(&self) -> HashMap<NodeIndex, usize> {
        let hard = EdgeFiltered::from_fn(&self.graph, |e| is_hard_dep(*e.weight()));
        match toposort(&hard, None) {
            Ok(order) => order
                .into_iter()
                .enumerate()
                .map(|(rank, node)| (node, rank))
                .collect(),
            Err(_) => HashMap::new(),
        }
    }

    /// Whether every hard-dependency target of `node` is closed.
    fn hard_deps_all_closed(&self, node: NodeIndex) -> bool {
        self.graph
            .edges(node)
            .filter(|e| is_hard_dep(*e.weight()))
            .all(|e| matches!(self.graph[e.target()].status, ItemStatus::Closed))
    }

    /// Hard-dependency targets of `node` that are not closed (sorted by id).
    fn open_hard_dep_targets(&self, node: NodeIndex) -> Vec<CloveId> {
        let mut ids: Vec<CloveId> = self
            .graph
            .edges(node)
            .filter(|e| is_hard_dep(*e.weight()))
            .map(|e| e.target())
            .filter(|&t| !matches!(self.graph[t].status, ItemStatus::Closed))
            .map(|t| self.graph[t].id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn has_hard_self_loop(&self, node: NodeIndex) -> bool {
        self.graph
            .edges(node)
            .any(|e| e.target() == node && is_hard_dep(*e.weight()))
    }
}

/// Render a dependency tree as a `cargo tree`-style Unicode tree. The returned
/// string includes the root line and a trailing newline.
pub fn render_dep_tree_human(root: &DepTreeNode) -> String {
    let mut out = String::new();
    out.push_str(root.id.as_ref());
    push_node_suffix(&mut out, root);
    out.push('\n');
    render_children(&mut out, &root.children, "");
    out
}

fn render_children(out: &mut String, children: &[DepTreeNode], prefix: &str) {
    let last = children.len().saturating_sub(1);
    for (index, child) in children.iter().enumerate() {
        let is_last = index == last;
        let connector = if is_last { "└── " } else { "├── " };
        out.push_str(prefix);
        out.push_str(connector);
        out.push_str(child.id.as_ref());
        push_node_suffix(out, child);
        out.push('\n');

        if !child.children.is_empty() {
            let extension = if is_last { "    " } else { "│   " };
            let child_prefix = format!("{prefix}{extension}");
            render_children(out, &child.children, &child_prefix);
        }
    }
}

fn push_node_suffix(out: &mut String, node: &DepTreeNode) {
    if node.cycle_ref {
        out.push_str(" (cycle)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn ts() -> DateTime<Utc> {
        "2026-06-02T10:00:00Z".parse().unwrap()
    }

    /// Build a frontmatter with the given id, status, and hard deps.
    fn fm(id: &str, status: ItemStatus, deps: &[&str]) -> ItemFrontmatter {
        ItemFrontmatter {
            schema: 1,
            id: CloveId::new(id).unwrap(),
            title: format!("Item {id}"),
            status,
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
            created: ts(),
            updated: ts(),
            closed: if matches!(status, ItemStatus::Closed) {
                Some(ts())
            } else {
                None
            },
            assignee: None,
            parent: None,
            labels: Vec::new(),
            deps: deps.iter().map(|d| CloveId::new(d).unwrap()).collect(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
    }

    fn id(s: &str) -> CloveId {
        CloveId::new(s).unwrap()
    }

    // V-U01
    #[test]
    fn three_node_blocking_chain() {
        // A depends on B depends on C.
        let a = "proj-AAAAAAAA";
        let b = "proj-BBBBBBBB";
        let c = "proj-CCCCCCCC";

        let open = [
            fm(a, ItemStatus::Open, &[b]),
            fm(b, ItemStatus::Open, &[c]),
            fm(c, ItemStatus::Open, &[]),
        ];
        let (graph, _) = GraphStore::build(&open);
        assert_eq!(graph.ready_items(), vec![id(c)]);
        let blocked = graph.blocked_items();
        assert!(blocked
            .iter()
            .any(|x| x.id == id(a) && x.blocking_deps == vec![id(b)]));
        assert!(blocked
            .iter()
            .any(|x| x.id == id(b) && x.blocking_deps == vec![id(c)]));

        // Close C → B becomes ready.
        let closed_c = [
            fm(a, ItemStatus::Open, &[b]),
            fm(b, ItemStatus::Open, &[c]),
            fm(c, ItemStatus::Closed, &[]),
        ];
        let (graph, _) = GraphStore::build(&closed_c);
        assert_eq!(graph.ready_items(), vec![id(b)]);

        // Close B too → A becomes ready.
        let closed_bc = [
            fm(a, ItemStatus::Open, &[b]),
            fm(b, ItemStatus::Closed, &[c]),
            fm(c, ItemStatus::Closed, &[]),
        ];
        let (graph, _) = GraphStore::build(&closed_bc);
        assert_eq!(graph.ready_items(), vec![id(a)]);
    }

    // V-U02
    #[test]
    fn partition_completeness() {
        let items = [
            fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"]),
            fm("proj-BBBBBBBB", ItemStatus::Closed, &[]),
            fm("proj-CCCCCCCC", ItemStatus::InProgress, &[]),
            fm("proj-DDDDDDDD", ItemStatus::Open, &["proj-CCCCCCCC"]),
            fm("proj-EEEEEEEE", ItemStatus::Closed, &[]),
        ];
        let (graph, _) = GraphStore::build(&items);

        let ready: HashSet<_> = graph.ready_items().into_iter().collect();
        let blocked: HashSet<_> = graph.blocked_items().into_iter().map(|b| b.id).collect();
        let closed: HashSet<_> = items
            .iter()
            .filter(|f| matches!(f.status, ItemStatus::Closed))
            .map(|f| f.id.clone())
            .collect();
        let all: HashSet<_> = items.iter().map(|f| f.id.clone()).collect();

        assert!(ready.is_disjoint(&blocked), "ready ∩ blocked must be empty");
        let union: HashSet<_> = ready
            .union(&blocked)
            .chain(closed.iter())
            .cloned()
            .collect();
        assert_eq!(union, all, "ready ∪ blocked ∪ closed == all");
    }

    // V-U03
    #[test]
    fn cycle_detection_variants() {
        // Two-node cycle A→B→A.
        let two = [
            fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"]),
            fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-AAAAAAAA"]),
        ];
        let (g, _) = GraphStore::build(&two);
        assert!(g.has_any_cycle());
        assert_eq!(
            g.all_cycles(),
            vec![vec![id("proj-AAAAAAAA"), id("proj-BBBBBBBB")]]
        );

        // Linear A→B→C: no cycle.
        let linear = [
            fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"]),
            fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-CCCCCCCC"]),
            fm("proj-CCCCCCCC", ItemStatus::Open, &[]),
        ];
        let (g, _) = GraphStore::build(&linear);
        assert!(!g.has_any_cycle());
        assert!(g.all_cycles().is_empty());

        // Self-loop.
        let selfloop = [fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-AAAAAAAA"])];
        let (g, _) = GraphStore::build(&selfloop);
        assert!(g.has_any_cycle());
        assert_eq!(g.all_cycles(), vec![vec![id("proj-AAAAAAAA")]]);

        // Empty graph.
        let (g, _) = GraphStore::build(&[]);
        assert!(!g.has_any_cycle());
    }

    // V-U04
    #[test]
    fn dangling_deps() {
        let missing = "proj-MISSING0";
        let items = [fm("proj-XXXXXXXX", ItemStatus::Open, &[missing])];
        let (graph, dangling) = GraphStore::build(&items);

        assert!(!graph.ready_items().contains(&id("proj-XXXXXXXX")));
        let blocked = graph.blocked_items();
        let x = blocked
            .iter()
            .find(|b| b.id == id("proj-XXXXXXXX"))
            .unwrap();
        assert_eq!(x.dangling_deps, vec![id(missing)]);
        assert!(x.blocking_deps.is_empty());
        assert!(graph.dangling_ids().contains(&id(missing)));
        assert!(dangling.iter().any(|d| d.to == id(missing)));
    }

    // V-U05
    #[test]
    fn soft_relations_do_not_block() {
        let mut p = fm("proj-PPPPPPPP", ItemStatus::Open, &[]);
        let q = fm("proj-QQQQQQQQ", ItemStatus::Open, &[]);
        p.relates = vec![id("proj-QQQQQQQQ")];
        let (graph, _) = GraphStore::build(&[p, q]);
        let ready: HashSet<_> = graph.ready_items().into_iter().collect();
        assert!(ready.contains(&id("proj-PPPPPPPP")));
        assert!(ready.contains(&id("proj-QQQQQQQQ")));

        // Duplicates edge: also non-blocking.
        let mut p = fm("proj-PPPPPPPP", ItemStatus::Open, &[]);
        let q = fm("proj-QQQQQQQQ", ItemStatus::Open, &[]);
        p.duplicates = vec![id("proj-QQQQQQQQ")];
        let (graph, _) = GraphStore::build(&[p, q]);
        assert_eq!(graph.ready_items().len(), 2);
    }

    // V-U06
    #[test]
    fn epic_children_summary() {
        let mut epic = fm("proj-EPICEPIC", ItemStatus::Open, &[]);
        epic.item_type = ItemType::Epic;
        let mut c1 = fm("proj-C1C1C1C1", ItemStatus::Closed, &[]);
        let c2 = fm("proj-C2C2C2C2", ItemStatus::Open, &[]);
        let mut c3 = fm("proj-C3C3C3C3", ItemStatus::Closed, &[]);
        c1.parent = Some(id("proj-EPICEPIC"));
        // c2 open child:
        let mut c2 = c2;
        c2.parent = Some(id("proj-EPICEPIC"));
        c3.parent = Some(id("proj-EPICEPIC"));

        let (graph, _) = GraphStore::build(&[epic.clone(), c1, c2, c3]);
        assert_eq!(
            graph.epic_children_summary(&id("proj-EPICEPIC")),
            Some(ChildrenSummary {
                total: 3,
                closed: 2,
                completable: false
            })
        );
        // Non-epic returns None.
        assert_eq!(graph.epic_children_summary(&id("proj-C1C1C1C1")), None);

        // Close all children → completable.
        let mut epic2 = fm("proj-EPICEPIC", ItemStatus::Open, &[]);
        epic2.item_type = ItemType::Epic;
        let mut k1 = fm("proj-C1C1C1C1", ItemStatus::Closed, &[]);
        let mut k2 = fm("proj-C2C2C2C2", ItemStatus::Closed, &[]);
        let mut k3 = fm("proj-C3C3C3C3", ItemStatus::Closed, &[]);
        k1.parent = Some(id("proj-EPICEPIC"));
        k2.parent = Some(id("proj-EPICEPIC"));
        k3.parent = Some(id("proj-EPICEPIC"));
        let (graph, _) = GraphStore::build(&[epic2, k1, k2, k3]);
        let summary = graph.epic_children_summary(&id("proj-EPICEPIC")).unwrap();
        assert!(summary.completable);
        assert_eq!(summary.closed, 3);
    }

    // V-U07
    #[test]
    fn dep_tree_depth_limit() {
        // Linear chain A1 → A2 → ... → A8.
        let ids: Vec<String> = (1..=8).map(|n| format!("proj-A000000{n}")).collect();
        let items: Vec<ItemFrontmatter> = (0..ids.len())
            .map(|i| {
                let deps: Vec<&str> = if i + 1 < ids.len() {
                    vec![ids[i + 1].as_str()]
                } else {
                    vec![]
                };
                fm(&ids[i], ItemStatus::Open, &deps)
            })
            .collect();
        let (graph, _) = GraphStore::build(&items);

        let tree = graph.dep_tree(&id(&ids[0]), 5).unwrap();
        // Depth 5 means root + 5 levels of children: A1..A6 present, A7 absent.
        let depth = tree_max_depth(&tree);
        assert_eq!(depth, 5, "tree should be bounded to 5 levels");
        assert!(!tree_contains(&tree, &id(&ids[6]))); // A7 not present
    }

    // V-U08
    #[test]
    fn dep_tree_cycle_marker() {
        let items = [
            fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"]),
            fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-CCCCCCCC"]),
            fm("proj-CCCCCCCC", ItemStatus::Open, &["proj-AAAAAAAA"]),
        ];
        let (graph, _) = GraphStore::build(&items);
        let tree = graph.dep_tree(&id("proj-AAAAAAAA"), 100).unwrap();
        assert_eq!(tree.id, id("proj-AAAAAAAA"));
        assert!(
            tree_has_cycle_ref(&tree),
            "a node should be marked cycle_ref"
        );
        // Completes without infinite recursion (reaching here proves it).
    }

    // V-U10
    #[test]
    fn parent_cycle_detection() {
        let mut x = fm("proj-XXXXXXXX", ItemStatus::Open, &[]);
        let mut y = fm("proj-YYYYYYYY", ItemStatus::Open, &[]);
        x.parent = Some(id("proj-YYYYYYYY"));
        y.parent = Some(id("proj-XXXXXXXX"));
        let (graph, _) = GraphStore::build(&[x, y]);
        assert!(graph.meta(&id("proj-XXXXXXXX")).unwrap().malformed_parent);
        assert!(graph.meta(&id("proj-YYYYYYYY")).unwrap().malformed_parent);
        let ready = graph.ready_items();
        assert!(!ready.contains(&id("proj-XXXXXXXX")));
        assert!(!ready.contains(&id("proj-YYYYYYYY")));
    }

    // V-U11
    #[test]
    fn ready_sorted_by_priority_then_topo() {
        let mk = |id: &str, prio: u8| {
            let mut f = fm(id, ItemStatus::Open, &[]);
            f.priority = Priority(prio);
            f
        };
        let items = [
            mk("proj-LOWPRIOR", 4),
            mk("proj-HIGHPRIO", 0),
            mk("proj-MIDPRIOR", 2),
        ];
        let (graph, _) = GraphStore::build(&items);
        assert_eq!(
            graph.ready_items(),
            vec![
                id("proj-HIGHPRIO"),
                id("proj-MIDPRIOR"),
                id("proj-LOWPRIOR")
            ]
        );
    }

    #[test]
    fn check_would_cycle_detects_back_edge() {
        // A→B→C; adding C→A would cycle.
        let items = [
            fm("proj-AAAAAAAA", ItemStatus::Open, &["proj-BBBBBBBB"]),
            fm("proj-BBBBBBBB", ItemStatus::Open, &["proj-CCCCCCCC"]),
            fm("proj-CCCCCCCC", ItemStatus::Open, &[]),
        ];
        let (graph, _) = GraphStore::build(&items);
        assert!(graph.check_would_cycle(&id("proj-CCCCCCCC"), &id("proj-AAAAAAAA")));
        // Adding A→C would NOT cycle (A already reaches C).
        assert!(!graph.check_would_cycle(&id("proj-AAAAAAAA"), &id("proj-CCCCCCCC")));
    }

    #[test]
    fn human_render_matches_cargo_tree_style() {
        let items = [
            fm(
                "proj-AAAAAAAA",
                ItemStatus::Open,
                &["proj-BBBBBBBB", "proj-CCCCCCCC"],
            ),
            fm("proj-BBBBBBBB", ItemStatus::Open, &[]),
            fm("proj-CCCCCCCC", ItemStatus::Open, &[]),
        ];
        let (graph, _) = GraphStore::build(&items);
        let tree = graph.dep_tree(&id("proj-AAAAAAAA"), 5).unwrap();
        let rendered = render_dep_tree_human(&tree);
        let expected = "proj-AAAAAAAA\n├── proj-BBBBBBBB\n└── proj-CCCCCCCC\n";
        assert_eq!(rendered, expected);
    }

    // --- tree helpers ---
    fn tree_max_depth(node: &DepTreeNode) -> usize {
        node.children
            .iter()
            .map(|c| 1 + tree_max_depth(c))
            .max()
            .unwrap_or(0)
    }
    fn tree_contains(node: &DepTreeNode, target: &CloveId) -> bool {
        &node.id == target || node.children.iter().any(|c| tree_contains(c, target))
    }
    fn tree_has_cycle_ref(node: &DepTreeNode) -> bool {
        node.cycle_ref || node.children.iter().any(tree_has_cycle_ref)
    }
}
