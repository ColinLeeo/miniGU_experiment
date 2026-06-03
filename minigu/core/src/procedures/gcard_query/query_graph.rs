use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use dashmap::DashMap;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;

use crate::procedures::gcard_query::PredicateApplyType::INNER;
use crate::procedures::gcard_query::abs_graph::AbstractGraph;
use crate::procedures::gcard_query::catalog::{
    DegreeSeqGraphCompressed, EdgeCardinality, make_alt_key,
};
use crate::procedures::gcard_query::degreepiecewise::Pcf;
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::flat_graph::FlatGraph;
use crate::procedures::gcard_query::graph::{Endpoints, GraphSkeleton};
use crate::procedures::gcard_query::types::{
    AbstractEdge, AbstractEdgeDef, CandidateTree, ComparisonOp, DecompositionDef,
    FunctionalDirection, PredicateDef, PredicateId, PredicateLocation,
};
use crate::procedures::gcard_query::union_find::UnionFind;
use crate::procedures::gcard_query::utils::{PathPattern, StarStatKey, manual_edge_cardinality};
use crate::procedures::gcard_query::{
    BUILD_ABSTRACT_EDGE_NANOS, BUILD_CYCLE_CHECK_NANOS, BUILD_PCF_LOOKUP_NANOS,
    BUILD_PIVOT_PATH_NANOS, BUILD_SCORE_TREE_NANOS, GCARD_MAX_STAR_DEGREE_OVERRIDE,
    GCARD_MAX_STAR_LENGTH_OVERRIDE, GCARD_STAR_CONFIG_UNSET, PredicateApplyType,
    functional_refine_enabled,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedures::gcard_query::catalog::CompressedDegreeSeq;
    use crate::procedures::gcard_query::degreepiecewise::PiecewiseConstantFunction;

    fn vertex(id: VertexId, label: &str) -> crate::procedures::gcard_query::types::VertexDef {
        crate::procedures::gcard_query::types::VertexDef {
            id,
            label: label.to_string(),
        }
    }

    fn edge(
        id: EdgeId,
        label: &str,
        src: VertexId,
        dst: VertexId,
    ) -> crate::procedures::gcard_query::types::EdgeDef {
        crate::procedures::gcard_query::types::EdgeDef {
            id,
            label: label.to_string(),
            src,
            dst,
        }
    }

    #[test]
    fn star_degree_sequence_is_applied_as_center_local_pcf() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![
                vertex(1, "center"),
                vertex(2, "a"),
                vertex(3, "b"),
                vertex(4, "c"),
            ],
            edges: vec![
                edge(10, "e1", 1, 2),
                edge(11, "e2", 1, 3),
                edge(12, "e3", 1, 4),
            ],
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();

        let star_key = StarStatKey::new(
            "center".to_string(),
            vec![
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "b".to_string()],
                    vec!["e2".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "c".to_string()],
                    vec!["e3".to_string()],
                ),
            ],
        );
        let star_pcf = PiecewiseConstantFunction {
            constants: vec![7.0],
            right_interval_edges: vec![6.0],
            cumulative_rows: vec![42.0],
        };
        let mut degree_seq_graph = DegreeSeqGraphCompressed::new();
        degree_seq_graph.star_stats.insert(
            star_key,
            CompressedDegreeSeq::SafeBound { function: star_pcf },
        );

        let mut abstract_graphs = query_graph
            .build_abstract_graph_flat(
                1,
                1,
                &degree_seq_graph,
                None,
                0,
                &PredicateApplyType::IGNORE,
                false,
            )
            .unwrap();

        let (abstract_graph, _) = abstract_graphs
            .iter_mut()
            .find(|(graph, _)| graph.edges.is_empty())
            .expect("star-covered leaf arms should produce a local-star-only candidate");
        assert!(
            abstract_graph.edges.is_empty(),
            "star-covered leaf arms should not remain as path abstract edges"
        );
        assert_eq!(abstract_graph.local_pcfs.get(&1).unwrap().len(), 1);
        assert_eq!(abstract_graph.get_es().unwrap(), 42.0);
    }

    #[test]
    fn raw_star_does_not_consume_non_leaf_arms() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![
                vertex(1, "center"),
                vertex(2, "leaf"),
                vertex(3, "mid"),
                vertex(4, "tail"),
            ],
            edges: vec![
                edge(10, "e_leaf", 1, 2),
                edge(11, "e_mid", 1, 3),
                edge(12, "e_tail", 3, 4),
            ],
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();

        let star_key = StarStatKey::new(
            "center".to_string(),
            vec![
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "leaf".to_string()],
                    vec!["e_leaf".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "mid".to_string()],
                    vec!["e_mid".to_string()],
                ),
            ],
        );
        let mut degree_seq_graph = DegreeSeqGraphCompressed::new();
        degree_seq_graph.star_stats.insert(
            star_key,
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction {
                    constants: vec![7.0],
                    right_interval_edges: vec![6.0],
                    cumulative_rows: vec![42.0],
                },
            },
        );

        assert!(
            query_graph
                .extract_raw_star_local_pcfs(&degree_seq_graph)
                .is_none(),
            "local raw-star PCF must not hide a non-leaf endpoint that still joins residual edges"
        );
    }

    #[test]
    fn star_matching_deduplicates_repeated_leaf_arms_like_pathce() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![
                vertex(1, "center"),
                vertex(2, "a"),
                vertex(3, "a"),
                vertex(4, "b"),
            ],
            edges: vec![
                edge(10, "e1", 1, 2),
                edge(11, "e1", 1, 3),
                edge(12, "e2", 1, 4),
            ],
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();

        let dedup_key = StarStatKey::new(
            "center".to_string(),
            vec![
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "b".to_string()],
                    vec!["e2".to_string()],
                ),
            ],
        );
        let repeated_key = StarStatKey::new(
            "center".to_string(),
            vec![
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "b".to_string()],
                    vec!["e2".to_string()],
                ),
            ],
        );
        let mut degree_seq_graph = DegreeSeqGraphCompressed::new();
        degree_seq_graph.star_stats.insert(
            dedup_key,
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction {
                    constants: vec![7.0],
                    right_interval_edges: vec![6.0],
                    cumulative_rows: vec![42.0],
                },
            },
        );
        degree_seq_graph.star_stats.insert(
            repeated_key,
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction {
                    constants: vec![11.0],
                    right_interval_edges: vec![9.0],
                    cumulative_rows: vec![99.0],
                },
            },
        );

        let abstract_graphs = query_graph
            .build_abstract_graph_flat(
                1,
                1,
                &degree_seq_graph,
                None,
                0,
                &PredicateApplyType::IGNORE,
                false,
            )
            .unwrap();

        let (abstract_graph, _) = abstract_graphs.first().unwrap();
        let local_pcfs = abstract_graph.local_pcfs.get(&1).unwrap();
        assert_eq!(
            local_pcfs.len(),
            1,
            "degree-1 star fallback is disabled; duplicate arm should remain as a path edge"
        );
        assert_eq!(local_pcfs[0].get_num_rows(), 42.0);
        assert_eq!(abstract_graph.edges.len(), 1);
    }

    #[test]
    fn unit_path_queries_split_abstract_edge_into_length_one_hops() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "a"), vertex(2, "b"), vertex(3, "c")],
            edges: vec![edge(10, "ab", 1, 2), edge(11, "bc", 2, 3)],
            predicates: vec![
                PredicateDef {
                    predicate_id: Some(1),
                    target: "vertex".to_string(),
                    id: 1,
                    property: "p".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int32(Some(1)),
                },
                PredicateDef {
                    predicate_id: Some(2),
                    target: "vertex".to_string(),
                    id: 2,
                    property: "p".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int32(Some(2)),
                },
                PredicateDef {
                    predicate_id: Some(3),
                    target: "vertex".to_string(),
                    id: 3,
                    property: "p".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int32(Some(3)),
                },
                PredicateDef {
                    predicate_id: Some(4),
                    target: "edge".to_string(),
                    id: 10,
                    property: "q".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int32(Some(10)),
                },
                PredicateDef {
                    predicate_id: Some(5),
                    target: "edge".to_string(),
                    id: 11,
                    property: "q".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int32(Some(11)),
                },
            ],
        };
        let query_graph = query.build_graph().unwrap();
        let abstract_edge = query_graph
            .build_abstract_edge_from_def(&AbstractEdgeDef {
                path_vertices: vec![1, 2, 3],
                original_edge_ids: vec![10, 11],
            })
            .unwrap();

        let unit_queries = query_graph.build_unit_path_queries(&abstract_edge).unwrap();

        assert_eq!(unit_queries.len(), 2);
        assert_eq!(unit_queries[0].path_elements.len(), 3);
        assert_eq!(unit_queries[1].path_elements.len(), 3);

        let mut hop0_vertex_positions: Vec<_> =
            unit_queries[0].vertex_predicates.keys().copied().collect();
        let mut hop1_vertex_positions: Vec<_> =
            unit_queries[1].vertex_predicates.keys().copied().collect();
        hop0_vertex_positions.sort_unstable();
        hop1_vertex_positions.sort_unstable();
        assert_eq!(hop0_vertex_positions, vec![0]);
        assert_eq!(hop1_vertex_positions, vec![0, 2]);
        assert_eq!(unit_queries[0].edge_predicates.get(&1).unwrap().len(), 1);
        assert_eq!(unit_queries[1].edge_predicates.get(&1).unwrap().len(), 1);
    }

    #[test]
    fn star_matching_falls_back_to_smaller_available_degree() {
        let arms = vec![
            (
                0,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
            ),
            (
                1,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "b".to_string()],
                    vec!["e2".to_string()],
                ),
            ),
            (
                2,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "c".to_string()],
                    vec!["e3".to_string()],
                ),
            ),
            (
                3,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "d".to_string()],
                    vec!["e4".to_string()],
                ),
            ),
        ];

        let fallback_key = StarStatKey::new(
            "center".to_string(),
            vec![arms[1].1.clone(), arms[2].1.clone(), arms[3].1.clone()],
        );
        let fallback_pcf = PiecewiseConstantFunction {
            constants: vec![13.0],
            right_interval_edges: vec![12.0],
            cumulative_rows: vec![99.0],
        };
        let mut degree_seq_graph = DegreeSeqGraphCompressed::new();
        degree_seq_graph.star_stats.insert(
            fallback_key,
            CompressedDegreeSeq::SafeBound {
                function: fallback_pcf,
            },
        );

        let (selected, pcf) =
            QueryGraph::find_matching_star_pcf("center", &arms, 4, &degree_seq_graph)
                .expect("d4 miss should fall back to an available d3 star");

        assert_eq!(selected, vec![1, 2, 3]);
        assert_eq!(pcf.get_num_rows(), 99.0);
    }

    #[test]
    fn star_matching_searches_beyond_first_max_degree_arms() {
        let arms = vec![
            (
                0,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "a".to_string()],
                    vec!["e1".to_string()],
                ),
            ),
            (
                1,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "b".to_string()],
                    vec!["e2".to_string()],
                ),
            ),
            (
                2,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "c".to_string()],
                    vec!["e3".to_string()],
                ),
            ),
            (
                3,
                PathPattern::new_without_reverse(
                    vec!["center".to_string(), "d".to_string()],
                    vec!["e4".to_string()],
                ),
            ),
        ];

        let matched_key = StarStatKey::new(
            "center".to_string(),
            vec![arms[1].1.clone(), arms[2].1.clone(), arms[3].1.clone()],
        );
        let mut degree_seq_graph = DegreeSeqGraphCompressed::new();
        degree_seq_graph.star_stats.insert(
            matched_key,
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction {
                    constants: vec![17.0],
                    right_interval_edges: vec![2.0],
                    cumulative_rows: vec![34.0],
                },
            },
        );

        let (selected, pcf) =
            QueryGraph::find_matching_star_pcf("center", &arms, 3, &degree_seq_graph)
                .expect("matching should search all degree-3 arm combinations, not just a/b/c");

        assert_eq!(selected, vec![1, 2, 3]);
        assert_eq!(pcf.get_num_rows(), 34.0);
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
                local_pcfs: HashMap::new(),
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

    /// Estimate the cardinality of each query edge using FlatGraph statistics.
    ///
    /// For each edge, the base cardinality is the edge count for that label.
    /// If the edge or its endpoints carry predicates, selectivity is applied
    /// using the independence assumption:
    ///   effective = edge_count × sel(edge_preds) × sel(src_preds) × sel(dst_preds)
    pub fn estimate_edge_cardinalities(
        &self,
        flat_graph: Option<&FlatGraph>,
    ) -> HashMap<EdgeId, u64> {
        let mut result = HashMap::new();
        for edge in self.inner.edges.values() {
            let base = flat_graph
                .map(|fg| fg.edge_count_by_label(&edge.label) as u64)
                .unwrap_or(1);

            let mut selectivity = 1.0f64;

            if let Some(fg) = flat_graph {
                // Edge predicates.
                for pred in &edge.predicates {
                    selectivity *= Self::estimate_selectivity_from_stats(
                        fg.edge_column_stats(&edge.label, &pred.property),
                        &pred.op,
                        &pred.value,
                    );
                }

                // Source vertex predicates.
                if let Some(src_vertex) = self.inner.vertices.get(&edge.src_vertex_id) {
                    for pred in &src_vertex.predicates {
                        selectivity *= Self::estimate_selectivity_from_stats(
                            fg.vertex_column_stats(&src_vertex.label, &pred.property),
                            &pred.op,
                            &pred.value,
                        );
                    }
                }

                // Destination vertex predicates.
                if let Some(dst_vertex) = self.inner.vertices.get(&edge.dst_vertex_id) {
                    for pred in &dst_vertex.predicates {
                        selectivity *= Self::estimate_selectivity_from_stats(
                            fg.vertex_column_stats(&dst_vertex.label, &pred.property),
                            &pred.op,
                            &pred.value,
                        );
                    }
                }
            }

            let effective = (base as f64 * selectivity).ceil().max(1.0) as u64;
            result.insert(edge.id, effective);
        }
        result
    }

    /// Estimate selectivity of a single predicate using column statistics.
    fn estimate_selectivity_from_stats(
        col_stats: Option<&super::flat_graph::stats::ColumnStats>,
        op: &ComparisonOp,
        value: &ScalarValue,
    ) -> f64 {
        use super::flat_graph::stats::cmp_scalar;

        let Some(stats) = col_stats else {
            // No stats available — assume Kuzu's default.
            return match op {
                ComparisonOp::Eq => 0.01,
                _ => 0.1,
            };
        };

        let non_null = stats.total_count - stats.null_count;
        if non_null == 0 {
            return 0.0;
        }

        match op {
            ComparisonOp::Eq | ComparisonOp::Ne => {
                let ndv = stats.ndv().max(1);
                let sel = 1.0 / ndv as f64;
                if matches!(op, ComparisonOp::Ne) {
                    1.0 - sel
                } else {
                    sel
                }
            }
            ComparisonOp::Gt | ComparisonOp::Ge | ComparisonOp::Lt | ComparisonOp::Le => {
                // Uniform distribution assumption: sel = (max - value) / (max - min).
                let (Some(min_val), Some(max_val)) = (&stats.min, &stats.max) else {
                    return 0.1; // fallback
                };
                let to_f64 = |v: &ScalarValue| -> Option<f64> {
                    use ScalarValue::*;
                    match v {
                        Int8(Some(n)) => Some(*n as f64),
                        Int16(Some(n)) => Some(*n as f64),
                        Int32(Some(n)) => Some(*n as f64),
                        Int64(Some(n)) => Some(*n as f64),
                        UInt8(Some(n)) => Some(*n as f64),
                        UInt16(Some(n)) => Some(*n as f64),
                        UInt32(Some(n)) => Some(*n as f64),
                        UInt64(Some(n)) => Some(*n as f64),
                        Float32(Some(n)) => Some(n.into_inner() as f64),
                        Float64(Some(n)) => Some(n.into_inner()),
                        _ => None,
                    }
                };
                let (Some(fmin), Some(fmax), Some(fval)) =
                    (to_f64(min_val), to_f64(max_val), to_f64(value))
                else {
                    // Non-numeric: compare against min/max boundaries.
                    return match op {
                        ComparisonOp::Gt | ComparisonOp::Ge => {
                            if cmp_scalar(value, max_val) != Some(std::cmp::Ordering::Less) {
                                0.0 // value >= max → nothing passes
                            } else {
                                0.5
                            }
                        }
                        ComparisonOp::Lt | ComparisonOp::Le => {
                            if cmp_scalar(value, min_val) != Some(std::cmp::Ordering::Greater) {
                                0.0
                            } else {
                                0.5
                            }
                        }
                        _ => 0.5,
                    };
                };
                let range = fmax - fmin;
                if range <= 0.0 {
                    return if fval >= fmin && fval <= fmax {
                        1.0
                    } else {
                        0.0
                    };
                }
                let sel = match op {
                    ComparisonOp::Gt | ComparisonOp::Ge => ((fmax - fval) / range).clamp(0.0, 1.0),
                    ComparisonOp::Lt | ComparisonOp::Le => ((fval - fmin) / range).clamp(0.0, 1.0),
                    _ => 0.5,
                };
                sel
            }
        }
    }

    pub fn find_k_path_edges_between_predicates(
        &self,
        predicate_vertices: &HashSet<VertexId>,
        k: usize,
    ) -> HashSet<EdgeId> {
        // 语义上是在问：
        // “哪些边位于两个带谓词顶点之间、长度不超过 k 的路径上？”
        // 这些边通常值得优先保留，因为它们更可能影响选择率传播。
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
        // 用显式栈做 DFS，而不是递归：
        // 1. 避免深路径时递归栈增长；
        // 2. 更容易把“当前边集合”和回溯过程写清楚。
        if k == 0 {
            if start == end {
                return vec![HashSet::new()];
            }
            return vec![];
        }
        let mut result = Vec::new();
        let mut path_edges = HashSet::new();
        let mut visited = HashSet::from([start]);
        let mut stack: Vec<(VertexId, Vec<(VertexId, EdgeId)>, usize)> = Vec::new();

        let neighbors = self.get_neighbor_edge_pairs(start);
        stack.push((start, neighbors, 0));

        while let Some(frame) = stack.last_mut() {
            let (_current, ref nbrs, ref mut idx) = *frame;
            if *idx >= nbrs.len() {
                // 当前点的邻居已经尝试完，开始回溯，并撤销把它放进路径时的状态。
                let (v, _, _) = stack.pop().unwrap();
                if let Some(parent) = stack.last() {
                    // 移除“父节点 -> 当前节点”这条路径边，恢复到进入该节点之前。
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

    /// 返回一个点的所有 `(邻居顶点, 连接边 id)`。
    ///
    /// 注意这里会同时看入边和出边，等价于临时把查询图按无向图来遍历。
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

    /// Build the best spanning tree by greedily picking lowest-cardinality edges
    /// (Kruskal's algorithm, ascending cardinality order).
    pub fn build_best_spanning_tree(
        &self,
        cardinalities: &HashMap<EdgeId, u64>,
    ) -> Option<CandidateTree> {
        let mut edges_with_card: Vec<_> = self
            .inner
            .edges
            .values()
            .map(|edge| {
                let card = cardinalities.get(&edge.id).copied().unwrap_or(1);
                (edge.clone(), card)
            })
            .collect();
        // Sort ascending by cardinality: pick lowest-cardinality edges first → tightest tree.
        // Use edge id as deterministic tiebreaker.
        edges_with_card.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.id.cmp(&b.0.id)));

        let mut selected_edges = HashSet::new();
        let mut uf = UnionFind::new();
        let mut total_score: u64 = 0;

        for (edge, card) in edges_with_card {
            uf.make_set(edge.src_vertex_id);
            uf.make_set(edge.dst_vertex_id);

            if uf.union(edge.src_vertex_id, edge.dst_vertex_id) {
                selected_edges.insert(edge.id);
                total_score += card;
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
        // 把一组边重新包装成一个独立的 QueryGraph，
        // 便于后续把“候选树”继续送入抽象边构造逻辑。
        let mut subgraph_vertices = HashMap::new();
        let mut subgraph_edges = HashMap::new();
        let mut subgraph_outgoing = HashMap::new();
        let mut subgraph_incoming = HashMap::new();

        let mut selected_edge_ids_sorted: Vec<_> = selected_edge_ids.iter().copied().collect();
        selected_edge_ids_sorted.sort_unstable();
        for edge_id in selected_edge_ids_sorted {
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
                local_pcfs: HashMap::new(),
            },
            predicate_index: self.predicate_index.clone(),
        }
    }

    pub fn build_k_best_trees(
        &self,
        cardinalities: &HashMap<EdgeId, u64>,
        k: usize,
    ) -> Vec<(QueryGraph, u64)> {
        if k == 0 {
            return Vec::new();
        }

        let first_tree = self.build_best_spanning_tree(cardinalities);
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

            let mut all_edge_ids: Vec<EdgeId> = self.inner.edges.keys().copied().collect();
            all_edge_ids.sort_unstable();
            let mut non_tree_edges: Vec<(EdgeId, u64)> = all_edge_ids
                .into_iter()
                .filter(|eid| !current.edge_ids.contains(eid))
                .map(|eid| {
                    let card = cardinalities.get(&eid).copied().unwrap_or(1);
                    (eid, card)
                })
                .collect();

            // Try adding smallest non-tree edges first (smallest perturbation).
            // Use edge id as deterministic tiebreaker.
            non_tree_edges.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

            for (new_edge_id, _new_edge_card) in non_tree_edges {
                if let Some(new_edge) = self.inner.edges.get(&new_edge_id) {
                    let src = new_edge.src_vertex_id;
                    let dst = new_edge.dst_vertex_id;

                    if let Some(path_edges) =
                        self.find_path_edges_in_tree(&current.edge_ids, src, dst)
                    {
                        // 往树里加一条非树边会形成环。
                        // 为了保持树结构，必须从这条环上再删掉一条边。
                        // 优先删除基数最大的树边，使新树总基数尽量小。
                        let mut path_edges_with_card: Vec<(EdgeId, u64)> = path_edges
                            .iter()
                            .map(|&eid| {
                                let card = cardinalities.get(&eid).copied().unwrap_or(1);
                                (eid, card)
                            })
                            .collect();

                        // Remove largest-cardinality tree edge first (best swap).
                        // Use edge id as deterministic tiebreaker.
                        path_edges_with_card
                            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                        for (edge_to_remove, _old_card) in path_edges_with_card {
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
                                let total_score: u64 = new_edge_set
                                    .iter()
                                    .map(|&eid| cardinalities.get(&eid).copied().unwrap_or(1))
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
                            let total_score: u64 = new_edge_set
                                .iter()
                                .map(|&eid| cardinalities.get(&eid).copied().unwrap_or(1))
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
        // 依然用并查集做无向环检测。
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
        // 这里必须排序，保证逻辑上等价但输入顺序不同的谓词集合能命中同一缓存 key。
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
        self.build_path_query_from_parts(
            &abstract_edge.path_vertices,
            &abstract_edge.original_edge_ids,
            &abstract_edge.predicates,
        )
    }

    fn build_path_query_from_parts(
        &self,
        path_vertices: &[VertexId],
        original_edge_ids: &[EdgeId],
        predicates: &[PredicateDef],
    ) -> GCardResult<PathQuery> {
        // 把抽象边重新展开成“路径查询”对象。
        // catalog / 采样模块更容易消费这种“顶点-边-顶点-边...”的线性表示。
        let mut path_elements = Vec::new();
        let mut vertex_predicates: HashMap<usize, Vec<PredicateDef>> = HashMap::new();
        let mut edge_predicates: HashMap<usize, Vec<PredicateDef>> = HashMap::new();

        for (idx, &vertex_id) in path_vertices.iter().enumerate() {
            let vertex = self.inner.vertices.get(&vertex_id).ok_or_else(|| {
                GCardError::VertexNotFound(format!("Vertex {} not found", vertex_id))
            })?;
            path_elements.push(PathElement::Vertex {
                label: vertex.label.clone(),
                position: idx * 2,
            });
            let mut v_preds = Vec::new();
            for pred in predicates {
                if pred.target == "vertex" && pred.id == vertex_id as u32 {
                    v_preds.push(pred.clone());
                }
            }
            if !v_preds.is_empty() {
                vertex_predicates.insert(idx * 2, v_preds);
            }

            if idx < original_edge_ids.len() {
                let edge_id = original_edge_ids[idx];
                let edge = self.inner.edges.get(&edge_id).ok_or_else(|| {
                    GCardError::EdgeNotFound(format!("Edge {} not found", edge_id))
                })?;
                let direction = if edge.src_vertex_id == vertex_id {
                    EdgeDirection::Outgoing
                } else if edge.dst_vertex_id == vertex_id {
                    EdgeDirection::Incoming
                } else {
                    return Err(GCardError::InvalidState(format!(
                        "edge {} is not incident to vertex {} in abstract path",
                        edge_id, vertex_id
                    )));
                };

                path_elements.push(PathElement::Edge {
                    label: edge.label.clone(),
                    position: idx * 2 + 1,
                    direction,
                });

                let mut e_preds = Vec::new();
                for pred in predicates {
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

    fn build_unit_path_queries(&self, abstract_edge: &AbstractEdge) -> GCardResult<Vec<PathQuery>> {
        if abstract_edge.path_vertices.len() != abstract_edge.original_edge_ids.len() + 1 {
            return Err(GCardError::InvalidData(format!(
                "abstract edge path is inconsistent: vertices={}, edges={}",
                abstract_edge.path_vertices.len(),
                abstract_edge.original_edge_ids.len()
            )));
        }

        let last_hop_idx = abstract_edge.original_edge_ids.len().saturating_sub(1);
        let mut queries = Vec::with_capacity(abstract_edge.original_edge_ids.len());

        for hop_idx in 0..abstract_edge.original_edge_ids.len() {
            let src_vertex_id = abstract_edge.path_vertices[hop_idx];
            let dst_vertex_id = abstract_edge.path_vertices[hop_idx + 1];
            let edge_id = abstract_edge.original_edge_ids[hop_idx];

            // Simple baseline: each predicate is attached to exactly one length-1 hop.
            // Internal vertex predicates are assigned to the outgoing hop; the sink
            // vertex predicate is assigned to the final hop.
            let hop_predicates = abstract_edge
                .predicates
                .iter()
                .filter(|pred| match pred.target.as_str() {
                    "edge" => pred.id == edge_id as u32,
                    "vertex" => {
                        pred.id == src_vertex_id as u32
                            || (hop_idx == last_hop_idx && pred.id == dst_vertex_id as u32)
                    }
                    _ => false,
                })
                .cloned()
                .collect::<Vec<_>>();

            queries.push(self.build_path_query_from_parts(
                &[src_vertex_id, dst_vertex_id],
                &[edge_id],
                &hop_predicates,
            )?);
        }

        Ok(queries)
    }

    fn flush_local_cache(local: &HashMap<(u32, u64), bool>, shared: &DashMap<(u32, u64), bool>) {
        // 线程本地缓存先积累，再批量合并进共享缓存，减少 DashMap 热点竞争。
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
        // 统一封装谓词比较逻辑，避免顶点谓词/边谓词各写一遍分支。
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
                functional: FunctionalDirection::None,
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
                functional: FunctionalDirection::None,
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

    fn build_abstract_edge_candidates_for_path(
        &self,
        path: &Path,
        k: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> GCardResult<Vec<Vec<AbstractEdge>>> {
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();

        let mut add_ranges = |ranges: Vec<(usize, usize)>,
                              candidates: &mut Vec<Vec<AbstractEdge>>|
         -> GCardResult<()> {
            if seen.insert(ranges.clone()) {
                candidates.push(self.build_abstract_edges_from_ranges(path, &ranges)?);
            }
            Ok(())
        };

        let mut functional_ranges = self.abstract_edge_ranges_for_path(path, k);
        let mut i = 0;
        while i + 1 < functional_ranges.len() {
            let cut_edge_idx = functional_ranges[i].1;
            let merged = (functional_ranges[i].0, functional_ranges[i + 1].1);

            if self.can_contract_path_cut(path, cut_edge_idx, degree_seq_graph)
                && self.path_slice_has_catalog(path, merged.0, merged.1, degree_seq_graph)
            {
                functional_ranges[i] = merged;
                functional_ranges.remove(i + 1);
                i = i.saturating_sub(1);
            } else {
                i += 1;
            }
        }
        add_ranges(functional_ranges, &mut candidates)?;

        for ranges in self.abstract_edge_range_variants_for_path(path, k) {
            add_ranges(ranges, &mut candidates)?;
        }

        Ok(candidates)
    }

    fn abstract_edge_ranges_for_path(&self, path: &Path, k: usize) -> Vec<(usize, usize)> {
        let l = path.edges.len();
        if l == 0 {
            return Vec::new();
        }

        let num_abstract_edges = (l as f64 / k as f64).ceil() as usize;

        if l % k == 0 {
            return self.abstract_edge_ranges_even(path, k);
        }

        let mut solution = Vec::new();
        let short_size = l.saturating_sub((num_abstract_edges - 1) * k);
        let mut min_predicate_count = usize::MAX;

        for short_pos in 0..num_abstract_edges {
            let mut candidate = vec![k; num_abstract_edges];
            candidate[short_pos] = short_size;
            let short_start = short_pos * k;
            let short_end = short_start + short_size;
            let short_predicate_count =
                self.count_predicates_in_range(path, short_start, short_end);

            if short_predicate_count < min_predicate_count {
                min_predicate_count = short_predicate_count;
                solution = candidate;
            }
        }

        if solution.is_empty() {
            solution = vec![k; num_abstract_edges - 1];
            solution.push(l - (num_abstract_edges - 1) * k);
        }

        let mut ranges = Vec::with_capacity(solution.len());
        let mut edge_idx = 0;
        for edge_count in solution {
            let end_idx = (edge_idx + edge_count).min(path.edges.len());
            ranges.push((edge_idx, end_idx));
            edge_idx += edge_count;
        }
        ranges
    }

    fn abstract_edge_range_variants_for_path(
        &self,
        path: &Path,
        k: usize,
    ) -> Vec<Vec<(usize, usize)>> {
        let l = path.edges.len();
        if l == 0 {
            return vec![Vec::new()];
        }
        if l % k == 0 {
            return vec![self.abstract_edge_ranges_even(path, k)];
        }

        let num_abstract_edges = (l as f64 / k as f64).ceil() as usize;
        let short_size = l.saturating_sub((num_abstract_edges - 1) * k);
        let mut variants = Vec::with_capacity(num_abstract_edges);

        for short_pos in 0..num_abstract_edges {
            let mut solution = vec![k; num_abstract_edges];
            solution[short_pos] = short_size;

            let mut ranges = Vec::with_capacity(solution.len());
            let mut edge_idx = 0;
            for edge_count in solution {
                let end_idx = (edge_idx + edge_count).min(path.edges.len());
                ranges.push((edge_idx, end_idx));
                edge_idx += edge_count;
            }
            variants.push(ranges);
        }

        variants
    }

    fn abstract_edge_ranges_even(&self, path: &Path, k: usize) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        let mut edge_idx = 0;

        while edge_idx < path.edges.len() {
            let end_idx = (edge_idx + k).min(path.edges.len());
            ranges.push((edge_idx, end_idx));
            edge_idx += k;
        }

        ranges
    }

    fn build_abstract_edges_from_ranges(
        &self,
        path: &Path,
        ranges: &[(usize, usize)],
    ) -> GCardResult<Vec<AbstractEdge>> {
        ranges
            .iter()
            .map(|&(start_edge_idx, end_edge_idx)| {
                self.build_abstract_edge_from_range(path, start_edge_idx, end_edge_idx)
            })
            .collect()
    }

    fn build_abstract_edge_from_range(
        &self,
        path: &Path,
        start_edge_idx: usize,
        end_edge_idx: usize,
    ) -> GCardResult<AbstractEdge> {
        let abstract_edge_edges = &path.edges[start_edge_idx..end_edge_idx];

        let mut predicates = Vec::new();
        for &edge_id in abstract_edge_edges {
            if let Some(edge) = self.inner.edges.get(&edge_id) {
                predicates.extend(edge.predicates.clone());
            }
        }

        let src_vertex_idx = start_edge_idx;
        let dst_vertex_idx = if end_edge_idx < path.vertices.len() {
            end_edge_idx
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

        Ok(AbstractEdge {
            src,
            dst,
            src_pcf: Arc::new(Pcf::empty()),
            dst_pcf: Arc::new(Pcf::empty()),
            functional: FunctionalDirection::None,
            predicates,
            original_edge_ids: abstract_edge_edges.to_vec(),
            path_vertices,
            selectivity: 1.0,
            path_str: String::new(),
        })
    }

    fn can_contract_path_cut(
        &self,
        path: &Path,
        cut_edge_idx: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> bool {
        if !functional_refine_enabled() {
            return false;
        }
        if cut_edge_idx == 0 || cut_edge_idx >= path.edges.len() {
            return false;
        }

        let shared_vertex_id = path.vertices[cut_edge_idx];
        if self
            .inner
            .vertices
            .get(&shared_vertex_id)
            .map(|v| !v.predicates.is_empty())
            .unwrap_or(false)
        {
            return false;
        }

        self.edge_is_functional(path.edges[cut_edge_idx - 1], degree_seq_graph)
            || self.edge_is_functional(path.edges[cut_edge_idx], degree_seq_graph)
    }

    fn edge_is_functional(
        &self,
        edge_id: EdgeId,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> bool {
        let Some(edge) = self.inner.edges.get(&edge_id) else {
            return false;
        };

        let cardinality = degree_seq_graph
            .edge_cardinality(&edge.label)
            .unwrap_or_else(|| manual_edge_cardinality(&edge.label));
        matches!(
            cardinality,
            EdgeCardinality::ManyToOne | EdgeCardinality::OneToMany
        )
    }

    fn path_slice_has_catalog(
        &self,
        path: &Path,
        start_edge_idx: usize,
        end_edge_idx: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> bool {
        if start_edge_idx >= end_edge_idx || end_edge_idx > path.edges.len() {
            return false;
        }
        let node_seq = path.vertices[start_edge_idx..=end_edge_idx]
            .iter()
            .filter_map(|id| self.inner.vertices.get(id).map(|v| v.label.clone()))
            .collect::<Vec<_>>();
        let edge_seq = path.edges[start_edge_idx..end_edge_idx]
            .iter()
            .filter_map(|id| self.inner.edges.get(id).map(|e| e.label.clone()))
            .collect::<Vec<_>>();
        if node_seq.len() != edge_seq.len() + 1 {
            return false;
        }
        let key = make_alt_key(&node_seq, &edge_seq);
        degree_seq_graph.path_has_endpoint_pair(
            &key,
            node_seq.first().map(String::as_str).unwrap_or_default(),
            node_seq.last().map(String::as_str).unwrap_or_default(),
        )
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
        unit_selectivity_walks: bool,
    ) -> GCardResult<Vec<(AbstractGraph, u64)>> {
        let selectivity_cache: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        // String-keyed vertex sample cache (label → sampled vertex IDs).
        let flat_vertex_cache: Arc<DashMap<String, Vec<VertexId>>> = Arc::new(DashMap::new());
        let pred_cache: Arc<DashMap<(u32, u64), bool>> = Arc::new(DashMap::new());

        let t_cycle = std::time::Instant::now();
        let has_cycle = self.has_cycle();
        BUILD_CYCLE_CHECK_NANOS.fetch_add(t_cycle.elapsed().as_nanos() as u64, Ordering::Relaxed);

        if !has_cycle {
            let abstract_graphs = self.build_abstract_graph_candidates_from_query_graph_flat(
                self,
                k,
                degree_seq_graph,
                flat_graph,
                sample_size,
                predicate_apply_type,
                unit_selectivity_walks,
                &selectivity_cache,
                &flat_vertex_cache,
                &pred_cache,
            )?;
            return Ok(abstract_graphs
                .into_iter()
                .map(|abstract_graph| (abstract_graph, 0))
                .collect());
        }

        // ── Cycle case: enumerate spanning trees ──────────────────────────────
        let t_tree = std::time::Instant::now();
        let cardinalities = self.estimate_edge_cardinalities(flat_graph);
        let trees_with_scores = self.build_k_best_trees(&cardinalities, tree_num.max(1));
        BUILD_SCORE_TREE_NANOS.fetch_add(t_tree.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let results: Vec<GCardResult<Vec<(AbstractGraph, u64)>>> = trees_with_scores
            .par_iter()
            .map(|(tree, tree_score)| {
                self.build_abstract_graph_candidates_from_query_graph_flat(
                    tree,
                    k,
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    unit_selectivity_walks,
                    &selectivity_cache,
                    &flat_vertex_cache,
                    &pred_cache,
                )
                .map(|graphs| {
                    graphs
                        .into_iter()
                        .map(|graph| (graph, *tree_score))
                        .collect()
                })
            })
            .collect();

        let mut abstract_graphs = Vec::new();
        for r in results {
            abstract_graphs.extend(r?);
        }
        Ok(abstract_graphs)
    }

    /// 从用户指定的分解方案构建抽象图，跳过自动 pivot/path 分解。
    ///
    /// JSON 格式：
    /// ```json
    /// {
    ///   "abstract_edges": [
    ///     { "path_vertices": [1, 2, 3], "original_edge_ids": [1, 2] },
    ///     { "path_vertices": [3, 4], "original_edge_ids": [3] }
    ///   ]
    /// }
    /// ```
    pub fn build_abstract_graph_flat_from_decomposition(
        &self,
        decomposition: &DecompositionDef,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        unit_selectivity_walks: bool,
    ) -> GCardResult<Vec<(AbstractGraph, u64)>> {
        let selectivity_cache: Arc<DashMap<String, f64>> = Arc::new(DashMap::new());
        let flat_vertex_cache: Arc<DashMap<String, Vec<VertexId>>> = Arc::new(DashMap::new());
        let pred_cache: Arc<DashMap<(u32, u64), bool>> = Arc::new(DashMap::new());

        let mut all_abstract_edges: Vec<AbstractEdge> = Vec::new();
        for ae_def in &decomposition.abstract_edges {
            let abstract_edge = self.build_abstract_edge_from_def(ae_def)?;
            all_abstract_edges.push(abstract_edge);
        }

        let results: Vec<GCardResult<AbstractEdge>> = all_abstract_edges
            .into_par_iter()
            .map(|mut abstract_edge| {
                self.fill_pcf_for_abstract_edge_flat(
                    &mut abstract_edge,
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    unit_selectivity_walks,
                    &selectivity_cache,
                    &flat_vertex_cache,
                    &pred_cache,
                )?;
                Ok(abstract_edge)
            })
            .collect();

        let mut abstract_graph = AbstractGraph::new();
        let mut next_edge_id: EdgeId = 1;
        for result in results {
            let abstract_edge = result?;
            let src_vertex = self.get_vertex(abstract_edge.src).ok_or_else(|| {
                GCardError::VertexNotFound(format!("Vertex {} not found", abstract_edge.src))
            })?;
            let dst_vertex = self.get_vertex(abstract_edge.dst).ok_or_else(|| {
                GCardError::VertexNotFound(format!("Vertex {} not found", abstract_edge.dst))
            })?;
            if abstract_graph.get_vertex(abstract_edge.src).is_none() {
                abstract_graph.add_vertex(src_vertex.clone());
            }
            if abstract_graph.get_vertex(abstract_edge.dst).is_none() {
                abstract_graph.add_vertex(dst_vertex.clone());
            }
            abstract_graph.add_edge(next_edge_id, abstract_edge);
            next_edge_id += 1;
        }

        Ok(vec![(abstract_graph, 0)])
    }

    /// 从单个 `AbstractEdgeDef` 构造 `AbstractEdge`，
    /// 自动收集路径上顶点和边的谓词。
    fn build_abstract_edge_from_def(&self, def: &AbstractEdgeDef) -> GCardResult<AbstractEdge> {
        if def.path_vertices.len() < 2 {
            return Err(GCardError::InvalidData(
                "abstract edge must have at least 2 path_vertices".to_string(),
            ));
        }
        if def.original_edge_ids.len() != def.path_vertices.len() - 1 {
            return Err(GCardError::InvalidData(format!(
                "original_edge_ids length ({}) must be path_vertices length ({}) - 1",
                def.original_edge_ids.len(),
                def.path_vertices.len(),
            )));
        }

        let src = def.path_vertices[0];
        let dst = *def.path_vertices.last().unwrap();

        let mut predicates = Vec::new();
        for &vertex_id in &def.path_vertices {
            if let Some(vertex) = self.inner.vertices.get(&vertex_id) {
                predicates.extend(vertex.predicates.clone());
            } else {
                return Err(GCardError::VertexNotFound(format!(
                    "Vertex {} not found in query graph",
                    vertex_id
                )));
            }
        }
        for &edge_id in &def.original_edge_ids {
            if let Some(edge) = self.inner.edges.get(&edge_id) {
                predicates.extend(edge.predicates.clone());
            } else {
                return Err(GCardError::EdgeNotFound(format!(
                    "Edge {} not found in query graph",
                    edge_id
                )));
            }
        }

        Ok(AbstractEdge {
            src,
            dst,
            src_pcf: Arc::new(Pcf::empty()),
            dst_pcf: Arc::new(Pcf::empty()),
            functional: FunctionalDirection::None,
            predicates,
            original_edge_ids: def.original_edge_ids.clone(),
            path_vertices: def.path_vertices.clone(),
            selectivity: 1.0,
            path_str: String::new(),
        })
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn functional_direction_for_abstract_edge(
        &self,
        abstract_edge: &AbstractEdge,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> FunctionalDirection {
        if !functional_refine_enabled() {
            return FunctionalDirection::None;
        }
        if abstract_edge.original_edge_ids.is_empty()
            || abstract_edge.path_vertices.len() != abstract_edge.original_edge_ids.len() + 1
        {
            return FunctionalDirection::None;
        }

        let mut src_to_dst = true;
        let mut dst_to_src = true;

        for (vertices, edge_id) in abstract_edge
            .path_vertices
            .windows(2)
            .zip(&abstract_edge.original_edge_ids)
        {
            let current = vertices[0];
            let next = vertices[1];
            let Some(edge) = self.inner.edges.get(edge_id) else {
                return FunctionalDirection::None;
            };
            let cardinality = degree_seq_graph
                .edge_cardinality(&edge.label)
                .unwrap_or_else(|| manual_edge_cardinality(&edge.label));

            let follows_edge_direction =
                edge.src_vertex_id == current && edge.dst_vertex_id == next;
            let follows_reverse_direction =
                edge.dst_vertex_id == current && edge.src_vertex_id == next;
            if !follows_edge_direction && !follows_reverse_direction {
                return FunctionalDirection::None;
            }

            let step_src_to_dst = if follows_edge_direction {
                matches!(cardinality, EdgeCardinality::ManyToOne)
            } else {
                matches!(cardinality, EdgeCardinality::OneToMany)
            };
            let step_dst_to_src = if follows_edge_direction {
                matches!(cardinality, EdgeCardinality::OneToMany)
            } else {
                matches!(cardinality, EdgeCardinality::ManyToOne)
            };

            src_to_dst &= step_src_to_dst;
            dst_to_src &= step_dst_to_src;
            if !src_to_dst && !dst_to_src {
                return FunctionalDirection::None;
            }
        }

        FunctionalDirection::from_flags(src_to_dst, dst_to_src)
    }

    fn build_abstract_graph_from_query_graph_flat(
        &self,
        query_graph: &QueryGraph,
        k: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        unit_selectivity_walks: bool,
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
                    unit_selectivity_walks,
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                )?;
                Ok(abstract_edge)
            })
            .collect();

        let mut abstract_graph = AbstractGraph::new();
        let mut next_edge_id: EdgeId = 1;
        let (results, local_pcfs) = self.extract_star_local_pcfs(results, degree_seq_graph);
        for (&center, pcfs) in &local_pcfs {
            if abstract_graph.get_vertex(center).is_none() {
                let vertex = self.get_vertex(center).ok_or_else(|| {
                    GCardError::VertexNotFound(format!("Vertex {} not found", center))
                })?;
                abstract_graph.add_vertex(vertex.clone());
            }
            for pcf in pcfs {
                abstract_graph.add_local_pcf(center, pcf.clone());
            }
        }
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

    fn build_abstract_graph_candidates_from_query_graph_flat(
        &self,
        query_graph: &QueryGraph,
        k: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        unit_selectivity_walks: bool,
        selectivity_cache: &Arc<DashMap<String, f64>>,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<Vec<AbstractGraph>> {
        let t_pivot = std::time::Instant::now();
        let pivot_nodes = query_graph.find_pivot_nodes();
        let paths = query_graph.find_paths_from_pivots(&pivot_nodes);
        BUILD_PIVOT_PATH_NANOS.fetch_add(t_pivot.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let t_ae = std::time::Instant::now();
        let limit = Self::split_candidate_limit();
        let mut edge_sets: Vec<Vec<AbstractEdge>> = vec![Vec::new()];
        for path in paths {
            let path_candidates =
                query_graph.build_abstract_edge_candidates_for_path(&path, k, degree_seq_graph)?;
            let mut next = Vec::new();
            for existing in &edge_sets {
                for candidate in &path_candidates {
                    let mut combined = existing.clone();
                    combined.extend(candidate.clone());
                    next.push(combined);
                    if next.len() >= limit {
                        break;
                    }
                }
                if next.len() >= limit {
                    break;
                }
            }
            edge_sets = next;
        }
        BUILD_ABSTRACT_EDGE_NANOS.fetch_add(t_ae.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let mut graphs = edge_sets
            .into_iter()
            .map(|edges| {
                self.build_abstract_graph_from_edges_flat(
                    edges,
                    HashMap::new(),
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    unit_selectivity_walks,
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                )
            })
            .collect::<GCardResult<Vec<_>>>()?;

        if let Some((local_pcfs, consumed_edges)) =
            query_graph.extract_raw_star_local_pcfs(degree_seq_graph)
        {
            let residual = query_graph.subgraph_without_edges(&consumed_edges);
            let pivot_nodes = residual.find_pivot_nodes();
            let residual_paths = residual.find_paths_from_pivots(&pivot_nodes);
            let mut residual_edge_sets: Vec<Vec<AbstractEdge>> = vec![Vec::new()];
            for path in residual_paths {
                let path_candidates =
                    residual.build_abstract_edge_candidates_for_path(&path, k, degree_seq_graph)?;
                let mut next = Vec::new();
                for existing in &residual_edge_sets {
                    for candidate in &path_candidates {
                        let mut combined = existing.clone();
                        combined.extend(candidate.clone());
                        next.push(combined);
                        if next.len() >= limit {
                            break;
                        }
                    }
                    if next.len() >= limit {
                        break;
                    }
                }
                residual_edge_sets = next;
            }

            for edges in residual_edge_sets {
                graphs.push(self.build_abstract_graph_from_edges_flat(
                    edges,
                    local_pcfs.clone(),
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    unit_selectivity_walks,
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                )?);
                if graphs.len() >= limit {
                    break;
                }
            }
        }

        Ok(graphs)
    }

    fn split_candidate_limit() -> usize {
        std::env::var("GCARD_SPLIT_CANDIDATE_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(16)
    }

    fn build_abstract_graph_from_edges_flat(
        &self,
        all_abstract_edges: Vec<AbstractEdge>,
        initial_local_pcfs: HashMap<VertexId, Vec<Pcf>>,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        unit_selectivity_walks: bool,
        selectivity_cache: &Arc<DashMap<String, f64>>,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<AbstractGraph> {
        let results: Vec<GCardResult<AbstractEdge>> = all_abstract_edges
            .into_par_iter()
            .map(|mut abstract_edge| {
                self.fill_pcf_for_abstract_edge_flat(
                    &mut abstract_edge,
                    degree_seq_graph,
                    flat_graph,
                    sample_size,
                    predicate_apply_type,
                    unit_selectivity_walks,
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                )?;
                Ok(abstract_edge)
            })
            .collect();

        let mut abstract_graph = AbstractGraph::new();
        let mut next_edge_id: EdgeId = 1;
        let (results, local_pcfs) = self.extract_star_local_pcfs(results, degree_seq_graph);
        for (&center, pcfs) in &initial_local_pcfs {
            if abstract_graph.get_vertex(center).is_none() {
                let vertex = self.get_vertex(center).ok_or_else(|| {
                    GCardError::VertexNotFound(format!("Vertex {} not found", center))
                })?;
                abstract_graph.add_vertex(vertex.clone());
            }
            for pcf in pcfs {
                abstract_graph.add_local_pcf(center, pcf.clone());
            }
        }
        for (&center, pcfs) in &local_pcfs {
            if abstract_graph.get_vertex(center).is_none() {
                let vertex = self.get_vertex(center).ok_or_else(|| {
                    GCardError::VertexNotFound(format!("Vertex {} not found", center))
                })?;
                abstract_graph.add_vertex(vertex.clone());
            }
            for pcf in pcfs {
                abstract_graph.add_local_pcf(center, pcf.clone());
            }
        }
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

    fn subgraph_without_edges(&self, consumed_edges: &HashSet<EdgeId>) -> QueryGraph {
        let mut graph = QueryGraph::new();
        graph.inner.vertices = self.inner.vertices.clone();
        graph.predicate_index = self.predicate_index.clone();

        for (&edge_id, edge) in &self.inner.edges {
            if consumed_edges.contains(&edge_id) {
                continue;
            }
            graph
                .inner
                .outgoing_edges
                .entry(edge.src_vertex_id)
                .or_default()
                .push(edge_id);
            graph
                .inner
                .incoming_edges
                .entry(edge.dst_vertex_id)
                .or_default()
                .push(edge_id);
            graph.inner.edges.insert(edge_id, edge.clone());
        }
        graph
    }

    fn extract_raw_star_local_pcfs(
        &self,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> Option<(HashMap<VertexId, Vec<Pcf>>, HashSet<EdgeId>)> {
        let catalog_max_star_degree = degree_seq_graph
            .star_stats
            .keys()
            .map(|key| key.degree)
            .max()
            .unwrap_or(0);
        let star_degree_override = GCARD_MAX_STAR_DEGREE_OVERRIDE.load(Ordering::Relaxed);
        let max_star_degree = if star_degree_override != GCARD_STAR_CONFIG_UNSET {
            star_degree_override
        } else {
            std::env::var("GCARD_MAX_STAR_DEGREE")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(catalog_max_star_degree)
        };
        if max_star_degree < 2 {
            return None;
        }

        let mut candidates_by_center: HashMap<VertexId, Vec<(EdgeId, PathPattern)>> =
            HashMap::new();
        for (&edge_id, edge) in &self.inner.edges {
            if !edge.predicates.is_empty() {
                continue;
            }
            let Some(src_vertex) = self.inner.vertices.get(&edge.src_vertex_id) else {
                continue;
            };
            let Some(dst_vertex) = self.inner.vertices.get(&edge.dst_vertex_id) else {
                continue;
            };
            if src_vertex.predicates.is_empty() && dst_vertex.predicates.is_empty() {
                if self.get_degree(edge.dst_vertex_id) == 1 {
                    candidates_by_center
                        .entry(edge.src_vertex_id)
                        .or_default()
                        .push((
                            edge_id,
                            PathPattern::new_without_reverse(
                                vec![src_vertex.label.clone(), dst_vertex.label.clone()],
                                vec![edge.label.clone()],
                            ),
                        ));
                }
                if self.get_degree(edge.src_vertex_id) == 1 {
                    candidates_by_center
                        .entry(edge.dst_vertex_id)
                        .or_default()
                        .push((
                            edge_id,
                            PathPattern::new_without_reverse(
                                vec![dst_vertex.label.clone(), src_vertex.label.clone()],
                                vec![edge.label.clone()],
                            ),
                        ));
                }
            }
        }

        let mut consumed_edges = HashSet::new();
        let mut local_pcfs: HashMap<VertexId, Vec<Pcf>> = HashMap::new();
        let mut centers: Vec<_> = candidates_by_center.keys().copied().collect();
        centers.sort_unstable();

        for center in centers {
            let Some(center_vertex) = self.inner.vertices.get(&center) else {
                continue;
            };
            let mut mergeable = candidates_by_center
                .remove(&center)
                .unwrap_or_default()
                .into_iter()
                .filter(|(edge_id, _)| !consumed_edges.contains(edge_id))
                .collect::<Vec<_>>();
            mergeable.sort_by(|a, b| a.1.vs.cmp(&b.1.vs).then(a.1.es.cmp(&b.1.es)));

            while !mergeable.is_empty() {
                let unique_arms = mergeable
                    .iter()
                    .enumerate()
                    .map(|(idx, (_, arm))| (idx, arm.clone()))
                    .collect::<Vec<_>>();
                let Some((selected_indices, pcf)) = Self::find_matching_star_pcf(
                    &center_vertex.label,
                    &unique_arms,
                    max_star_degree,
                    degree_seq_graph,
                ) else {
                    break;
                };

                for &idx in &selected_indices {
                    consumed_edges.insert(mergeable[idx].0);
                }
                local_pcfs.entry(center).or_default().push(pcf);
                for idx in selected_indices.into_iter().rev() {
                    mergeable.remove(idx);
                }
            }
        }

        (!consumed_edges.is_empty()).then_some((local_pcfs, consumed_edges))
    }

    fn extract_star_local_pcfs(
        &self,
        results: Vec<GCardResult<AbstractEdge>>,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> (Vec<GCardResult<AbstractEdge>>, HashMap<VertexId, Vec<Pcf>>) {
        let mut edges = Vec::new();
        let mut passthrough_errors = Vec::new();
        for result in results {
            match result {
                Ok(edge) => edges.push(edge),
                Err(err) => passthrough_errors.push(Err(err)),
            }
        }

        let mut candidates_by_center: HashMap<VertexId, Vec<(usize, PathPattern)>> = HashMap::new();
        for (idx, edge) in edges.iter().enumerate() {
            if !edge.predicates.is_empty() {
                continue;
            }
            for center in [edge.src, edge.dst] {
                let other = if center == edge.src {
                    edge.dst
                } else {
                    edge.src
                };
                if self.get_degree(other) != 1 {
                    continue;
                }
                if let Some(arm) = self.star_arm_for_edge(edge, center) {
                    candidates_by_center
                        .entry(center)
                        .or_default()
                        .push((idx, arm));
                }
            }
        }

        let catalog_max_star_degree = degree_seq_graph
            .star_stats
            .keys()
            .map(|key| key.degree)
            .max()
            .unwrap_or(0);
        let catalog_max_star_length = degree_seq_graph
            .star_stats
            .keys()
            .map(|key| key.max_arm_len)
            .max()
            .unwrap_or(0);
        let star_degree_override = GCARD_MAX_STAR_DEGREE_OVERRIDE.load(Ordering::Relaxed);
        let max_star_degree = if star_degree_override != GCARD_STAR_CONFIG_UNSET {
            star_degree_override
        } else {
            std::env::var("GCARD_MAX_STAR_DEGREE")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(catalog_max_star_degree)
        };
        let star_length_override = GCARD_MAX_STAR_LENGTH_OVERRIDE.load(Ordering::Relaxed);
        let max_star_length = if star_length_override != GCARD_STAR_CONFIG_UNSET {
            star_length_override
        } else {
            std::env::var("GCARD_MAX_STAR_LENGTH")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(catalog_max_star_length)
        };

        let mut consumed = HashSet::new();
        let mut local_pcfs: HashMap<VertexId, Vec<Pcf>> = HashMap::new();
        let mut centers: Vec<_> = candidates_by_center.keys().copied().collect();
        centers.sort_unstable();
        for center in centers {
            if max_star_degree == 0 {
                continue;
            }
            let center_label = match self.get_vertex(center) {
                Some(v) => v.label.clone(),
                None => continue,
            };
            let mut mergeable = candidates_by_center
                .remove(&center)
                .unwrap_or_default()
                .into_iter()
                .filter(|(idx, _)| !consumed.contains(idx))
                .filter(|(_, arm)| arm.es.len() <= max_star_length)
                .collect::<Vec<_>>();

            mergeable.sort_by(|a, b| a.1.vs.cmp(&b.1.vs).then(a.1.es.cmp(&b.1.es)));

            while !mergeable.is_empty() {
                let mut unique_arms = Vec::new();
                let mut last_arm: Option<&PathPattern> = None;
                for (i, (_, arm)) in mergeable.iter().enumerate() {
                    if last_arm.is_some_and(|last| last == arm) {
                        continue;
                    }
                    unique_arms.push((i, arm.clone()));
                    last_arm = Some(arm);
                }

                let Some((selected_indices, pcf)) = Self::find_matching_star_pcf(
                    &center_label,
                    &unique_arms,
                    max_star_degree,
                    degree_seq_graph,
                ) else {
                    break;
                };

                for &i in &selected_indices {
                    consumed.insert(mergeable[i].0);
                }
                local_pcfs.entry(center).or_default().push(pcf);
                for i in selected_indices.into_iter().rev() {
                    mergeable.remove(i);
                }
            }
        }

        let mut out = passthrough_errors;
        out.extend(
            edges
                .into_iter()
                .enumerate()
                .filter_map(|(idx, edge)| (!consumed.contains(&idx)).then_some(Ok(edge))),
        );
        (out, local_pcfs)
    }

    fn find_matching_star_pcf(
        center_label: &str,
        unique_arms: &[(usize, PathPattern)],
        max_star_degree: usize,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> Option<(Vec<usize>, Pcf)> {
        let max_degree = max_star_degree.min(unique_arms.len());
        if max_degree < 2 {
            return None;
        }
        for degree in (2..=max_degree).rev() {
            let mut selected = Vec::with_capacity(degree);
            if let Some(matched) = Self::find_matching_star_pcf_at_degree(
                center_label,
                unique_arms,
                degree,
                0,
                &mut selected,
                degree_seq_graph,
            ) {
                return Some(matched);
            }
        }
        None
    }

    fn find_matching_star_pcf_at_degree(
        center_label: &str,
        unique_arms: &[(usize, PathPattern)],
        target_degree: usize,
        start: usize,
        selected: &mut Vec<usize>,
        degree_seq_graph: &DegreeSeqGraphCompressed,
    ) -> Option<(Vec<usize>, Pcf)> {
        if selected.len() == target_degree {
            let arms = selected
                .iter()
                .map(|&idx| unique_arms[idx].1.clone())
                .collect::<Vec<_>>();
            let star_key = StarStatKey::new(center_label.to_string(), arms);
            return degree_seq_graph
                .star_stats
                .contains_key(&star_key)
                .then(|| {
                    let pcf = degree_seq_graph.get_piece_func_by_star(&star_key);
                    let mergeable_indices = selected
                        .iter()
                        .map(|&idx| unique_arms[idx].0)
                        .collect::<Vec<_>>();
                    (mergeable_indices, pcf)
                });
        }

        let remaining_needed = target_degree - selected.len();
        if unique_arms.len().saturating_sub(start) < remaining_needed {
            return None;
        }

        for idx in start..unique_arms.len() {
            if unique_arms.len() - idx < remaining_needed {
                break;
            }
            selected.push(idx);
            if let Some(matched) = Self::find_matching_star_pcf_at_degree(
                center_label,
                unique_arms,
                target_degree,
                idx + 1,
                selected,
                degree_seq_graph,
            ) {
                return Some(matched);
            }
            selected.pop();
        }
        None
    }

    fn star_arm_for_edge(&self, edge: &AbstractEdge, center: VertexId) -> Option<PathPattern> {
        let mut node_labels = edge
            .path_vertices
            .iter()
            .map(|vid| self.get_vertex(*vid).map(|v| v.label.clone()))
            .collect::<Option<Vec<_>>>()?;
        let mut edge_labels = edge
            .original_edge_ids
            .iter()
            .map(|eid| self.get_edge(*eid).map(|e| e.label.clone()))
            .collect::<Option<Vec<_>>>()?;

        if edge.dst == center {
            node_labels.reverse();
            edge_labels.reverse();
        } else if edge.src != center {
            return None;
        }

        Some(PathPattern::new_without_reverse(node_labels, edge_labels))
    }

    fn fill_pcf_for_abstract_edge_flat(
        &self,
        abstract_edge: &mut AbstractEdge,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
        sample_size: usize,
        predicate_apply_type: &PredicateApplyType,
        unit_selectivity_walks: bool,
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
                && matches!(
                    predicate_apply_type,
                    PredicateApplyType::INNER | PredicateApplyType::SCALE
                ) {
                let cache_key =
                    Self::build_selectivity_cache_key(&alt_key, &abstract_edge.predicates);
                if let Some(cached) = selectivity_cache.get(&cache_key) {
                    *cached
                } else {
                    crate::procedures::gcard_query::SAMPLING_CALLS
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    let sel = if unit_selectivity_walks {
                        self.compute_selectivity_flat_unit_paths(
                            fg,
                            abstract_edge,
                            sample_size,
                            flat_vertex_cache,
                            pred_cache,
                        )?
                    } else {
                        self.compute_selectivity_flat(
                            fg,
                            abstract_edge,
                            sample_size,
                            flat_vertex_cache,
                            pred_cache,
                        )?
                    };
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
            match predicate_apply_type {
                PredicateApplyType::INNER => {
                    src_pcf_func = src_pcf_func.truncate_by_ratio(selectivity);
                    dst_pcf_func = dst_pcf_func.truncate_by_ratio(selectivity);
                }
                PredicateApplyType::SCALE => {
                    src_pcf_func = src_pcf_func.scale_by_ratio(selectivity);
                    dst_pcf_func = dst_pcf_func.scale_by_ratio(selectivity);
                }
                PredicateApplyType::IGNORE => {}
            }
        }

        abstract_edge.src_pcf = Arc::new(src_pcf_func);
        abstract_edge.dst_pcf = Arc::new(dst_pcf_func);
        abstract_edge.functional =
            self.functional_direction_for_abstract_edge(abstract_edge, degree_seq_graph);

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
        self.compute_selectivity_flat_for_path_query(
            flat_graph,
            &path_query,
            &abstract_edge.path_str,
            sample_size,
            flat_vertex_cache,
            pred_cache,
        )
    }

    fn compute_selectivity_flat_unit_paths(
        &self,
        flat_graph: &FlatGraph,
        abstract_edge: &AbstractEdge,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<f64> {
        let unit_queries = self.build_unit_path_queries(abstract_edge)?;
        let mut selectivity = 1.0f64;

        for (hop_idx, path_query) in unit_queries.iter().enumerate() {
            let hop_desc = format!("{} [unit-hop={}]", abstract_edge.path_str, hop_idx);
            let hop_selectivity = self.compute_selectivity_flat_for_path_query(
                flat_graph,
                path_query,
                &hop_desc,
                sample_size,
                flat_vertex_cache,
                pred_cache,
            )?;
            selectivity *= hop_selectivity;
        }

        Ok(selectivity)
    }

    fn compute_selectivity_flat_for_path_query(
        &self,
        flat_graph: &FlatGraph,
        path_query: &PathQuery,
        path_desc: &str,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> GCardResult<f64> {
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
                    // Cache: (expected_label, vid, edge_label, outgoing) → neighbors with edge IDs.
                    let mut nbr_cache: HashMap<
                        (String, VertexId, String, bool),
                        Vec<(VertexId, EdgeId)>,
                    > = HashMap::new();
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

        eprintln!(
            "[selectivity-debug] path={}, src_label={}, samples={}, struct_success={}, sum_struct={:.4}, sum_pred={:.4}, sel={:.6}",
            path_desc,
            compiled.src_label,
            sampled_starts.len(),
            struct_success_sample_count,
            sum_struct_weight,
            sum_pred_weight,
            selectivity,
        );

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
        nbr_cache: &mut HashMap<(String, VertexId, String, bool), Vec<(VertexId, EdgeId)>>,
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<(f64, f64)> {
        if compiled.steps.is_empty() {
            return Ok((1.0, 1.0));
        }

        let mut current_vid = start_vertex;
        let mut current_label = compiled.src_label.as_str();
        let mut weight: f64 = 1.0;
        let mut pred_ok = true;

        for step in &compiled.steps {
            match step {
                FlatCompiledStep::Vertex { label, predicates } => {
                    current_label = label.as_str();
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
                                        Some(v) => {
                                            if crate::procedures::gcard_query::GCARD_VERBOSE
                                                .load(std::sync::atomic::Ordering::Relaxed)
                                                && weight == 1.0
                                            {
                                                eprintln!(
                                                    "[pred-debug] vid={}, prop_idx={}, stored={:?}, expected={:?}, op={:?}",
                                                    current_vid,
                                                    rp.prop_index,
                                                    v,
                                                    &rp.value,
                                                    &rp.op
                                                );
                                            }
                                            self.compare_values(v, &rp.op, &rp.value)?
                                        }
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
                    let cache_key = (
                        current_label.to_string(),
                        current_vid,
                        edge_label.clone(),
                        outgoing,
                    );

                    // Fetch and cache neighbors (with edge IDs).
                    if !nbr_cache.contains_key(&cache_key) {
                        let t0 = std::time::Instant::now();
                        let slice = flat_graph.neighbors_with_eid_for_label(
                            current_label,
                            current_vid,
                            edge_label,
                            outgoing,
                        );
                        prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                        nbr_cache.insert(cache_key.clone(), slice.to_vec());
                    }
                    let nbrs = &nbr_cache[&cache_key];

                    let degree = nbrs.len();
                    if degree == 0 {
                        if crate::procedures::gcard_query::GCARD_VERBOSE
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            let bucket_edges = flat_graph.hop_bucket_edge_count(
                                current_label,
                                edge_label,
                                outgoing,
                            );
                            eprintln!(
                                "[walk-deadend] vid={}, expected_label={}, edge_label={}, outgoing={}, bucket_edges={:?}",
                                current_vid, current_label, edge_label, outgoing, bucket_edges
                            );
                        }
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
