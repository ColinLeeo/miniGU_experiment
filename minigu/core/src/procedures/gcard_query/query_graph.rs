use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use dashmap::DashMap;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use rayon::prelude::*;

use crate::procedures::gcard_query::PredicateApplyType::INNER;
use crate::procedures::gcard_query::abs_graph::AbstractGraph;
use crate::procedures::gcard_query::catalog::{DegreeSeqGraphCompressed, make_alt_key};
use crate::procedures::gcard_query::degreepiecewise::Pcf;
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::flat_graph::FlatGraph;
use crate::procedures::gcard_query::graph::{Endpoints, GraphSkeleton};
use crate::procedures::gcard_query::types::{
    AbstractEdge, CandidateTree, ComparisonOp, PredicateDef, PredicateId, PredicateLocation,
};
use crate::procedures::gcard_query::union_find::UnionFind;
use crate::procedures::gcard_query::{
    BUILD_ABSTRACT_EDGE_NANOS, BUILD_CYCLE_CHECK_NANOS, BUILD_PCF_LOOKUP_NANOS,
    BUILD_PIVOT_PATH_NANOS, BUILD_SCORE_TREE_NANOS, PredicateApplyType,
};

#[derive(Debug, Clone)]
pub struct QueryEdge {
    pub id: EdgeId,
    pub label: String,
    pub src_vertex_id: VertexId,
    pub dst_vertex_id: VertexId,
    pub predicates: Vec<PredicateDef>,
}

impl Endpoints for QueryEdge {
    fn src(&self) -> VertexId {
        self.src_vertex_id
    }

    fn dst(&self) -> VertexId {
        self.dst_vertex_id
    }
}

pub struct QueryGraph {
    pub(crate) inner: GraphSkeleton<QueryEdge>,
    pub predicate_index: HashMap<PredicateId, (PredicateLocation, usize)>,
}

struct Path {
    pub start: VertexId,
    pub end: VertexId,
    pub vertices: Vec<VertexId>,
    pub edges: Vec<EdgeId>,
}

impl std::ops::Deref for QueryGraph {
    type Target = GraphSkeleton<QueryEdge>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl QueryGraph {
    pub fn new() -> Self {
        Self {
            inner: GraphSkeleton {
                vertices: HashMap::new(),
                edges: HashMap::new(),
                outgoing_edges: HashMap::new(),
                incoming_edges: HashMap::new(),
            },
            predicate_index: HashMap::new(),
        }
    }

    pub fn get_predicate_location(&self, predicate_id: PredicateId) -> Option<PredicateLocation> {
        self.predicate_index
            .get(&predicate_id)
            .map(|(location, _)| *location)
    }

    pub fn get_predicate(&self, predicate_id: PredicateId) -> Option<&PredicateDef> {
        self.predicate_index
            .get(&predicate_id)
            .and_then(|(location, idx)| match location {
                PredicateLocation::Vertex(vertex_id) => {
                    self.inner.vertices.get(vertex_id)?.predicates.get(*idx)
                }
                PredicateLocation::Edge(edge_id) => {
                    self.inner.edges.get(edge_id)?.predicates.get(*idx)
                }
            })
    }

    pub fn get_predicates(&self, location: PredicateLocation) -> Vec<&PredicateDef> {
        match location {
            PredicateLocation::Vertex(vertex_id) => self
                .inner
                .vertices
                .get(&vertex_id)
                .map(|v| v.predicates.iter().collect())
                .unwrap_or_default(),
            PredicateLocation::Edge(edge_id) => self
                .inner
                .edges
                .get(&edge_id)
                .map(|e| e.predicates.iter().collect())
                .unwrap_or_default(),
        }
    }

    pub fn score_single_edge(
        &self,
        edge: &QueryEdge,
        predicate_vertices: &HashSet<VertexId>,
        k_path: &HashSet<EdgeId>,
    ) -> u32 {
        if !edge.predicates.is_empty() {
            return 5;
        }
        let src_has_predicate = predicate_vertices.contains(&edge.src_vertex_id);
        let dst_has_predicate = predicate_vertices.contains(&edge.dst_vertex_id);
        if src_has_predicate && dst_has_predicate {
            return 4;
        }
        if k_path.contains(&edge.id) {
            return 3;
        }
        if k_path.contains(&edge.id) {
            return 2;
        }
        1
    }

    pub fn score_edges(&self, k: usize) -> HashMap<EdgeId, u32> {
        let predicate_vertices: HashSet<VertexId> = self
            .inner
            .vertices
            .iter()
            .filter(|(_, v)| !v.predicates.is_empty())
            .map(|(id, _)| *id)
            .collect();
        let k_path_edges = self.find_k_path_edges_between_predicates(&predicate_vertices, k);
        let mut scores = HashMap::new();
        for edge in self.inner.edges.values() {
            let score = self.score_single_edge(edge, &predicate_vertices, &k_path_edges);
            scores.insert(edge.id, score);
        }
        scores
    }

    pub fn find_k_path_edges_between_predicates(
        &self,
        predicate_vertices: &HashSet<VertexId>,
        k: usize,
    ) -> HashSet<EdgeId> {
        let mut k_path_edges = HashSet::new();
        if k == 0 {
            return k_path_edges;
        }

        let predicate_vec: Vec<VertexId> = predicate_vertices.iter().copied().collect();
        for i in 0..predicate_vec.len() {
            for j in (i + 1)..predicate_vec.len() {
                let start = predicate_vec[i];
                let end = predicate_vec[j];
                let paths = self.find_paths_with_length(start, end, k);
                for path in paths {
                    k_path_edges.extend(path);
                }
            }
        }
        k_path_edges
    }

    pub fn find_paths_with_length(
        &self,
        start: VertexId,
        end: VertexId,
        k: usize,
    ) -> Vec<HashSet<EdgeId>> {
        if k == 0 {
            if start == end {
                return vec![HashSet::new()];
            }
            return vec![];
        }
        let mut result = Vec::new();
        let mut path_edges = HashSet::new();
        let mut visited = HashSet::from([start]);
        // Stack of (vertex, neighbor_iterator_index); backtrack by popping.
        let mut stack: Vec<(VertexId, Vec<(VertexId, EdgeId)>, usize)> = Vec::new();

        let neighbors = self.get_neighbor_edge_pairs(start);
        stack.push((start, neighbors, 0));

        while let Some(frame) = stack.last_mut() {
            let (_current, ref nbrs, ref mut idx) = *frame;
            if *idx >= nbrs.len() {
                // Backtrack
                let (v, _, _) = stack.pop().unwrap();
                if let Some(parent) = stack.last() {
                    // Remove the edge that led to v
                    let (_, ref parent_nbrs, parent_idx) = *parent;
                    if parent_idx > 0 {
                        let (_, edge_id) = parent_nbrs[parent_idx - 1];
                        path_edges.remove(&edge_id);
                    }
                }
                visited.remove(&v);
                continue;
            }
            let (neighbor, edge_id) = nbrs[*idx];
            *idx += 1;

            if visited.contains(&neighbor) {
                continue;
            }

            path_edges.insert(edge_id);
            visited.insert(neighbor);

            if path_edges.len() == k {
                if neighbor == end {
                    result.push(path_edges.clone());
                }
                path_edges.remove(&edge_id);
                visited.remove(&neighbor);
            } else {
                let next_nbrs = self.get_neighbor_edge_pairs(neighbor);
                stack.push((neighbor, next_nbrs, 0));
            }
        }
        result
    }

    /// Get all (neighbor_vertex, edge_id) pairs for a vertex.
    fn get_neighbor_edge_pairs(&self, v: VertexId) -> Vec<(VertexId, EdgeId)> {
        let mut pairs = Vec::new();
        if let Some(outgoing_ids) = self.inner.outgoing_edges.get(&v) {
            for &edge_id in outgoing_ids {
                if let Some(edge) = self.inner.edges.get(&edge_id) {
                    pairs.push((edge.dst_vertex_id, edge_id));
                }
            }
        }
        if let Some(incoming_ids) = self.inner.incoming_edges.get(&v) {
            for &edge_id in incoming_ids {
                if let Some(edge) = self.inner.edges.get(&edge_id) {
                    pairs.push((edge.src_vertex_id, edge_id));
                }
            }
        }
        pairs
    }

    pub fn build_best_spanning_tree(&self, scores: &HashMap<EdgeId, u32>) -> Option<CandidateTree> {
        let mut edges_with_scores: Vec<_> = self
            .inner
            .edges
            .values()
            .map(|edge| {
                let score = scores.get(&edge.id).copied().unwrap_or(0);
                (edge.clone(), score)
            })
            .collect();
        edges_with_scores.sort_by(|a, b| b.1.cmp(&a.1));

        let mut selected_edges = HashSet::new();
        let mut uf = UnionFind::new();
        let mut total_score = 0;

        for (edge, score) in edges_with_scores {
            uf.make_set(edge.src_vertex_id);
            uf.make_set(edge.dst_vertex_id);

            if uf.union(edge.src_vertex_id, edge.dst_vertex_id) {
                selected_edges.insert(edge.id);
                total_score += score;
            }
        }

        if selected_edges.is_empty() {
            None
        } else {
            Some(CandidateTree {
                edge_ids: selected_edges,
                total_score,
            })
        }
    }

    fn build_subgraph_from_edges(&self, selected_edge_ids: &HashSet<EdgeId>) -> QueryGraph {
        let mut subgraph_vertices = HashMap::new();
        let mut subgraph_edges = HashMap::new();
        let mut subgraph_outgoing = HashMap::new();
        let mut subgraph_incoming = HashMap::new();

        for &edge_id in selected_edge_ids {
            if let Some(edge) = self.inner.edges.get(&edge_id) {
                subgraph_edges.insert(edge_id, edge.clone());

                if let Some(vertex) = self.inner.vertices.get(&edge.src_vertex_id) {
                    subgraph_vertices
                        .entry(edge.src_vertex_id)
                        .or_insert_with(|| vertex.clone());
                }

                if let Some(vertex) = self.inner.vertices.get(&edge.dst_vertex_id) {
                    subgraph_vertices
                        .entry(edge.dst_vertex_id)
                        .or_insert_with(|| vertex.clone());
                }

                subgraph_outgoing
                    .entry(edge.src_vertex_id)
                    .or_insert_with(Vec::new)
                    .push(edge_id);

                subgraph_incoming
                    .entry(edge.dst_vertex_id)
                    .or_insert_with(Vec::new)
                    .push(edge_id);
            }
        }

        QueryGraph {
            inner: GraphSkeleton {
                vertices: subgraph_vertices,
                edges: subgraph_edges,
                outgoing_edges: subgraph_outgoing,
                incoming_edges: subgraph_incoming,
            },
            predicate_index: self.predicate_index.clone(),
        }
    }

    /// 返回 (QueryGraph, total_score) 的列表，按分数从高到低。
    pub fn build_k_best_trees(
        &self,
        scores: &HashMap<EdgeId, u32>,
        k: usize,
    ) -> Vec<(QueryGraph, u32)> {
        if k == 0 {
            return Vec::new();
        }

        let first_tree = self.build_best_spanning_tree(scores);
        if first_tree.is_none() {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut seen_trees: HashSet<Vec<EdgeId>> = HashSet::new();
        let mut candidates = BinaryHeap::new();

        let first_tree = first_tree.unwrap();
        let mut sorted_key: Vec<EdgeId> = first_tree.edge_ids.iter().copied().collect();
        sorted_key.sort();
        seen_trees.insert(sorted_key);
        candidates.push(first_tree);

        while result.len() < k && !candidates.is_empty() {
            let current = candidates.pop().unwrap();
            let current_edge_set = current.edge_ids.clone();

            result.push((
                self.build_subgraph_from_edges(&current_edge_set),
                current.total_score,
            ));
            if result.len() >= k {
                break;
            }

            let all_edge_ids: HashSet<EdgeId> = self.inner.edges.keys().copied().collect();
            let mut non_tree_edges: Vec<(EdgeId, u32)> = all_edge_ids
                .difference(&current.edge_ids)
                .map(|&eid| {
                    let score = scores.get(&eid).copied().unwrap_or(0);
                    (eid, score)
                })
                .collect();

            non_tree_edges.sort_by(|a, b| b.1.cmp(&a.1));

            for (new_edge_id, _new_edge_score) in non_tree_edges {
                if let Some(new_edge) = self.inner.edges.get(&new_edge_id) {
                    let src = new_edge.src_vertex_id;
                    let dst = new_edge.dst_vertex_id;

                    if let Some(path_edges) =
                        self.find_path_edges_in_tree(&current.edge_ids, src, dst)
                    {
                        let mut path_edges_with_scores: Vec<(EdgeId, u32)> = path_edges
                            .iter()
                            .map(|&eid| {
                                let score = scores.get(&eid).copied().unwrap_or(0);
                                (eid, score)
                            })
                            .collect();

                        path_edges_with_scores.sort_by(|a, b| a.1.cmp(&b.1));

                        for (edge_to_remove, _old_score) in path_edges_with_scores {
                            let mut new_edge_set = current.edge_ids.clone();
                            new_edge_set.remove(&edge_to_remove);
                            new_edge_set.insert(new_edge_id);

                            let mut sorted_key: Vec<EdgeId> =
                                new_edge_set.iter().copied().collect();
                            sorted_key.sort();
                            if seen_trees.contains(&sorted_key) {
                                continue;
                            }

                            if self.is_tree(&new_edge_set) {
                                let total_score: u32 = new_edge_set
                                    .iter()
                                    .map(|&eid| scores.get(&eid).copied().unwrap_or(0))
                                    .sum();

                                let candidate =
                                    crate::procedures::gcard_query::types::CandidateTree {
                                        edge_ids: new_edge_set.clone(),
                                        total_score,
                                    };

                                seen_trees.insert(sorted_key);
                                candidates.push(candidate);
                            }
                        }
                    } else {
                        let mut new_edge_set = current.edge_ids.clone();
                        new_edge_set.insert(new_edge_id);

                        let mut sorted_key: Vec<EdgeId> = new_edge_set.iter().copied().collect();
                        sorted_key.sort();
                        if seen_trees.contains(&sorted_key) {
                            continue;
                        }

                        if self.is_tree(&new_edge_set) {
                            let total_score: u32 = new_edge_set
                                .iter()
                                .map(|&eid| scores.get(&eid).copied().unwrap_or(0))
                                .sum();

                            let candidate = crate::procedures::gcard_query::types::CandidateTree {
                                edge_ids: new_edge_set.clone(),
                                total_score,
                            };

                            seen_trees.insert(sorted_key);
                            candidates.push(candidate);
                        }
                    }
                }
            }
        }

        result
    }

    fn has_cycle(&self) -> bool {
        let mut uf = UnionFind::new();

        for edge in self.inner.edges.values() {
            uf.make_set(edge.src_vertex_id);
            uf.make_set(edge.dst_vertex_id);
            if !uf.union(edge.src_vertex_id, edge.dst_vertex_id) {
                return true;
            }
        }

        false
    }

    fn build_selectivity_cache_key(
        alt_key: &crate::procedures::gcard_query::catalog::AltKey,
        predicates: &[PredicateDef],
    ) -> String {
        use std::fmt::Write;
        let mut key = format!("path:{}", alt_key);
        // Sort predicates by (target, id, property, op) for a deterministic key
        let mut sorted_preds: Vec<_> = predicates.iter().collect();
        sorted_preds.sort_by(|a, b| {
            a.target
                .cmp(&b.target)
                .then(a.id.cmp(&b.id))
                .then(a.property.cmp(&b.property))
                .then(format!("{:?}", a.op).cmp(&format!("{:?}", b.op)))
        });
        for p in sorted_preds {
            let _ = write!(
                key,
                "|{}:{}:{}:{:?}:{:?}",
                p.target, p.id, p.property, p.op, p.value
            );
        }
        key
    }

    fn build_path_query(&self, abstract_edge: &AbstractEdge) -> GCardResult<PathQuery> {
        let mut path_elements = Vec::new();
        let mut vertex_predicates: HashMap<usize, Vec<PredicateDef>> = HashMap::new();
        let mut edge_predicates: HashMap<usize, Vec<PredicateDef>> = HashMap::new();

        for (idx, &vertex_id) in abstract_edge.path_vertices.iter().enumerate() {
            let vertex = self.inner.vertices.get(&vertex_id).ok_or_else(|| {
                GCardError::VertexNotFound(format!("Vertex {} not found", vertex_id))
            })?;
            path_elements.push(PathElement::Vertex {
                label: vertex.label.clone(),
                position: idx * 2,
            });
            let mut v_preds = Vec::new();
            for pred in &abstract_edge.predicates {
                if pred.target == "vertex" && pred.id == vertex_id as u32 {
                    v_preds.push(pred.clone());
                }
            }
            if !v_preds.is_empty() {
                vertex_predicates.insert(idx * 2, v_preds);
            }

            if idx < abstract_edge.original_edge_ids.len() {
                let edge_id = abstract_edge.original_edge_ids[idx];
                let edge = self.inner.edges.get(&edge_id).ok_or_else(|| {
                    GCardError::EdgeNotFound(format!("Edge {} not found", edge_id))
                })?;
                let edge_parts: Vec<&str> = edge.label.split('_').collect();
                let direction = if !edge_parts.is_empty() {
                    let src_label = edge_parts[0];
                    let dst_label = edge_parts.last().copied().unwrap_or("");
                    if src_label == vertex.label {
                        EdgeDirection::Outgoing
                    } else if dst_label == vertex.label {
                        EdgeDirection::Incoming
                    } else {
                        EdgeDirection::Outgoing
                    }
                } else {
                    EdgeDirection::Outgoing
                };

                path_elements.push(PathElement::Edge {
                    label: edge.label.clone(),
                    position: idx * 2 + 1,
                    direction,
                });

                let mut e_preds = Vec::new();
                for pred in &abstract_edge.predicates {
                    if pred.target == "edge" && pred.id == edge_id as u32 {
                        e_preds.push(pred.clone());
                    }
                }
                if !e_preds.is_empty() {
                    edge_predicates.insert(idx * 2 + 1, e_preds);
                }
            }
        }

        Ok(PathQuery {
            path_elements,
            vertex_predicates,
            edge_predicates,
        })
    }

    fn flush_local_cache(local: &HashMap<(u32, u64), bool>, shared: &DashMap<(u32, u64), bool>) {
        for (&k, &v) in local {
            shared.entry(k).or_insert(v);
        }
    }

    fn compare_values(
        &self,
        value: &ScalarValue,
        op: &ComparisonOp,
        expected: &ScalarValue,
    ) -> GCardResult<bool> {
        use ComparisonOp::*;

        match op {
            Eq => Ok(value == expected),
            Ne => Ok(value != expected),
            Gt => self.compare_ordered(value, expected, |a, b| {
                self.partial_cmp_scalar(a, b)
                    .map(|ord| ord == std::cmp::Ordering::Greater)
            }),
            Ge => self.compare_ordered(value, expected, |a, b| {
                self.partial_cmp_scalar(a, b).map(|ord| {
                    ord == std::cmp::Ordering::Greater || ord == std::cmp::Ordering::Equal
                })
            }),
            Lt => self.compare_ordered(value, expected, |a, b| {
                self.partial_cmp_scalar(a, b)
                    .map(|ord| ord == std::cmp::Ordering::Less)
            }),
            Le => self.compare_ordered(value, expected, |a, b| {
                self.partial_cmp_scalar(a, b)
                    .map(|ord| ord == std::cmp::Ordering::Less || ord == std::cmp::Ordering::Equal)
            }),
        }
    }

    fn compare_ordered<F>(
        &self,
        value: &ScalarValue,
        expected: &ScalarValue,
        cmp: F,
    ) -> GCardResult<bool>
    where
        F: FnOnce(&ScalarValue, &ScalarValue) -> Option<bool>,
    {
        cmp(value, expected).ok_or_else(|| {
            GCardError::InvalidData(format!(
                "Cannot compare values: {:?} and {:?}",
                value, expected
            ))
        })
    }

    fn partial_cmp_scalar(&self, a: &ScalarValue, b: &ScalarValue) -> Option<std::cmp::Ordering> {
        use ScalarValue::*;

        match (a, b) {
            (Int8(Some(a_val)), Int8(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Int16(Some(a_val)), Int16(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Int32(Some(a_val)), Int32(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Int64(Some(a_val)), Int64(Some(b_val))) => Some(a_val.cmp(b_val)),
            (UInt8(Some(a_val)), UInt8(Some(b_val))) => Some(a_val.cmp(b_val)),
            (UInt16(Some(a_val)), UInt16(Some(b_val))) => Some(a_val.cmp(b_val)),
            (UInt32(Some(a_val)), UInt32(Some(b_val))) => Some(a_val.cmp(b_val)),
            (UInt64(Some(a_val)), UInt64(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Float32(Some(a_val)), Float32(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Float64(Some(a_val)), Float64(Some(b_val))) => Some(a_val.cmp(b_val)),
            (String(Some(a_val)), String(Some(b_val))) => Some(a_val.cmp(b_val)),
            (Boolean(Some(a_val)), Boolean(Some(b_val))) => Some(a_val.cmp(b_val)),
            // 跨类型比较：尝试转换为f64
            _ => {
                let a_f64 = self.to_f64_opt(a);
                let b_f64 = self.to_f64_opt(b);
                match (a_f64, b_f64) {
                    (Some(a_f), Some(b_f)) => {
                        use ordered_float::OrderedFloat;
                        Some(OrderedFloat(a_f).cmp(&OrderedFloat(b_f)))
                    }
                    _ => None,
                }
            }
        }
    }

    fn to_f64_opt(&self, value: &ScalarValue) -> Option<f64> {
        use ScalarValue::*;
        match value {
            Int8(Some(v)) => Some(*v as f64),
            Int16(Some(v)) => Some(*v as f64),
            Int32(Some(v)) => Some(*v as f64),
            Int64(Some(v)) => Some(*v as f64),
            UInt8(Some(v)) => Some(*v as f64),
            UInt16(Some(v)) => Some(*v as f64),
            UInt32(Some(v)) => Some(*v as f64),
            UInt64(Some(v)) => Some(*v as f64),
            Float32(Some(v)) => Some(v.into_inner() as f64),
            Float64(Some(v)) => Some(v.into_inner()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PathQuery {
    path_elements: Vec<PathElement>,
    vertex_predicates: HashMap<usize, Vec<PredicateDef>>,
    edge_predicates: HashMap<usize, Vec<PredicateDef>>,
}

#[derive(Debug, Clone)]
enum EdgeDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone)]
enum PathElement {
    Vertex {
        label: String,
        position: usize,
    },
    Edge {
        label: String,
        position: usize,
        direction: EdgeDirection,
    },
}

/// Per-thread profiling accumulator for walk steps.
#[derive(Default)]
struct WalkProf {
    nbr_nanos: u64,
    prop_nanos: u64,
    walk_count: u64,
}

/// Pre-resolved predicate: property index + comparison value + operator.
#[derive(Clone)]
struct ResolvedPredicate {
    predicate_id: Option<u32>,
    prop_index: usize,
    op: ComparisonOp,
    value: ScalarValue,
}

impl QueryGraph {
    fn find_paths_from_pivots(&self, pivot_nodes: &HashSet<VertexId>) -> Vec<Path> {
        if pivot_nodes.is_empty() {
            return self.build_path_from_entire_graph();
        }

        let mut paths = Vec::new();
        let mut visited_edges = HashSet::new();

        for &pivot in pivot_nodes {
            let neighbors = self.get_neighbors(pivot);

            for neighbor in neighbors {
                let outgoing = self.get_outgoing_edges(pivot);
                let incoming = self.get_incoming_edges(pivot);
                let edge_opt = outgoing
                    .iter()
                    .find(|e| e.dst_vertex_id == neighbor)
                    .or_else(|| incoming.iter().find(|e| e.src_vertex_id == neighbor));

                if let Some(edge) = edge_opt {
                    let edge_key = if pivot < neighbor {
                        (pivot, neighbor, edge.id)
                    } else {
                        (neighbor, pivot, edge.id)
                    };

                    if visited_edges.contains(&edge_key) {
                        continue;
                    }
                    visited_edges.insert(edge_key);

                    if let Some(path) = self.traverse_path(pivot, neighbor, edge.id, pivot_nodes) {
                        paths.push(path);
                    }
                }
            }
        }

        paths
    }

    fn build_path_from_entire_graph(&self) -> Vec<Path> {
        if self.inner.vertices.is_empty() {
            return Vec::new();
        }

        let start_vertex = self
            .inner
            .vertices
            .keys()
            .filter(|&&vid| self.get_degree(vid) == 1)
            .min()
            .copied()
            .or_else(|| self.inner.vertices.keys().min().copied())
            .expect("Graph should have at least one vertex");

        let mut vertices = vec![start_vertex];
        let mut edges = Vec::new();
        let mut current_vertex = start_vertex;

        loop {
            let neighbors = self.get_neighbors(current_vertex);
            let mut next_vertex = None;
            let mut next_edge_id = None;
            for neighbor in neighbors {
                if vertices.contains(&neighbor) {
                    continue;
                }

                let outgoing = self.get_outgoing_edges(current_vertex);
                let incoming = self.get_incoming_edges(current_vertex);
                let edge_opt = outgoing
                    .iter()
                    .find(|e| e.dst_vertex_id == neighbor)
                    .or_else(|| incoming.iter().find(|e| e.src_vertex_id == neighbor));

                if let Some(edge) = edge_opt {
                    next_vertex = Some(neighbor);
                    next_edge_id = Some(edge.id);
                    break;
                }
            }

            if let (Some(next_v), Some(next_e)) = (next_vertex, next_edge_id) {
                vertices.push(next_v);
                edges.push(next_e);
                current_vertex = next_v;
            } else {
                break;
            }
        }
        vec![Path {
            start: vertices[0],
            end: vertices[vertices.len() - 1],
            vertices,
            edges,
        }]
    }

    fn traverse_path(
        &self,
        start_pivot: VertexId,
        current: VertexId,
        first_edge_id: EdgeId,
        pivot_nodes: &HashSet<VertexId>,
    ) -> Option<Path> {
        let mut vertices = vec![start_pivot, current];
        let mut edges = vec![first_edge_id];
        let mut visited_vertices = HashSet::from([start_pivot, current]);
        let mut current_vertex = current;

        loop {
            if pivot_nodes.contains(&current_vertex) {
                return Some(Path {
                    start: start_pivot,
                    end: current_vertex,
                    vertices,
                    edges,
                });
            }

            if self.get_degree(current_vertex) == 1 {
                return Some(Path {
                    start: start_pivot,
                    end: current_vertex,
                    vertices,
                    edges,
                });
            }

            let neighbors = self.get_neighbors(current_vertex);
            let mut next_vertex = None;
            let mut next_edge_id = None;

            for neighbor in neighbors {
                if visited_vertices.contains(&neighbor) {
                    continue;
                }

                let outgoing = self.get_outgoing_edges(current_vertex);
                let incoming = self.get_incoming_edges(current_vertex);
                let edge_opt = outgoing
                    .iter()
                    .find(|e| e.dst_vertex_id == neighbor)
                    .or_else(|| incoming.iter().find(|e| e.src_vertex_id == neighbor));

                if let Some(edge) = edge_opt {
                    next_vertex = Some(neighbor);
                    next_edge_id = Some(edge.id);
                    break;
                }
            }

            if let (Some(next_v), Some(next_e)) = (next_vertex, next_edge_id) {
                vertices.push(next_v);
                edges.push(next_e);
                visited_vertices.insert(next_v);
                current_vertex = next_v;
            } else {
                return Some(Path {
                    start: start_pivot,
                    end: current_vertex,
                    vertices,
                    edges,
                });
            }
        }
    }

    fn build_abstract_edges_for_path(
        &self,
        path: &Path,
        k: usize,
    ) -> GCardResult<Vec<AbstractEdge>> {
        let l = path.edges.len();
        if l == 0 {
            return Ok(Vec::new());
        }

        let num_abstract_edges = (l as f64 / k as f64).ceil() as usize;

        if l % k == 0 {
            return self.build_abstract_edges_even(path, k);
        }

        self.build_abstract_edges_optimal(path, k, num_abstract_edges)
    }

    fn build_abstract_edges_even(&self, path: &Path, k: usize) -> GCardResult<Vec<AbstractEdge>> {
        let mut abstract_edges = Vec::new();
        let mut edge_idx = 0;

        while edge_idx < path.edges.len() {
            let end_idx = (edge_idx + k).min(path.edges.len());
            let abstract_edge_edges = &path.edges[edge_idx..end_idx];

            let mut predicates = Vec::new();
            for &edge_id in abstract_edge_edges {
                if let Some(edge) = self.inner.edges.get(&edge_id) {
                    predicates.extend(edge.predicates.clone());
                }
            }

            let src_vertex_idx = edge_idx;
            let dst_vertex_idx = if end_idx < path.vertices.len() {
                end_idx
            } else {
                path.vertices.len() - 1
            };

            let src = path.vertices[src_vertex_idx];
            let dst = path.vertices[dst_vertex_idx];

            let path_vertices = path.vertices[src_vertex_idx..=dst_vertex_idx].to_vec();
            for &vertex_id in &path_vertices {
                if let Some(vertex) = self.inner.vertices.get(&vertex_id) {
                    predicates.extend(vertex.predicates.clone());
                }
            }

            let src_pcf = Arc::new(Pcf::empty());
            let dst_pcf = Arc::new(Pcf::empty());

            abstract_edges.push(AbstractEdge {
                src,
                dst,
                src_pcf,
                dst_pcf,
                predicates,
                original_edge_ids: abstract_edge_edges.to_vec(),
                path_vertices,
                selectivity: 1.0,
                path_str: String::new(),
            });

            edge_idx += k;
        }

        Ok(abstract_edges)
    }

    fn build_abstract_edges_optimal(
        &self,
        path: &Path,
        k: usize,
        num_abstract_edges: usize,
    ) -> GCardResult<Vec<AbstractEdge>> {
        let l = path.edges.len();
        let short_size = l.saturating_sub((num_abstract_edges - 1) * k);

        let mut best_solution: Option<Vec<usize>> = None;
        let mut min_predicate_count = usize::MAX;

        for short_pos in 0..num_abstract_edges {
            let mut solution = vec![k; num_abstract_edges];
            solution[short_pos] = short_size;
            let short_start = short_pos * k;
            let short_end = short_start + short_size;
            let short_predicate_count =
                self.count_predicates_in_range(path, short_start, short_end);

            if short_predicate_count < min_predicate_count {
                min_predicate_count = short_predicate_count;
                best_solution = Some(solution);
            }
        }

        let solution = best_solution.unwrap_or_else(|| {
            let mut sol = vec![k; num_abstract_edges - 1];
            sol.push(l - (num_abstract_edges - 1) * k);
            sol
        });

        let mut abstract_edges = Vec::new();
        let mut edge_idx = 0;

        for &edge_count in &solution {
            let end_idx = (edge_idx + edge_count).min(path.edges.len());
            let abstract_edge_edges = &path.edges[edge_idx..end_idx];

            let mut predicates = Vec::new();
            for &edge_id in abstract_edge_edges {
                if let Some(edge) = self.inner.edges.get(&edge_id) {
                    predicates.extend(edge.predicates.clone());
                }
            }

            let src_vertex_idx = edge_idx;
            let dst_vertex_idx = if end_idx < path.vertices.len() {
                end_idx
            } else {
                path.vertices.len() - 1
            };

            let src = path.vertices[src_vertex_idx];
            let dst = path.vertices[dst_vertex_idx];

            let path_vertices = path.vertices[src_vertex_idx..=dst_vertex_idx].to_vec();
            for &vertex_id in &path_vertices {
                if let Some(vertex) = self.inner.vertices.get(&vertex_id) {
                    predicates.extend(vertex.predicates.clone());
                }
            }

            let src_pcf = Arc::new(Pcf::empty());
            let dst_pcf = Arc::new(Pcf::empty());

            abstract_edges.push(AbstractEdge {
                src,
                dst,
                src_pcf,
                dst_pcf,
                predicates,
                original_edge_ids: abstract_edge_edges.to_vec(),
                path_vertices,
                selectivity: 1.0,
                path_str: String::new(),
            });

            edge_idx += edge_count;
        }

        Ok(abstract_edges)
    }

    fn find_path_edges_in_tree(
        &self,
        tree_edges: &HashSet<EdgeId>,
        src: VertexId,
        dst: VertexId,
    ) -> Option<HashSet<EdgeId>> {
        if src == dst {
            return Some(HashSet::new());
        }

        let mut queue = std::collections::VecDeque::new();
        queue.push_back((src, HashSet::new()));
        let mut visited = HashSet::new();
        visited.insert(src);

        while let Some((current, path)) = queue.pop_front() {
            if current == dst {
                return Some(path);
            }

            for edge in self.get_outgoing_edges(current) {
                if tree_edges.contains(&edge.id) && !visited.contains(&edge.dst()) {
                    let mut new_path = path.clone();
                    new_path.insert(edge.id);
                    visited.insert(edge.dst());
                    queue.push_back((edge.dst(), new_path));
                }
            }

            for edge in self.get_incoming_edges(current) {
                if tree_edges.contains(&edge.id) && !visited.contains(&edge.src()) {
                    let mut new_path = path.clone();
                    new_path.insert(edge.id);
                    visited.insert(edge.src());
                    queue.push_back((edge.src(), new_path));
                }
            }
        }

        None
    }

    fn is_tree(&self, edge_set: &HashSet<EdgeId>) -> bool {
        if edge_set.is_empty() {
            return false;
        }

        let mut uf = UnionFind::new();
        let mut vertex_set = HashSet::new();

        for &edge_id in edge_set {
            if let Some(edge) = self.inner.edges.get(&edge_id) {
                uf.make_set(edge.src_vertex_id);
                uf.make_set(edge.dst_vertex_id);
                vertex_set.insert(edge.src_vertex_id);
                vertex_set.insert(edge.dst_vertex_id);

                if !uf.union(edge.src_vertex_id, edge.dst_vertex_id) {
                    return false;
                }
            }
        }

        edge_set.len() == vertex_set.len() - 1
    }

    fn count_predicates_in_range(
        &self,
        path: &Path,
        start_edge_idx: usize,
        end_edge_idx: usize,
    ) -> usize {
        let mut count = 0;

        for &edge_id in &path.edges[start_edge_idx..end_edge_idx] {
            if let Some(edge) = self.edges.get(&edge_id) {
                count += edge.predicates.len();
            }
        }

        let start_vertex_idx = start_edge_idx;
        let end_vertex_idx = if end_edge_idx < path.vertices.len() {
            end_edge_idx
        } else {
            path.vertices.len() - 1
        };

        for &vertex_id in &path.vertices[start_vertex_idx..=end_vertex_idx] {
            if let Some(vertex) = self.inner.vertices.get(&vertex_id) {
                count += vertex.predicates.len();
            }
        }

        count
    }
}

// ── FlatGraph-based sampling ───────────────────────────────────────────────────
//
// These types and methods mirror the DB-based (MemoryGraph + MemTransaction)
// sampling path but use [`FlatGraph`] for all graph access, removing any
// dependency on MVCC or the storage layer.

/// Pre-compiled path step for FlatGraph walks.
///
/// Uses string labels instead of numeric `LabelId`s.
#[derive(Clone)]
enum FlatCompiledStep {
    Vertex {
        /// Vertex label (lowercased), used to look up vertex properties.
        label: String,
        predicates: Vec<ResolvedPredicate>,
    },
    Edge {
        edge_label: String,
        direction: EdgeDirection,
        predicates: Vec<ResolvedPredicate>,
    },
}

/// A fully compiled path query for FlatGraph walks.
struct FlatCompiledPathQuery {
    steps: Vec<FlatCompiledStep>,
    /// Label of the source vertex, used for sampling start vertices.
    src_label: String,
}

impl FlatCompiledPathQuery {
    /// Compile a [`PathQuery`] against `flat_graph`.
    ///
    /// Property indices are resolved via [`FlatGraph::vertex_prop_index`] /
    /// [`FlatGraph::edge_prop_index`].  If a property cannot be resolved (e.g.
    /// the FlatGraph was built without properties), the predicate is silently
    /// dropped — the walk treats the step as always-passing, giving selectivity
    /// 1.0 for that predicate.
    fn compile_flat(path_query: &PathQuery, flat_graph: &FlatGraph) -> GCardResult<Self> {
        let mut steps = Vec::with_capacity(path_query.path_elements.len());
        let mut src_label = String::new();
        let mut first_vertex = true;

        for element in &path_query.path_elements {
            match element {
                PathElement::Vertex { label, position } => {
                    if first_vertex {
                        src_label = label.clone();
                        first_vertex = false;
                    }

                    let predicates = if let Some(preds) = path_query.vertex_predicates.get(position)
                    {
                        preds
                            .iter()
                            .filter_map(|p| {
                                let prop_index =
                                    flat_graph.vertex_prop_index(label, &p.property)?;
                                Some(ResolvedPredicate {
                                    predicate_id: p.predicate_id,
                                    prop_index,
                                    op: p.op.clone(),
                                    value: p.value.clone(),
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };

                    steps.push(FlatCompiledStep::Vertex {
                        label: label.clone(),
                        predicates,
                    });
                }
                PathElement::Edge {
                    label,
                    position,
                    direction,
                } => {
                    let predicates = if let Some(preds) = path_query.edge_predicates.get(position) {
                        preds
                            .iter()
                            .filter_map(|p| {
                                let prop_index = flat_graph.edge_prop_index(label, &p.property)?;
                                Some(ResolvedPredicate {
                                    predicate_id: p.predicate_id,
                                    prop_index,
                                    op: p.op.clone(),
                                    value: p.value.clone(),
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };

                    steps.push(FlatCompiledStep::Edge {
                        edge_label: label.clone(),
                        direction: direction.clone(),
                        predicates,
                    });
                }
            }
        }

        Ok(FlatCompiledPathQuery { steps, src_label })
    }
}

// ── FlatGraph methods on QueryGraph ──────────────────────────────────────────

impl QueryGraph {
    // ── Public entry point ────────────────────────────────────────────────────

    /// Build abstract graphs using [`FlatGraph`] for Wander Join sampling.
    ///
    /// This is a drop-in replacement for [`build_abstract_graph`] that does not
    /// require a `MemoryGraph` / `MemTransaction`.  Pass `flat_graph = None` to
    /// skip predicate sampling entirely (INNER predicates are treated as IGNORE).
    pub fn build_abstract_graph_flat(
        &self,
        k: usize,
        tree_num: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
    ) -> GCardResult<Vec<(AbstractGraph, u32)>> {
        let selectivity_cache: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        // String-keyed vertex sample cache (label → sampled vertex IDs).
        let flat_vertex_cache: Arc<DashMap<String, Vec<VertexId>>> = Arc::new(DashMap::new());
        let pred_cache: Arc<DashMap<(u32, u64), bool>> = Arc::new(DashMap::new());

        let t_cycle = std::time::Instant::now();
        let has_cycle = self.has_cycle();
        BUILD_CYCLE_CHECK_NANOS.fetch_add(t_cycle.elapsed().as_nanos() as u64, Ordering::Relaxed);

        if !has_cycle {
            let abstract_graph = self.build_abstract_graph_from_query_graph_flat(
                self,
                k,
                degree_seq_graph,
                flat_graph,
                sample_size,
                predicate_apply_type,
                &selectivity_cache,
                &flat_vertex_cache,
                &pred_cache,
            )?;
            return Ok(vec![(abstract_graph, 0)]);
        }

        // ── Cycle case: enumerate spanning trees ──────────────────────────────
        let t_tree = std::time::Instant::now();
        let scores = self.score_edges(k);
        let trees_with_scores = self.build_k_best_trees(&scores, tree_num.max(1));
        BUILD_SCORE_TREE_NANOS.fetch_add(t_tree.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let results: Vec<GCardResult<(AbstractGraph, u32)>> = trees_with_scores
            .par_iter()
            .map(|(tree, tree_score)| {
                self.build_abstract_graph_from_query_graph_flat(
                    tree,
                    k,
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    &selectivity_cache,
                    &flat_vertex_cache,
                    &pred_cache,
                )
                .map(|ag| (ag, *tree_score))
            })
            .collect();

        let mut abstract_graphs = Vec::new();
        for r in results {
            abstract_graphs.push(r?);
        }
        Ok(abstract_graphs)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn build_abstract_graph_from_query_graph_flat(
        &self,
        query_graph: &QueryGraph,
        k: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        selectivity_cache: &Arc<DashMap<String, f64>>,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<AbstractGraph> {
        let t_pivot = std::time::Instant::now();
        let pivot_nodes = query_graph.find_pivot_nodes();
        let paths = query_graph.find_paths_from_pivots(&pivot_nodes);
        BUILD_PIVOT_PATH_NANOS.fetch_add(t_pivot.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let t_ae = std::time::Instant::now();
        let mut all_abstract_edges: Vec<AbstractEdge> = Vec::new();
        for path in paths {
            let abstract_edges = query_graph.build_abstract_edges_for_path(&path, k)?;
            all_abstract_edges.extend(abstract_edges);
        }
        BUILD_ABSTRACT_EDGE_NANOS.fetch_add(t_ae.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let results: Vec<GCardResult<AbstractEdge>> = all_abstract_edges
            .into_par_iter()
            .map(|mut abstract_edge| {
                self.fill_pcf_for_abstract_edge_flat(
                    &mut abstract_edge,
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                )?;
                Ok(abstract_edge)
            })
            .collect();

        let mut abstract_graph = AbstractGraph::new();
        let mut next_edge_id: EdgeId = 1;
        for result in results {
            let abstract_edge = result?;
            let src_vertex = self.get_vertex(abstract_edge.src).unwrap();
            let dst_vertex = self.get_vertex(abstract_edge.dst).unwrap();
            if abstract_graph.get_vertex(abstract_edge.src).is_none() {
                abstract_graph.add_vertex(src_vertex.clone());
            }
            if abstract_graph.get_vertex(abstract_edge.dst).is_none() {
                abstract_graph.add_vertex(dst_vertex.clone());
            }
            abstract_graph.add_edge(next_edge_id, abstract_edge);
            next_edge_id += 1;
        }

        Ok(abstract_graph)
    }

    fn fill_pcf_for_abstract_edge_flat(
        &self,
        abstract_edge: &mut AbstractEdge,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        selectivity_cache: &Arc<DashMap<String, f64>>,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<()> {
        // ── Build alt-key and label lists (identical to DB path) ──────────────
        let mut node_labels = Vec::new();
        for &vertex_id in &abstract_edge.path_vertices {
            if let Some(vertex) = self.inner.vertices.get(&vertex_id) {
                node_labels.push(vertex.label.clone());
            } else {
                return Err(GCardError::VertexNotFound(format!(
                    "Vertex {} not found",
                    vertex_id
                )));
            }
        }

        let mut edge_labels = Vec::new();
        for &edge_id in &abstract_edge.original_edge_ids {
            if let Some(edge) = self.inner.edges.get(&edge_id) {
                edge_labels.push(edge.label.clone());
            } else {
                return Err(GCardError::EdgeNotFound(format!(
                    "Edge {} not found",
                    edge_id
                )));
            }
        }

        let alt_key = make_alt_key(&node_labels, &edge_labels);

        let src_label = self
            .inner
            .vertices
            .get(&abstract_edge.src)
            .ok_or_else(|| {
                GCardError::VertexNotFound(format!("Source vertex {} not found", abstract_edge.src))
            })?
            .label
            .clone();

        let dst_label = self
            .inner
            .vertices
            .get(&abstract_edge.dst)
            .ok_or_else(|| {
                GCardError::VertexNotFound(format!(
                    "Destination vertex {} not found",
                    abstract_edge.dst
                ))
            })?
            .label
            .clone();

        let t_pcf = std::time::Instant::now();
        let mut src_pcf_func = degree_seq_graph.get_piece_func_by_path(&alt_key, &src_label);
        let mut dst_pcf_func = degree_seq_graph.get_piece_func_by_path(&alt_key, &dst_label);
        BUILD_PCF_LOOKUP_NANOS.fetch_add(t_pcf.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let mut output = String::new();
        for i in 0..node_labels.len() {
            output.push_str(&format!("{} -> ", node_labels[i]));
            if i < edge_labels.len() {
                output.push_str(&format!("{} ->", edge_labels[i]));
            }
        }
        abstract_edge.path_str = output;

        // ── Predicate sampling (FlatGraph path) ───────────────────────────────
        if let Some(fg) = flat_graph {
            let selectivity = if !abstract_edge.predicates.is_empty()
                && matches!(predicate_apply_type, PredicateApplyType::INNER)
            {
                let cache_key =
                    Self::build_selectivity_cache_key(&alt_key, &abstract_edge.predicates);
                if let Some(cached) = selectivity_cache.get(&cache_key) {
                    *cached
                } else {
                    crate::procedures::gcard_query::SAMPLING_CALLS
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    let sel = self.compute_selectivity_flat(
                        fg,
                        abstract_edge,
                        sample_size,
                        flat_vertex_cache,
                        pred_cache,
                    )?;
                    crate::procedures::gcard_query::SAMPLING_NANOS.fetch_add(
                        t0.elapsed().as_nanos() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    selectivity_cache.insert(cache_key, sel);
                    sel
                }
            } else {
                1.0f64
            };

            abstract_edge.selectivity = selectivity;
            if matches!(predicate_apply_type, INNER) {
                src_pcf_func = src_pcf_func.truncate_by_ratio(selectivity);
                dst_pcf_func = dst_pcf_func.truncate_by_ratio(selectivity);
            }
        }

        abstract_edge.src_pcf = Arc::new(src_pcf_func);
        abstract_edge.dst_pcf = Arc::new(dst_pcf_func);

        Ok(())
    }

    fn compute_selectivity_flat(
        &self,
        flat_graph: &FlatGraph,
        abstract_edge: &AbstractEdge,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<f64> {
        let path_query = self.build_path_query(abstract_edge)?;
        let compiled = FlatCompiledPathQuery::compile_flat(&path_query, flat_graph)?;

        // Sample start vertices from FlatGraph using the source label.
        let sampled_starts = {
            let label = &compiled.src_label;
            if let Some(cached) = flat_vertex_cache.get(label) {
                cached.clone()
            } else {
                let mut rng = rand::thread_rng();
                let vids = flat_graph.sample_vertices_by_label(label, sample_size, &mut rng);
                flat_vertex_cache.insert(label.clone(), vids.clone());
                vids
            }
        };

        if sampled_starts.is_empty() {
            return Ok(0.0);
        }

        const PILOT_WALKS: usize = 5;
        const MAX_WALKS: usize = 50;
        const CV_THRESHOLD: f64 = 0.3;
        const CV_UPPER_BOUND: f64 = 3.0;
        const BATCH_SIZE: usize = 128;

        let struct_count_min: usize = 300;
        let struct_count_max: usize = 4_000;
        let rel_eps: f64 = 0.10;
        let z_95: f64 = 1.96;
        let eps0: f64 = 1e-12;

        let mut struct_success_sample_count: usize = 0;
        let mut sum_struct_weight: f64 = 0.0;
        let mut sum_pred_weight: f64 = 0.0;
        let mut sum_struct_weight_sq: f64 = 0.0;
        let mut sum_pred_weight_sq: f64 = 0.0;
        let mut sum_cross_weight: f64 = 0.0;
        let mut total_vertices_visited: usize = 0;
        let mut converged = false;

        for batch in sampled_starts.chunks(BATCH_SIZE) {
            let batch_results: Vec<(Option<(f64, f64)>, WalkProf)> = batch
                .par_iter()
                .map(|&start_vid| {
                    use rand::Rng;
                    let mut rng = rand::thread_rng();
                    // Cache: (vid, edge_label, outgoing) → neighbors with edge IDs.
                    let mut nbr_cache: HashMap<(VertexId, String, bool), Vec<(VertexId, EdgeId)>> =
                        HashMap::new();
                    let mut prof = WalkProf::default();
                    let mut local_cache: HashMap<(u32, u64), bool> = HashMap::new();

                    let mut walk_struct: Vec<f64> = Vec::with_capacity(PILOT_WALKS);
                    let mut walk_pred: Vec<f64> = Vec::with_capacity(PILOT_WALKS);

                    for _ in 0..PILOT_WALKS {
                        prof.walk_count += 1;
                        let r = self.execute_flat_walk(
                            flat_graph,
                            &compiled,
                            start_vid,
                            &mut rng,
                            &mut nbr_cache,
                            &mut prof,
                            &mut local_cache,
                        );
                        let (sw, pw) = match r {
                            Ok(v) => v,
                            Err(_) => {
                                Self::flush_local_cache(&local_cache, pred_cache);
                                return (None, prof);
                            }
                        };
                        // Include dead-end walks (sw=0) in the average instead of
                        // discarding the start vertex. Dead ends on intermediate hops
                        // are common in sparse graphs (e.g. 87% of posts have no
                        // comment replies). Early exit biases the estimator; only
                        // discard after ALL pilot walks fail.
                        walk_struct.push(sw);
                        walk_pred.push(pw);
                    }

                    let pilot_mean = walk_struct.iter().sum::<f64>() / walk_struct.len() as f64;
                    // If ALL pilot walks were structural dead ends, this start vertex
                    // has no valid paths — skip it entirely.
                    if pilot_mean == 0.0 {
                        Self::flush_local_cache(&local_cache, pred_cache);
                        return (None, prof);
                    }
                    if pilot_mean > 0.0 {
                        let variance = walk_struct
                            .iter()
                            .map(|&w| (w - pilot_mean) * (w - pilot_mean))
                            .sum::<f64>()
                            / (walk_struct.len() as f64 - 1.0).max(1.0);
                        let cv = variance.sqrt() / pilot_mean;
                        if cv > CV_THRESHOLD && cv <= CV_UPPER_BOUND {
                            for _ in 0..(MAX_WALKS - PILOT_WALKS) {
                                prof.walk_count += 1;
                                let r = self.execute_flat_walk(
                                    flat_graph,
                                    &compiled,
                                    start_vid,
                                    &mut rng,
                                    &mut nbr_cache,
                                    &mut prof,
                                    &mut local_cache,
                                );
                                let (sw, pw) = match r {
                                    Ok(v) => v,
                                    Err(_) => {
                                        Self::flush_local_cache(&local_cache, pred_cache);
                                        return (None, prof);
                                    }
                                };
                                walk_struct.push(sw);
                                walk_pred.push(pw);
                            }
                        }
                    }

                    Self::flush_local_cache(&local_cache, pred_cache);

                    let total_walks = walk_struct.len() as f64;
                    let struct_weight = walk_struct.iter().sum::<f64>() / total_walks;
                    let pred_weight = walk_pred.iter().sum::<f64>() / total_walks;
                    let result = if struct_weight > 0.0 {
                        Some((struct_weight, pred_weight))
                    } else {
                        None
                    };
                    (result, prof)
                })
                .collect();

            total_vertices_visited += batch_results.len();
            for (_, prof) in &batch_results {
                crate::procedures::gcard_query::SAMPLING_TOTAL_WALKS
                    .fetch_add(prof.walk_count, std::sync::atomic::Ordering::Relaxed);
                crate::procedures::gcard_query::SAMPLING_NBR_NANOS
                    .fetch_add(prof.nbr_nanos, std::sync::atomic::Ordering::Relaxed);
                crate::procedures::gcard_query::SAMPLING_PROP_NANOS
                    .fetch_add(prof.prop_nanos, std::sync::atomic::Ordering::Relaxed);
            }

            for (result, _) in &batch_results {
                let &Some((struct_weight, pred_weight)) = result else {
                    continue;
                };
                struct_success_sample_count += 1;
                sum_struct_weight += struct_weight;
                sum_pred_weight += pred_weight;
                sum_struct_weight_sq += struct_weight * struct_weight;
                sum_pred_weight_sq += pred_weight * pred_weight;
                sum_cross_weight += struct_weight * pred_weight;

                if struct_success_sample_count >= struct_count_max {
                    converged = true;
                    break;
                }
            }

            if converged {
                break;
            }

            if struct_success_sample_count >= struct_count_min {
                let k = struct_success_sample_count as f64;
                let mean_struct = sum_struct_weight / k;
                let mean_pred = sum_pred_weight / k;
                let denom = k - 1.0;
                if denom > 0.0 && mean_struct.abs() > eps0 {
                    let selectivity_est = mean_pred / mean_struct;
                    let var_pred = (sum_pred_weight_sq - k * mean_pred * mean_pred) / denom;
                    let var_struct = (sum_struct_weight_sq - k * mean_struct * mean_struct) / denom;
                    let cov = (sum_cross_weight - k * mean_pred * mean_struct) / denom;
                    let mu_x = mean_pred;
                    let mu_y = mean_struct;
                    let se_sq = (var_pred / (mu_y * mu_y)
                        + mu_x * mu_x * var_struct / mu_y.powi(4)
                        - 2.0 * mu_x * cov / mu_y.powi(3))
                        / k;
                    if se_sq.is_finite() && se_sq >= 0.0 {
                        let ci_half = z_95 * se_sq.sqrt();
                        if ci_half / selectivity_est.abs().max(eps0) <= rel_eps {
                            break;
                        }
                    }
                }
            }
        }

        crate::procedures::gcard_query::SAMPLING_VERTICES_PROCESSED.fetch_add(
            total_vertices_visited as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        let selectivity = if sum_struct_weight > 0.0 {
            sum_pred_weight / sum_struct_weight
        } else {
            0.0
        };

        Ok(selectivity)
    }

    /// Execute a single Wander Join walk over [`FlatGraph`].
    ///
    /// Returns `(struct_weight, pred_weight)` — the structural weight and the
    /// predicate-filtered weight for this walk.  `(0.0, 0.0)` means the walk
    /// failed (dead end or predicate failure).
    fn execute_flat_walk(
        &self,
        flat_graph: &FlatGraph,
        compiled: &FlatCompiledPathQuery,
        start_vertex: VertexId,
        rng: &mut impl rand::Rng,
        nbr_cache: &mut HashMap<(VertexId, String, bool), Vec<(VertexId, EdgeId)>>,
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<(f64, f64)> {
        if compiled.steps.is_empty() {
            return Ok((1.0, 1.0));
        }

        let mut current_vid = start_vertex;
        let mut weight: f64 = 1.0;
        let mut pred_ok = true;

        for step in &compiled.steps {
            match step {
                FlatCompiledStep::Vertex {
                    label: _,
                    predicates,
                } => {
                    if pred_ok && !predicates.is_empty() {
                        let t0 = std::time::Instant::now();
                        let props = flat_graph.vertex_props(current_vid);
                        prof.prop_nanos += t0.elapsed().as_nanos() as u64;

                        let Some(props) = props else {
                            return Ok((0.0, 0.0));
                        };

                        for rp in predicates {
                            // Check local cache first.
                            let pass = if let Some(pid) = rp.predicate_id {
                                if let Some(&cached) = local_pred_cache.get(&(pid, current_vid)) {
                                    cached
                                } else {
                                    let p = match props.get(rp.prop_index) {
                                        Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                                        None => false,
                                    };
                                    local_pred_cache.insert((pid, current_vid), p);
                                    p
                                }
                            } else {
                                match props.get(rp.prop_index) {
                                    Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                                    None => false,
                                }
                            };
                            if !pass {
                                pred_ok = false;
                                break;
                            }
                        }
                    }
                }
                FlatCompiledStep::Edge {
                    edge_label,
                    direction,
                    predicates,
                } => {
                    let outgoing = matches!(direction, EdgeDirection::Outgoing);
                    let cache_key = (current_vid, edge_label.clone(), outgoing);

                    // Fetch and cache neighbors (with edge IDs).
                    if !nbr_cache.contains_key(&cache_key) {
                        let t0 = std::time::Instant::now();
                        let slice =
                            flat_graph.neighbors_with_eid(current_vid, edge_label, outgoing);
                        prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                        nbr_cache.insert(cache_key.clone(), slice.to_vec());
                    }
                    let nbrs = &nbr_cache[&cache_key];

                    let degree = nbrs.len();
                    if degree == 0 {
                        return Ok((0.0, 0.0));
                    }
                    let idx = rng.gen_range(0..degree);
                    let (chosen_nbr, chosen_eid) = nbrs[idx];
                    weight *= degree as f64;
                    current_vid = chosen_nbr;

                    // Check edge predicates.
                    if pred_ok && !predicates.is_empty() {
                        let t0 = std::time::Instant::now();
                        let eprops = flat_graph.edge_props(chosen_eid);
                        prof.prop_nanos += t0.elapsed().as_nanos() as u64;

                        let Some(eprops) = eprops else {
                            pred_ok = false;
                            continue;
                        };

                        for rp in predicates {
                            let pass = if let Some(pid) = rp.predicate_id {
                                if let Some(&cached) = local_pred_cache.get(&(pid, chosen_eid)) {
                                    cached
                                } else {
                                    let p = match eprops.get(rp.prop_index) {
                                        Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                                        None => false,
                                    };
                                    local_pred_cache.insert((pid, chosen_eid), p);
                                    p
                                }
                            } else {
                                match eprops.get(rp.prop_index) {
                                    Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                                    None => false,
                                }
                            };
                            if !pass {
                                pred_ok = false;
                                break;
                            }
                        }
                    }
                }
            }
        }

        let struct_weight = weight;
        let pred_weight = if pred_ok { weight } else { 0.0 };
        Ok((struct_weight, pred_weight))
    }
}
