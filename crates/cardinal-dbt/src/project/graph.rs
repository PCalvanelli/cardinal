//! The DAG: children of each relation, and the levels to analyze it in.

use super::Relation;
use crate::names::RelationId;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{BTreeMap, HashMap};

type Levels = (
    Vec<Vec<RelationId>>,
    BTreeMap<RelationId, Vec<RelationId>>,
    Vec<RelationId>,
);

pub(super) fn levels(relations: &BTreeMap<RelationId, Relation>) -> Levels {
    let mut g: DiGraph<RelationId, ()> = DiGraph::new();
    let mut idx: HashMap<&RelationId, NodeIndex> = HashMap::new();
    for id in relations.keys() {
        idx.insert(id, g.add_node(id.clone()));
    }
    let mut children: BTreeMap<RelationId, Vec<RelationId>> = BTreeMap::new();
    for r in relations.values() {
        for d in &r.depends_on {
            g.add_edge(idx[d], idx[&r.id], ());
            children.entry(d.clone()).or_default().push(r.id.clone());
        }
    }
    let mut indeg: HashMap<NodeIndex, usize> = g
        .node_indices()
        .map(|n| (n, g.neighbors_directed(n, petgraph::Incoming).count()))
        .collect();
    let mut frontier: Vec<NodeIndex> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut levels = Vec::new();
    let mut seen = 0;
    while !frontier.is_empty() {
        frontier.sort_by(|a, b| g[*a].cmp(&g[*b]));
        let mut next = Vec::new();
        for n in &frontier {
            for m in g.neighbors_directed(*n, petgraph::Outgoing) {
                let d = indeg.get_mut(&m).unwrap();
                *d -= 1;
                if *d == 0 {
                    next.push(m);
                }
            }
        }
        seen += frontier.len();
        levels.push(frontier.iter().map(|n| g[*n].clone()).collect());
        frontier = next;
    }
    let mut cycle = Vec::new();
    if seen < g.node_count() {
        cycle = indeg
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(n, _)| g[*n].clone())
            .collect();
        cycle.sort();
        levels.push(cycle.clone());
    }
    (levels, children, cycle)
}
