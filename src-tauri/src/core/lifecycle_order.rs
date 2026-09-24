//! Phase 42 — Plugin start / stop order topological sort
//!
//! Based on the Phase 32 capability dependency graph (installed plugin nodes + shared capability edges),
//! Kahn's algorithm arranges nodes into "layers": plugins in layer[i] depend only on nodes in layers ≤ i.
//! Nodes in a cycle share a layer (they must be ready together for the shared capability to resolve).
//!
//! In the returned plan, `stop_order` is `layers` reversed + each layer reversed internally (started last, stopped first).

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::plugin::{CapabilityGraphEdgeDto, PluginManager};

/// Start plan. `layers[i]` is the set of plugin ids that can start in parallel at step i (the whole layer depends only on layers ≤ i-1).
/// `stop_order` is the full shutdown sequence (flattened): layers reversed + each layer reversed.
/// `edges` are Phase 32-style shared capability edges (exposed along the way so the UI can draw the full graph).
#[derive(Debug, Clone, Serialize)]
pub struct LifecyclePlanDto {
    pub layers: Vec<Vec<String>>,
    pub stop_order: Vec<String>,
    pub edges: Vec<CapabilityGraphEdgeDto>,
}

/// Directed-graph Kahn layering: the `(from, to)` semantics of directed_edges = "from must precede to".
/// Existing capability shared edges are double-written by the caller (both directions); per-node in-degree is equivalent to the old implementation;
/// leftover nodes in a cycle are merged into the final layer (carrying over Phase 42's "start a cycle together" semantics).
pub fn layers_from_edges(
    ids: &BTreeSet<String>,
    directed_edges: &[(String, String)],
) -> Vec<Vec<String>> {
    let mut adj: BTreeMap<String, BTreeSet<String>> =
        ids.iter().map(|id| (id.clone(), BTreeSet::new())).collect();
    let mut in_degree: BTreeMap<String, usize> = ids.iter().map(|id| (id.clone(), 0)).collect();
    for (from, to) in directed_edges {
        if !ids.contains(from) || !ids.contains(to) || from == to {
            continue;
        }
        if let Some(set) = adj.get_mut(from) {
            if set.insert(to.clone()) {
                *in_degree.entry(to.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut remaining: BTreeSet<String> = ids.clone();
    let mut layers: Vec<Vec<String>> = Vec::new();
    loop {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|id| in_degree.get(*id).copied().unwrap_or(0) == 0)
            .cloned()
            .collect();
        if ready.is_empty() {
            break;
        }
        let mut ready_sorted = ready;
        ready_sorted.sort();
        for id in &ready_sorted {
            if let Some(neighbors) = adj.get(id) {
                for n in neighbors {
                    if let Some(d) = in_degree.get_mut(n) {
                        if *d > 0 {
                            *d -= 1;
                        }
                    }
                }
            }
        }
        for id in &ready_sorted {
            remaining.remove(id);
        }
        layers.push(ready_sorted);
    }
    if !remaining.is_empty() {
        let mut cyc: Vec<String> = remaining.into_iter().collect();
        cyc.sort();
        layers.push(cyc);
    }
    layers
}

/// Project directed edges from the raw edges: capability shared pairs are **double-written** (preserving undirected semantics),
/// and dependency pairs `(dependent, dep)` are **inverted** to `dep → dependent` (dependencies start first).
pub fn layers_for(
    ids: &BTreeSet<String>,
    cap_pairs: &[(String, String)],
    dep_pairs: &[(String, String)],
) -> Vec<Vec<String>> {
    let mut directed: Vec<(String, String)> = Vec::new();
    for (a, b) in cap_pairs {
        directed.push((a.clone(), b.clone()));
        directed.push((b.clone(), a.clone()));
    }
    for (dependent, dep) in dep_pairs {
        directed.push((dep.clone(), dependent.clone()));
    }
    layers_from_edges(ids, &directed)
}

/// Free function: topologically layer using Phase 32's `capability_dependency_graph` + plugin dependency edges.
pub fn compute_lifecycle_plan(mgr: &PluginManager) -> LifecyclePlanDto {
    let graph = mgr.capability_dependency_graph();
    let ids: BTreeSet<String> = graph.nodes.iter().map(|n| n.id.clone()).collect();

    let cap_pairs: Vec<(String, String)> = graph
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    // F5 — plugin dependency edges (dependent, dep); layers_for inverts them internally to dep → dependent.
    let dep_pairs = mgr.dependency_edges();
    let layers = layers_for(&ids, &cap_pairs, &dep_pairs);

    // stop_order: layers reversed + each layer reversed internally
    let mut stop_order: Vec<String> = Vec::with_capacity(ids.len());
    for layer in layers.iter().rev() {
        for id in layer.iter().rev() {
            stop_order.push(id.clone());
        }
    }

    LifecyclePlanDto {
        layers,
        stop_order,
        edges: graph.edges,
    }
}

/// Start all installed but not running plugins in plan order. Each layer runs in order (serial within a layer;
/// true parallelism would need PluginProcess::spawn refactored to lock-free; for now safety favors conservative serialization).
/// Returns (started_count, errors: Vec<String>).
pub fn start_all_in_order(mgr: &PluginManager, plan: &LifecyclePlanDto) -> (usize, Vec<String>) {
    let mut started = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for layer in &plan.layers {
        for id in layer {
            if let Err(e) = mgr.start(id) {
                errors.push(format!("{id}: {e}"));
            } else {
                started += 1;
            }
        }
    }
    (started, errors)
}

/// Start only plugins in `subset` (in plan layer order). For start recovery: bring up only what was running at last exit.
pub fn start_subset_in_order(
    mgr: &PluginManager,
    plan: &LifecyclePlanDto,
    subset: &BTreeSet<String>,
) -> (usize, Vec<String>) {
    let mut started = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for layer in &plan.layers {
        for id in layer {
            if !subset.contains(id) {
                continue;
            }
            if let Err(e) = mgr.start(id) {
                errors.push(format!("{id}: {e}"));
            } else {
                started += 1;
            }
        }
    }
    (started, errors)
}

/// Stop all running plugins in reverse plan order.
pub fn stop_all_in_order(mgr: &PluginManager, plan: &LifecyclePlanDto) -> usize {
    let mut stopped = 0usize;
    for id in &plan.stop_order {
        mgr.stop(id);
        stopped += 1;
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// Double-write Phase 32's undirected shared capability edges into directed edges (same as production `compute_lifecycle_plan`),
    /// so migrated cases still follow the old semantics of "shared capability = mutual neighbors".
    fn cap_edges_directed(edges: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (a, b) in edges {
            if a != b {
                out.push((a.to_string(), b.to_string()));
                out.push((b.to_string(), a.to_string()));
            }
        }
        out
    }

    /// Dependency chain: a → b (depends on b), b → c (depends on c): layer order c, b, a.
    /// Edge direction = dep → dependent ("run dep first").
    #[test]
    fn dependency_chain_layers_in_order() {
        let ids: BTreeSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let edges = vec![
            ("b".to_string(), "a".to_string()), // a depends on b → edge b→a
            ("c".to_string(), "b".to_string()), // b depends on c → edge c→b
        ];
        let layers = layers_from_edges(&ids, &edges);
        assert_eq!(
            layers,
            vec![
                vec!["c".to_string()],
                vec!["b".to_string()],
                vec!["a".to_string()],
            ]
        );
    }

    /// No dependencies, no sharing → all nodes in the first layer (existing behavior unchanged).
    #[test]
    fn no_edges_puts_all_nodes_in_first_layer() {
        let ids: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let layers = layers_from_edges(&ids, &[]);
        assert_eq!(layers, vec![vec!["a".to_string(), "b".to_string()]]);
    }

    /// Dependency + shared capability mixed: the dep layer goes first; capability-cycle nodes share a layer.
    #[test]
    fn dependency_edge_orders_before_capability_cycle() {
        let ids: BTreeSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        // capability sharing (a↔c both ways) + a depends on b (edge b→a)
        let edges = vec![
            ("a".to_string(), "c".to_string()),
            ("c".to_string(), "a".to_string()),
            ("b".to_string(), "a".to_string()),
        ];
        let layers = layers_from_edges(&ids, &edges);
        assert_eq!(layers[0], vec!["b".to_string()]);
        assert_eq!(layers[1], vec!["a".to_string(), "c".to_string()]);
    }

    /// Hits the production conversion directly: dependency pair `(dependent, dep)` = ("a","b") means a depends on b,
    /// layers_for must invert it to b → a so b starts before a. If the conversion were written same-direction, it would yield [[a],[b]].
    #[test]
    fn direction_inversion_orders_dependency_before_dependent() {
        let ids = id_set(&["a", "b"]);
        let cap_pairs: Vec<(String, String)> = vec![];
        let dep_pairs = vec![("a".to_string(), "b".to_string())];
        let layers = layers_for(&ids, &cap_pairs, &dep_pairs);
        assert_eq!(layers, vec![vec!["b".to_string()], vec!["a".to_string()],]);
    }

    #[test]
    fn kahn_isolates_independent_first() {
        // a-b share an edge + one independent node c → the first layer holds only c; a+b fall into the cycle fallback layer as mutual neighbors
        let ids = id_set(&["a", "b", "c"]);
        let layers = layers_from_edges(&ids, &cap_edges_directed(&[("a", "b")]));
        assert_eq!(layers[0], vec!["c".to_string()]);
        // a+b are mutual neighbors, each with in_degree=1 → enter the cycle fallback layer (started together)
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn cycle_nodes_group_into_one_layer() {
        // a-b share an edge, b-c share an edge → all three nodes have neighbors, in-degree is never 0
        let ids = id_set(&["a", "b", "c"]);
        let layers = layers_from_edges(&ids, &cap_edges_directed(&[("a", "b"), ("b", "c")]));
        // All enter the "cycle fallback" layer
        assert_eq!(
            layers.len(),
            1,
            "no in_degree=0 node in the cycle, so they merge into one layer"
        );
        assert_eq!(
            layers[0],
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn stop_order_reverses_start_layers_and_inner() {
        // Simulated plan: layer 0 = [a, b], layer 1 = [c]; expected stop = [c, b, a]
        let plan = LifecyclePlanDto {
            layers: vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string()],
            ],
            stop_order: Vec::new(),
            edges: Vec::new(),
        };
        // Recompute stop_order
        let mut stop: Vec<String> = Vec::new();
        for layer in plan.layers.iter().rev() {
            for id in layer.iter().rev() {
                stop.push(id.clone());
            }
        }
        assert_eq!(
            stop,
            vec!["c".to_string(), "b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn layers_cover_all_nodes_no_duplicates() {
        // 5 nodes, 3 edges; confirm Kahn drops no node and duplicates none
        let ids = id_set(&["a", "b", "c", "d", "e"]);
        let layers = layers_from_edges(
            &ids,
            &cap_edges_directed(&[("a", "b"), ("c", "d"), ("d", "e")]),
        );
        let flat: Vec<String> = layers.into_iter().flatten().collect();
        let emitted: BTreeSet<String> = flat.iter().cloned().collect();
        assert_eq!(emitted, ids);
        assert_eq!(flat.len(), emitted.len(), "nodes must not be duplicated");
    }
}
