use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};

/// Global filter pool cache shared across all `gcard_query` invocations.
///
/// Key encodes the full predicate semantics, so reuse across queries is safe
/// even when per-query `predicate_id` numbering collides.  This turns the
/// expensive O(|label|) filter scan into an amortized one-time cost per
/// distinct `(label, predicate-set)`.
static GLOBAL_FILTERED_POOL_CACHE: LazyLock<
    Arc<DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>>,
> = LazyLock::new(|| Arc::new(DashMap::new()));

fn decomp_trace_enabled() -> bool {
    std::env::var_os("GCARD_DECOMP_TRACE_LOG").is_some()
}

fn decomp_trace_line(line: impl AsRef<str>) {
    let Some(path) = std::env::var_os("GCARD_DECOMP_TRACE_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{}", line.as_ref());
    }
}

fn decomp_trace_multiline(text: impl AsRef<str>) {
    let Some(path) = std::env::var_os("GCARD_DECOMP_TRACE_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = write!(file, "{}", text.as_ref());
        if !text.as_ref().ends_with('\n') {
            let _ = writeln!(file);
        }
    }
}

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
use crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid;
use crate::procedures::gcard_query::flat_graph::{FlatGraph, sample_without_replacement};
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
    use crate::procedures::gcard_query::flat_graph::FlatGraphBuilder;

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
    fn single_vertex_query_uses_label_cardinality() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "person")],
            edges: Vec::new(),
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();
        let mut builder = FlatGraphBuilder::new();
        builder.add_vertex(1, "person", vec![]);
        builder.add_vertex(2, "person", vec![]);
        builder.add_vertex(3, "person", vec![]);
        let flat_graph = builder.build();

        let mut candidates = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                100,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.get_es().unwrap(), 3.0);
    }

    #[test]
    fn single_vertex_query_applies_predicate_selectivity() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "person")],
            edges: Vec::new(),
            predicates: vec![crate::procedures::gcard_query::types::PredicateDef {
                predicate_id: None,
                target: "vertex".to_string(),
                id: 1,
                property: "age".to_string(),
                op: ComparisonOp::Eq,
                value: ScalarValue::Int64(Some(20)),
            }],
        };
        let query_graph = query.build_graph().unwrap();
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("person", vec!["age".to_string()]);
        builder.add_vertex(1, "person", vec![ScalarValue::Int64(Some(20))]);
        builder.add_vertex(2, "person", vec![ScalarValue::Int64(Some(20))]);
        builder.add_vertex(3, "person", vec![ScalarValue::Int64(Some(30))]);
        builder.add_vertex(4, "person", vec![ScalarValue::Int64(Some(30))]);
        let flat_graph = builder.build();

        let mut candidates = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                100,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.get_es().unwrap(), 2.0);
    }

    #[test]
    fn single_vertex_query_samples_inclusive_boundary_predicate() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "person")],
            edges: Vec::new(),
            predicates: vec![crate::procedures::gcard_query::types::PredicateDef {
                predicate_id: None,
                target: "vertex".to_string(),
                id: 1,
                property: "downvotes".to_string(),
                op: ComparisonOp::Le,
                value: ScalarValue::Int64(Some(0)),
            }],
        };
        let query_graph = query.build_graph().unwrap();
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("person", vec!["downvotes".to_string()]);
        for vid in 0..98 {
            builder.add_vertex(vid, "person", vec![ScalarValue::Int64(Some(0))]);
        }
        builder.add_vertex(98, "person", vec![ScalarValue::Int64(Some(1))]);
        builder.add_vertex(99, "person", vec![ScalarValue::Int64(Some(1920))]);
        let flat_graph = builder.build();

        let mut candidates = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                100,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();

        assert_eq!(candidates[0].0.get_es().unwrap(), 98.0);
    }

    #[test]
    fn single_vertex_query_samples_predicates_jointly() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "person")],
            edges: Vec::new(),
            predicates: vec![
                crate::procedures::gcard_query::types::PredicateDef {
                    predicate_id: None,
                    target: "vertex".to_string(),
                    id: 1,
                    property: "age".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int64(Some(20)),
                },
                crate::procedures::gcard_query::types::PredicateDef {
                    predicate_id: None,
                    target: "vertex".to_string(),
                    id: 1,
                    property: "downvotes".to_string(),
                    op: ComparisonOp::Eq,
                    value: ScalarValue::Int64(Some(0)),
                },
            ],
        };
        let query_graph = query.build_graph().unwrap();
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("person", vec!["age".to_string(), "downvotes".to_string()]);
        for vid in 0..9 {
            let group = vid / 3;
            builder.add_vertex(
                vid,
                "person",
                vec![
                    ScalarValue::Int64(Some(20 + group as i64 * 10)),
                    ScalarValue::Int64(Some(group as i64)),
                ],
            );
        }
        let flat_graph = builder.build();

        let mut candidates = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                9,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();

        assert_eq!(candidates[0].0.get_es().unwrap(), 3.0);
    }

    #[test]
    fn single_vertex_partial_sample_zero_hit_uses_upper_bound() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "person")],
            edges: Vec::new(),
            predicates: vec![crate::procedures::gcard_query::types::PredicateDef {
                predicate_id: None,
                target: "vertex".to_string(),
                id: 1,
                property: "age".to_string(),
                op: ComparisonOp::Eq,
                value: ScalarValue::Int64(Some(20)),
            }],
        };
        let query_graph = query.build_graph().unwrap();
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("person", vec!["age".to_string()]);
        for vid in 0..100 {
            builder.add_vertex(vid, "person", vec![ScalarValue::Int64(Some(30))]);
        }
        let flat_graph = builder.build();

        let mut partial = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                10,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();
        let mut exhaustive = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &DegreeSeqGraphCompressed::new(),
                Some(&flat_graph),
                100,
                &PredicateApplyType::INNER,
                false,
            )
            .unwrap();

        assert!(partial[0].0.get_es().unwrap() > 0.0);
        assert_eq!(exhaustive[0].0.get_es().unwrap(), 0.0);
    }

    #[test]
    fn single_edge_query_builds_and_reduces_directly() {
        let query = crate::procedures::gcard_query::types::Query {
            vertices: vec![vertex(1, "a"), vertex(2, "b")],
            edges: vec![edge(10, "e", 1, 2)],
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();
        let path_key = make_alt_key(&["a".to_string(), "b".to_string()], &["e".to_string()]);
        let mut endpoints = HashMap::new();
        endpoints.insert(
            "a".to_string(),
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction::from_degree_sequence(&[2, 1], 0.01, false)
                    .unwrap(),
            },
        );
        endpoints.insert(
            "b".to_string(),
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction::from_degree_sequence(&[1, 1, 1], 0.01, false)
                    .unwrap(),
            },
        );
        let mut catalog = DegreeSeqGraphCompressed::new();
        catalog.edge_set_to_endpoints.insert(path_key, endpoints);

        let mut candidates = query_graph
            .build_abstract_graph_flat(
                2,
                10,
                &catalog,
                None,
                100,
                &PredicateApplyType::IGNORE,
                false,
            )
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.get_es().unwrap(), 3.0);
    }

    #[test]
    fn compiled_walk_plan_resolves_forward_and_reverse_csr_handles() {
        let mut builder = FlatGraphBuilder::new();
        builder.add_vertex(1, "a", vec![]);
        builder.add_vertex(2, "b", vec![]);
        builder.add_edge(100, 1, "a", 2, "b", "e", vec![]);
        let flat_graph = builder.build();
        let path_query = PathQuery {
            path_elements: vec![
                PathElement::Vertex {
                    label: "a".to_string(),
                    position: 0,
                },
                PathElement::Edge {
                    label: "e".to_string(),
                    position: 0,
                    direction: EdgeDirection::Outgoing,
                },
                PathElement::Vertex {
                    label: "b".to_string(),
                    position: 1,
                },
            ],
            vertex_predicates: HashMap::new(),
            edge_predicates: HashMap::new(),
        };

        let plans = FlatCompiledPathQuery::compile_all(&path_query, &flat_graph).unwrap();

        let FlatCompiledStep::Edge { csr, .. } = &plans[0].right_segment[0] else {
            panic!("right segment must begin with an edge");
        };
        assert_eq!(csr.unwrap().neighbors_slice(1), &[(2, 100)]);

        let FlatCompiledStep::Edge { csr, .. } = &plans[1].left_segment[0] else {
            panic!("left segment must begin with an edge");
        };
        assert_eq!(csr.unwrap().neighbors_slice(2), &[(1, 100)]);
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

    fn trace_sorted_edge_ids(edge_ids: &HashSet<EdgeId>) -> Vec<EdgeId> {
        let mut ids: Vec<EdgeId> = edge_ids.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    fn trace_predicates(predicates: &[PredicateDef]) -> String {
        if predicates.is_empty() {
            return "[]".to_string();
        }
        predicates
            .iter()
            .map(|p| {
                format!(
                    "{}#{}.{:?}({}) {:?} {:?}",
                    p.target, p.id, p.predicate_id, p.property, p.op, p.value
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn trace_edge_brief(
        &self,
        edge_id: EdgeId,
        cardinalities: Option<&HashMap<EdgeId, u64>>,
    ) -> String {
        match self.inner.edges.get(&edge_id) {
            Some(edge) => {
                let src = self
                    .inner
                    .vertices
                    .get(&edge.src_vertex_id)
                    .map(|v| v.label.as_str())
                    .unwrap_or("?");
                let dst = self
                    .inner
                    .vertices
                    .get(&edge.dst_vertex_id)
                    .map(|v| v.label.as_str())
                    .unwrap_or("?");
                let card = cardinalities
                    .and_then(|c| c.get(&edge_id).copied())
                    .map(|c| format!(", card={}", c))
                    .unwrap_or_default();
                format!(
                    "e{}:{}({})->{}({}) label={}{} edge_preds={}",
                    edge_id,
                    edge.src_vertex_id,
                    src,
                    edge.dst_vertex_id,
                    dst,
                    edge.label,
                    card,
                    Self::trace_predicates(&edge.predicates)
                )
            }
            None => format!("e{}:<missing>", edge_id),
        }
    }

    fn trace_edge_set(
        &self,
        edge_ids: &HashSet<EdgeId>,
        cardinalities: Option<&HashMap<EdgeId, u64>>,
    ) -> String {
        Self::trace_sorted_edge_ids(edge_ids)
            .into_iter()
            .map(|eid| self.trace_edge_brief(eid, cardinalities))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    fn trace_path(&self, path: &Path) -> String {
        let mut chain = String::new();
        for (idx, vertex_id) in path.vertices.iter().enumerate() {
            if idx > 0 {
                let edge_id = path.edges.get(idx - 1).copied().unwrap_or_default();
                let edge_label = self
                    .inner
                    .edges
                    .get(&edge_id)
                    .map(|e| e.label.as_str())
                    .unwrap_or("?");
                chain.push_str(&format!(" -e{}:{}-> ", edge_id, edge_label));
            }
            let vertex = self.inner.vertices.get(vertex_id);
            let label = vertex.map(|v| v.label.as_str()).unwrap_or("?");
            let preds = vertex
                .map(|v| Self::trace_predicates(&v.predicates))
                .unwrap_or_else(|| "[]".to_string());
            chain.push_str(&format!("v{}:{} preds={}", vertex_id, label, preds));
        }
        format!(
            "start={} end={} vertices={:?} edges={:?} chain={}",
            path.start, path.end, path.vertices, path.edges, chain
        )
    }

    fn trace_ranges(&self, path: &Path, ranges: &[(usize, usize)]) -> String {
        ranges
            .iter()
            .map(|(start, end)| {
                let edge_ids = path.edges.get(*start..*end).unwrap_or(&[]).to_vec();
                let vertex_ids = if *start <= *end && *end < path.vertices.len() {
                    path.vertices[*start..=*end].to_vec()
                } else {
                    Vec::new()
                };
                format!(
                    "{}..{} vertices={:?} edges={:?}",
                    start, end, vertex_ids, edge_ids
                )
            })
            .collect::<Vec<_>>()
            .join(" || ")
    }

    fn trace_abstract_edge(edge: &AbstractEdge) -> String {
        let functional = match edge.functional {
            FunctionalDirection::None => "none",
            FunctionalDirection::SrcToDst => "src->dst",
            FunctionalDirection::DstToSrc => "dst->src",
            FunctionalDirection::Both => "both",
        };
        format!(
            "{}->{} path_vertices={:?} original_edges={:?} predicates={} selectivity={:.6} functional={}",
            edge.src,
            edge.dst,
            edge.path_vertices,
            edge.original_edge_ids,
            Self::trace_predicates(&edge.predicates),
            edge.selectivity,
            functional
        )
    }

    fn trace_abstract_edge_set(edges: &[AbstractEdge]) -> String {
        if edges.is_empty() {
            return "<empty>".to_string();
        }
        edges
            .iter()
            .enumerate()
            .map(|(idx, edge)| format!("ae{}:{}", idx + 1, Self::trace_abstract_edge(edge)))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    fn trace_query_summary(&self) -> String {
        let mut out = String::new();
        out.push_str("vertices:\n");
        let mut vertex_ids: Vec<_> = self.inner.vertices.keys().copied().collect();
        vertex_ids.sort_unstable();
        for vid in vertex_ids {
            if let Some(vertex) = self.inner.vertices.get(&vid) {
                out.push_str(&format!(
                    "  v{} label={} predicates={} degree={}\n",
                    vid,
                    vertex.label,
                    Self::trace_predicates(&vertex.predicates),
                    self.get_degree(vid)
                ));
            }
        }
        out.push_str("edges:\n");
        let mut edge_ids: Vec<_> = self.inner.edges.keys().copied().collect();
        edge_ids.sort_unstable();
        for eid in edge_ids {
            out.push_str(&format!("  {}\n", self.trace_edge_brief(eid, None)));
        }
        out
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

    pub fn estimate_positive_rank_closure_factor_for_abstract_graph_flat(
        &self,
        abstract_graph: &AbstractGraph,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: Option<&FlatGraph>,
    ) -> f64 {
        let Some(fg) = flat_graph else {
            return 1.0;
        };

        let covered_edges: HashSet<EdgeId> = abstract_graph
            .edges
            .values()
            .flat_map(|edge| edge.original_edge_ids.iter().copied())
            .collect();
        let min_tree_edges = self.inner.vertices.len().saturating_sub(1);
        if covered_edges.len() < min_tree_edges {
            return 1.0;
        }

        let mut factor = 1.0f64;

        for edge in self.inner.edges.values() {
            if covered_edges.contains(&edge.id) {
                continue;
            }

            if let Some(rank_factor) = self.estimate_positive_rank_edge_closure_factor(
                edge,
                &covered_edges,
                degree_seq_graph,
                fg,
            ) {
                factor *= rank_factor;
                continue;
            }

            // No legacy density fallback: only the positive-rank estimator is applied.
        }

        factor.clamp(0.0, 1.0)
    }

    fn estimate_positive_rank_edge_closure_factor(
        &self,
        missing_edge: &QueryEdge,
        covered_edges: &HashSet<EdgeId>,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: &FlatGraph,
    ) -> Option<f64> {
        let (_, missing_path_edges) = self.find_two_hop_path_in_edge_set(
            covered_edges,
            missing_edge.dst_vertex_id,
            missing_edge.src_vertex_id,
        )?;

        let cycle_edges: HashSet<EdgeId> = [
            missing_edge.id,
            missing_path_edges[0],
            missing_path_edges[1],
        ]
        .into_iter()
        .collect();
        let mut directional_taus = Vec::with_capacity(3);
        let mut directional_path_cards = Vec::with_capacity(3);
        let mut missing_direction_path_card = None;

        let mut cycle_edge_ids: Vec<EdgeId> = cycle_edges.iter().copied().collect();
        cycle_edge_ids.sort_unstable();
        for closing_edge_id in cycle_edge_ids {
            let closing_edge = self.inner.edges.get(&closing_edge_id)?;
            let path_edge_set: HashSet<EdgeId> = cycle_edges
                .iter()
                .copied()
                .filter(|edge_id| *edge_id != closing_edge_id)
                .collect();
            let (path_vertices, path_edges) = self.find_two_hop_path_in_edge_set(
                &path_edge_set,
                closing_edge.dst_vertex_id,
                closing_edge.src_vertex_id,
            )?;
            let (tau, w_path) = self.estimate_positive_rank_direction_tau(
                closing_edge,
                path_vertices,
                path_edges,
                degree_seq_graph,
                flat_graph,
            )?;
            if closing_edge_id == missing_edge.id {
                missing_direction_path_card = Some(w_path);
            }
            directional_taus.push(tau);
            directional_path_cards.push(w_path);
        }

        if directional_taus.len() != 3 {
            return None;
        }
        directional_taus.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let upper = directional_path_cards
            .into_iter()
            .fold(f64::INFINITY, |acc, value| acc.min(value));
        let tau = directional_taus[1].clamp(0.0, upper);
        let w_path = missing_direction_path_card?;
        if w_path <= 0.0 {
            return Some(0.0);
        }

        Some((tau / w_path).clamp(0.0, 1.0))
    }

    fn estimate_positive_rank_direction_tau(
        &self,
        closing_edge: &QueryEdge,
        path_vertices: [VertexId; 3],
        path_edges: [EdgeId; 2],
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: &FlatGraph,
    ) -> Option<(f64, f64)> {
        let path_node_labels: Vec<String> = path_vertices
            .iter()
            .map(|vertex_id| self.inner.vertices.get(vertex_id).map(|v| v.label.clone()))
            .collect::<Option<Vec<_>>>()?;
        let path_edge_labels: Vec<String> = path_edges
            .iter()
            .map(|edge_id| self.inner.edges.get(edge_id).map(|e| e.label.clone()))
            .collect::<Option<Vec<_>>>()?;
        let path_key = make_alt_key(&path_node_labels, &path_edge_labels);

        let path_start_label = path_node_labels.first()?;
        let path_end_label = path_node_labels.last()?;
        let path_start_pcf = degree_seq_graph.get_piece_func_by_path(&path_key, path_start_label);
        let path_end_pcf = degree_seq_graph.get_piece_func_by_path(&path_key, path_end_label);
        if path_start_pcf.is_empty_placeholder() || path_end_pcf.is_empty_placeholder() {
            return None;
        }

        let closing_src_vertex = self.inner.vertices.get(&closing_edge.src_vertex_id)?;
        let closing_dst_vertex = self.inner.vertices.get(&closing_edge.dst_vertex_id)?;
        let closing_node_labels = vec![
            closing_src_vertex.label.clone(),
            closing_dst_vertex.label.clone(),
        ];
        let closing_edge_labels = vec![closing_edge.label.clone()];
        let closing_key = make_alt_key(&closing_node_labels, &closing_edge_labels);

        let mut closing_src_pcf =
            degree_seq_graph.get_piece_func_by_path(&closing_key, &closing_src_vertex.label);
        let mut closing_dst_pcf =
            degree_seq_graph.get_piece_func_by_path(&closing_key, &closing_dst_vertex.label);
        if closing_src_pcf.is_empty_placeholder() || closing_dst_pcf.is_empty_placeholder() {
            return None;
        }

        let edge_selectivity =
            self.estimate_query_edge_predicate_selectivity(closing_edge, flat_graph);
        if edge_selectivity <= 0.0 {
            return Some((
                0.0,
                Self::pcf_relation_cardinality(&path_start_pcf, &path_end_pcf),
            ));
        }
        if edge_selectivity < 1.0 {
            closing_src_pcf = closing_src_pcf.scale_by_ratio(edge_selectivity);
            closing_dst_pcf = closing_dst_pcf.scale_by_ratio(edge_selectivity);
        }

        let w_path = Self::pcf_relation_cardinality(&path_start_pcf, &path_end_pcf);
        let closing_card = Self::pcf_relation_cardinality(&closing_src_pcf, &closing_dst_pcf);
        if w_path <= 0.0 || closing_card <= 0.0 {
            return Some((0.0, w_path));
        }

        let dst_population = flat_graph.vertex_count_by_label(&closing_dst_vertex.label) as f64;
        let src_population = flat_graph.vertex_count_by_label(&closing_src_vertex.label) as f64;
        if dst_population <= 0.0 || src_population <= 0.0 {
            return Some((0.0, w_path));
        }

        let j_dst = Self::pcf_positive_rank_dot(&path_start_pcf, &closing_dst_pcf, dst_population);
        let j_src = Self::pcf_positive_rank_dot(&path_end_pcf, &closing_src_pcf, src_population);
        let tau = j_dst * j_src / (w_path * closing_card);

        Some((tau, w_path))
    }

    fn find_two_hop_path_in_edge_set(
        &self,
        edge_set: &HashSet<EdgeId>,
        start: VertexId,
        end: VertexId,
    ) -> Option<([VertexId; 3], [EdgeId; 2])> {
        if start == end {
            return None;
        }

        let mut first_edges = Vec::new();
        first_edges.extend(self.get_outgoing_edges(start));
        first_edges.extend(self.get_incoming_edges(start));

        for first in first_edges {
            if !edge_set.contains(&first.id) {
                continue;
            }
            let mid = if first.src_vertex_id == start {
                first.dst_vertex_id
            } else {
                first.src_vertex_id
            };
            if mid == start {
                continue;
            }

            let mut second_edges = Vec::new();
            second_edges.extend(self.get_outgoing_edges(mid));
            second_edges.extend(self.get_incoming_edges(mid));
            for second in second_edges {
                if second.id == first.id || !edge_set.contains(&second.id) {
                    continue;
                }
                let other = if second.src_vertex_id == mid {
                    second.dst_vertex_id
                } else {
                    second.src_vertex_id
                };
                if other == end {
                    return Some(([start, mid, end], [first.id, second.id]));
                }
            }
        }

        None
    }

    fn pcf_positive_rank_dot(left: &Pcf, right: &Pcf, population: f64) -> f64 {
        if population <= 0.0 {
            return 0.0;
        }
        let mut left_desc = Self::pcf_segments(left, population);
        let mut right_desc = Self::pcf_segments(right, population);
        left_desc.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        right_desc.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Self::pcf_segment_dot(&left_desc, &right_desc)
    }

    fn estimate_query_edge_predicate_selectivity(
        &self,
        edge: &QueryEdge,
        flat_graph: &FlatGraph,
    ) -> f64 {
        let mut edge_selectivity = 1.0f64;
        for pred in &edge.predicates {
            edge_selectivity *= Self::estimate_selectivity_from_stats(
                flat_graph.edge_column_stats(&edge.label, &pred.property),
                &pred.op,
                &pred.value,
            );
        }
        edge_selectivity.clamp(0.0, 1.0)
    }

    fn pcf_relation_cardinality(src_pcf: &Pcf, dst_pcf: &Pcf) -> f64 {
        let src_rows = src_pcf.get_num_rows();
        let dst_rows = dst_pcf.get_num_rows();
        if src_rows <= 0.0 {
            return dst_rows.max(0.0);
        }
        if dst_rows <= 0.0 {
            return src_rows.max(0.0);
        }
        src_rows.min(dst_rows)
    }

    fn pcf_segments(pcf: &Pcf, population: f64) -> Vec<(f64, f64)> {
        let mut segments = Vec::new();
        let mut left = 0.0;
        for (idx, right) in pcf.right_interval_edges.iter().copied().enumerate() {
            let capped_right = right.min(population).max(left);
            let width = capped_right - left;
            if width > 0.0 {
                segments.push((width, pcf.constants[idx].max(0.0)));
            }
            left = capped_right;
            if left >= population {
                break;
            }
        }
        if left < population {
            segments.push((population - left, 0.0));
        }
        segments
    }

    fn pcf_segment_dot(left: &[(f64, f64)], right: &[(f64, f64)]) -> f64 {
        let mut i = 0usize;
        let mut j = 0usize;
        let mut left_remaining = left.first().map(|x| x.0).unwrap_or(0.0);
        let mut right_remaining = right.first().map(|x| x.0).unwrap_or(0.0);
        let mut total = 0.0;

        while i < left.len() && j < right.len() {
            let width = left_remaining.min(right_remaining);
            if width > 0.0 {
                total += width * left[i].1 * right[j].1;
                left_remaining -= width;
                right_remaining -= width;
            }

            if left_remaining <= 1e-9 {
                i += 1;
                left_remaining = left.get(i).map(|x| x.0).unwrap_or(0.0);
            }
            if right_remaining <= 1e-9 {
                j += 1;
                right_remaining = right.get(j).map(|x| x.0).unwrap_or(0.0);
            }
        }

        total
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
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[tree-enum] initial_best_tree score={} edges={}",
                first_tree.total_score,
                self.trace_edge_set(&first_tree.edge_ids, Some(cardinalities))
            ));
        }
        let mut sorted_key: Vec<EdgeId> = first_tree.edge_ids.iter().copied().collect();
        sorted_key.sort();
        seen_trees.insert(sorted_key);
        candidates.push(first_tree);

        while result.len() < k && !candidates.is_empty() {
            let current = candidates.pop().unwrap();
            let current_edge_set = current.edge_ids.clone();
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[tree-enum] emit_tree rank={} score={} edges={}",
                    result.len() + 1,
                    current.total_score,
                    self.trace_edge_set(&current_edge_set, Some(cardinalities))
                ));
            }

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

            for (new_edge_id, new_edge_card) in non_tree_edges {
                if let Some(new_edge) = self.inner.edges.get(&new_edge_id) {
                    let src = new_edge.src_vertex_id;
                    let dst = new_edge.dst_vertex_id;
                    if decomp_trace_enabled() {
                        decomp_trace_line(format!(
                            "[cycle-break] tree_rank={} add_non_tree_edge={} card={} src={} dst={}",
                            result.len(),
                            new_edge_id,
                            new_edge_card,
                            src,
                            dst
                        ));
                    }

                    if let Some(path_edges) =
                        self.find_path_edges_in_tree(&current.edge_ids, src, dst)
                    {
                        if decomp_trace_enabled() {
                            let mut cycle_edges = path_edges.clone();
                            cycle_edges.insert(new_edge_id);
                            decomp_trace_line(format!(
                                "[cycle-break] formed_cycle path_edges={} cycle_edges={}",
                                self.trace_edge_set(&path_edges, Some(cardinalities)),
                                self.trace_edge_set(&cycle_edges, Some(cardinalities))
                            ));
                        }
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

                        for (edge_to_remove, old_card) in path_edges_with_card {
                            let mut new_edge_set = current.edge_ids.clone();
                            new_edge_set.remove(&edge_to_remove);
                            new_edge_set.insert(new_edge_id);

                            let mut sorted_key: Vec<EdgeId> =
                                new_edge_set.iter().copied().collect();
                            sorted_key.sort();
                            if seen_trees.contains(&sorted_key) {
                                if decomp_trace_enabled() {
                                    decomp_trace_line(format!(
                                        "[cycle-break] remove_edge={} old_card={} result=skip_seen edges={}",
                                        edge_to_remove,
                                        old_card,
                                        self.trace_edge_set(&new_edge_set, Some(cardinalities))
                                    ));
                                }
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

                                if decomp_trace_enabled() {
                                    decomp_trace_line(format!(
                                        "[cycle-break] remove_edge={} old_card={} result=push_candidate score={} edges={}",
                                        edge_to_remove,
                                        old_card,
                                        total_score,
                                        self.trace_edge_set(&new_edge_set, Some(cardinalities))
                                    ));
                                }
                                seen_trees.insert(sorted_key);
                                candidates.push(candidate);
                            } else if decomp_trace_enabled() {
                                decomp_trace_line(format!(
                                    "[cycle-break] remove_edge={} old_card={} result=skip_not_tree edges={}",
                                    edge_to_remove,
                                    old_card,
                                    self.trace_edge_set(&new_edge_set, Some(cardinalities))
                                ));
                            }
                        }
                    } else {
                        if decomp_trace_enabled() {
                            decomp_trace_line(format!(
                                "[cycle-break] add_non_tree_edge={} did_not_find_existing_path",
                                new_edge_id
                            ));
                        }
                        let mut new_edge_set = current.edge_ids.clone();
                        new_edge_set.insert(new_edge_id);

                        let mut sorted_key: Vec<EdgeId> = new_edge_set.iter().copied().collect();
                        sorted_key.sort();
                        if seen_trees.contains(&sorted_key) {
                            if decomp_trace_enabled() {
                                decomp_trace_line(format!(
                                    "[cycle-break] disconnected_add result=skip_seen edges={}",
                                    self.trace_edge_set(&new_edge_set, Some(cardinalities))
                                ));
                            }
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

                            if decomp_trace_enabled() {
                                decomp_trace_line(format!(
                                    "[cycle-break] disconnected_add result=push_candidate score={} edges={}",
                                    total_score,
                                    self.trace_edge_set(&new_edge_set, Some(cardinalities))
                                ));
                            }
                            seen_trees.insert(sorted_key);
                            candidates.push(candidate);
                        } else if decomp_trace_enabled() {
                            decomp_trace_line(format!(
                                "[cycle-break] disconnected_add result=skip_not_tree edges={}",
                                self.trace_edge_set(&new_edge_set, Some(cardinalities))
                            ));
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

/// Aggregated output of `run_walk_batch`.
struct WalkBatchResult {
    sum_struct_weight: f64,
    sum_pred_weight: f64,
    struct_success_sample_count: usize,
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
        let mut visited_paths: HashSet<Vec<EdgeId>> = HashSet::new();

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
                        let mut reverse_edges = path.edges.clone();
                        reverse_edges.reverse();
                        let path_key = if path.edges <= reverse_edges {
                            path.edges.clone()
                        } else {
                            reverse_edges
                        };
                        if visited_paths.insert(path_key) {
                            paths.push(path);
                        }
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

        if decomp_trace_enabled() {
            decomp_trace_line(format!("[path] {}", self.trace_path(path)));
        }

        let mut functional_ranges = self.abstract_edge_ranges_for_path(path, k);
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[path-ranges] default ranges={}",
                self.trace_ranges(path, &functional_ranges)
            ));
        }
        let mut i = 0;
        while i + 1 < functional_ranges.len() {
            let cut_edge_idx = functional_ranges[i].1;
            let merged = (functional_ranges[i].0, functional_ranges[i + 1].1);
            let can_contract = self.can_contract_path_cut(path, cut_edge_idx, degree_seq_graph);
            let has_catalog =
                self.path_slice_has_catalog(path, merged.0, merged.1, degree_seq_graph);

            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[path-merge-check] cut_edge_idx={} merged={}..{} can_contract={} has_catalog={}",
                    cut_edge_idx, merged.0, merged.1, can_contract, has_catalog
                ));
            }
            if can_contract && has_catalog {
                functional_ranges[i] = merged;
                functional_ranges.remove(i + 1);
                if decomp_trace_enabled() {
                    decomp_trace_line(format!(
                        "[path-merge] accepted ranges={}",
                        self.trace_ranges(path, &functional_ranges)
                    ));
                }
                i = i.saturating_sub(1);
            } else {
                i += 1;
            }
        }
        add_ranges(functional_ranges.clone(), &mut candidates)?;
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[path-candidate] mode=functional_merge_or_default ranges={} abstract_edges={}",
                self.trace_ranges(path, &functional_ranges),
                Self::trace_abstract_edge_set(candidates.last().map(Vec::as_slice).unwrap_or(&[]))
            ));
        }

        for ranges in self.abstract_edge_range_variants_for_path(path, k) {
            let before = candidates.len();
            add_ranges(ranges.clone(), &mut candidates)?;
            if decomp_trace_enabled() {
                if candidates.len() > before {
                    decomp_trace_line(format!(
                        "[path-candidate] mode=range_variant ranges={} abstract_edges={}",
                        self.trace_ranges(path, &ranges),
                        Self::trace_abstract_edge_set(
                            candidates.last().map(Vec::as_slice).unwrap_or(&[])
                        )
                    ));
                } else {
                    decomp_trace_line(format!(
                        "[path-candidate] mode=range_variant ranges={} result=skip_duplicate",
                        self.trace_ranges(path, &ranges)
                    ));
                }
            }
        }

        if decomp_trace_enabled() {
            decomp_trace_line(format!("[path-candidate] total={}", candidates.len()));
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
enum FlatCompiledStep<'graph> {
    Vertex {
        /// Vertex label (lowercased), used to look up vertex properties.
        label: String,
        predicates: Vec<ResolvedPredicate>,
    },
    Edge {
        /// Unique within one compiled plan; used as an allocation-free
        /// neighbor-cache key together with the current vertex ID.
        cache_slot: usize,
        /// Pre-resolved immutable adjacency bucket. `None` represents a hop
        /// absent from the current FlatGraph and therefore an empty row.
        csr: Option<&'graph CsrAdjWithEid>,
        edge_label: String,
        predicates: Vec<ResolvedPredicate>,
    },
}

/// One compiled walk plan for a [`PathQuery`].
///
/// A plan is parametrized by `start_idx` — the index of the vertex in the
/// path where the walk begins.  From that vertex it walks the path in two
/// independent segments:
///
/// * `left_segment` traverses edges with index `start_idx-1, start_idx-2, …, 0` in **reversed**
///   direction (the walker moves from a "later" vertex on the original path to an "earlier" one).
/// * `right_segment` traverses edges with index `start_idx, start_idx+1, …, k-1` in their
///   **original** direction.
///
/// When `start_idx == 0` the walk degenerates to the classic forward walk
/// (empty left segment).  When `start_idx == k_edges` it is a pure reverse
/// walk.  Otherwise it is a split walk that branches from the middle.
///
/// Property indices are resolved via [`FlatGraph::vertex_prop_index`] /
/// [`FlatGraph::edge_prop_index`].  If a property cannot be resolved (e.g.
/// the FlatGraph was built without properties), the predicate is silently
/// dropped — the walk treats the step as always-passing, giving selectivity
/// 1.0 for that predicate.
struct FlatCompiledPathQuery<'graph> {
    start_idx: usize,
    /// Label of the start vertex, used for sampling start vertices.
    start_label: String,
    /// Predicates that apply to the start vertex.
    start_predicates: Vec<ResolvedPredicate>,
    /// Steps for the left segment, alternating Edge, Vertex, Edge, Vertex,
    /// ending at the leftmost vertex on the path.  Empty when `start_idx == 0`.
    left_segment: Vec<FlatCompiledStep<'graph>>,
    /// Steps for the right segment, alternating Edge, Vertex, Edge, Vertex,
    /// ending at the rightmost vertex on the path.  Empty when
    /// `start_idx == k_edges`.
    right_segment: Vec<FlatCompiledStep<'graph>>,
}

impl<'graph> FlatCompiledPathQuery<'graph> {
    /// Build all `k_edges + 1` plans for the given path query.
    fn compile_all(
        path_query: &PathQuery,
        flat_graph: &'graph FlatGraph,
    ) -> GCardResult<Vec<Self>> {
        // 1) Extract a linear list of vertices and edges with resolved predicates.
        let mut vertex_infos: Vec<(String, Vec<ResolvedPredicate>)> = Vec::new();
        let mut edge_infos: Vec<(String, EdgeDirection, Vec<ResolvedPredicate>)> = Vec::new();

        for element in &path_query.path_elements {
            match element {
                PathElement::Vertex { label, position } => {
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
                    vertex_infos.push((label.clone(), predicates));
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
                    edge_infos.push((label.clone(), direction.clone(), predicates));
                }
            }
        }

        if vertex_infos.is_empty() {
            return Ok(Vec::new());
        }

        let k = edge_infos.len();
        let mut plans = Vec::with_capacity(k + 1);

        for start_idx in 0..=k {
            let (start_label, start_predicates) = vertex_infos[start_idx].clone();
            let mut next_cache_slot = 0;

            // Left segment: walk from start_idx down to 0.  Each edge is
            // traversed against its original direction.
            let mut left_segment = Vec::new();
            for i in (0..start_idx).rev() {
                let (edge_label, dir, edge_preds) = edge_infos[i].clone();
                let outgoing = matches!(dir, EdgeDirection::Incoming);
                let csr =
                    flat_graph.hop_csr_for_label(&vertex_infos[i + 1].0, &edge_label, outgoing);
                left_segment.push(FlatCompiledStep::Edge {
                    cache_slot: next_cache_slot,
                    csr,
                    edge_label,
                    predicates: edge_preds,
                });
                next_cache_slot += 1;
                let (v_label, v_preds) = vertex_infos[i].clone();
                left_segment.push(FlatCompiledStep::Vertex {
                    label: v_label,
                    predicates: v_preds,
                });
            }

            // Right segment: walk from start_idx up to k.  Edges keep their
            // original direction.
            let mut right_segment = Vec::new();
            for i in start_idx..k {
                let (edge_label, dir, edge_preds) = edge_infos[i].clone();
                let outgoing = matches!(dir, EdgeDirection::Outgoing);
                let csr = flat_graph.hop_csr_for_label(&vertex_infos[i].0, &edge_label, outgoing);
                right_segment.push(FlatCompiledStep::Edge {
                    cache_slot: next_cache_slot,
                    csr,
                    edge_label,
                    predicates: edge_preds,
                });
                next_cache_slot += 1;
                let (v_label, v_preds) = vertex_infos[i + 1].clone();
                right_segment.push(FlatCompiledStep::Vertex {
                    label: v_label,
                    predicates: v_preds,
                });
            }

            plans.push(Self {
                start_idx,
                start_label,
                start_predicates,
                left_segment,
                right_segment,
            });
        }

        Ok(plans)
    }
}

// ── FlatGraph methods on QueryGraph ──────────────────────────────────────────

impl QueryGraph {
    // ── Public entry point ────────────────────────────────────────────────────

    fn single_vertex_sample_seed(label: &str, predicates: &[PredicateDef]) -> u64 {
        // Stable FNV-1a makes repeated provider generation reproducible.
        let mut hash = 0xcbf29ce484222325u64;
        for byte in format!("{}:{:?}", label, predicates).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn single_vertex_upper_selectivity(hits: usize, samples: usize) -> f64 {
        debug_assert!(samples > 0 && hits <= samples);
        if hits == 0 {
            // Exact one-sided Clopper-Pearson upper bound for zero successes
            // at 95% confidence (approximately the rule of three).
            return 1.0 - 0.05f64.powf(1.0 / samples as f64);
        }

        // One-sided 95% Wilson upper bound. GCard is an upper estimator, so
        // use the confidence bound rather than the raw hit ratio.
        const Z: f64 = 1.644_853_626_951_472_2;
        let n = samples as f64;
        let p = hits as f64 / n;
        let z2 = Z * Z;
        let denominator = 1.0 + z2 / n;
        let center = p + z2 / (2.0 * n);
        let margin = Z * ((p * (1.0 - p) / n) + z2 / (4.0 * n * n)).sqrt();
        ((center + margin) / denominator).clamp(0.0, 1.0)
    }

    fn estimate_single_vertex_cardinality(
        &self,
        flat_graph: &FlatGraph,
        label: &str,
        predicates: &[PredicateDef],
        sample_size: usize,
    ) -> GCardResult<u64> {
        const MIN_HITS: usize = 3;
        const MAX_SAMPLE_MULTIPLIER: usize = 8;

        let total = flat_graph.vertex_count_by_label(label);
        if total == 0 || predicates.is_empty() {
            return Ok(total as u64);
        }

        let resolved = predicates
            .iter()
            .map(|predicate| {
                let prop_index = flat_graph
                    .vertex_prop_index(label, &predicate.property)
                    .ok_or_else(|| {
                        GCardError::InvalidState(format!(
                            "property {} is missing from FlatGraph label {}",
                            predicate.property, label
                        ))
                    })?;
                Ok(ResolvedPredicate {
                    predicate_id: predicate.predicate_id,
                    prop_index,
                    op: predicate.op,
                    value: predicate.value.clone(),
                })
            })
            .collect::<GCardResult<Vec<_>>>()?;

        let initial_samples = sample_size.max(1).min(total);
        let max_samples = sample_size
            .max(1)
            .saturating_mul(MAX_SAMPLE_MULTIPLIER)
            .min(total);
        let mut rng =
            rand::rngs::StdRng::seed_from_u64(Self::single_vertex_sample_seed(label, predicates));
        let sampled = flat_graph.sample_vertices_by_label(label, max_samples, &mut rng);

        let mut evaluated = 0usize;
        let mut hits = 0usize;
        let mut target = initial_samples;
        loop {
            hits += sampled[evaluated..target]
                .iter()
                .filter(|&&vid| self.vertex_passes_predicates_uncached(flat_graph, vid, &resolved))
                .count();
            evaluated = target;
            if hits >= MIN_HITS || evaluated == max_samples {
                break;
            }
            target = evaluated.saturating_mul(2).min(max_samples);
        }

        if evaluated == total {
            return Ok(hits as u64);
        }

        let selectivity = Self::single_vertex_upper_selectivity(hits, evaluated);
        Ok(((total as f64) * selectivity).ceil().min(total as f64) as u64)
    }

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
        // Share the filter pool cache across all queries — same `(label,
        // predicate-set)` tuple always yields the same filtered vertex list
        // regardless of which query asked, and the upfront scan cost
        // dominates the predicate-aware sampler.
        let filtered_pool_cache: Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        > = GLOBAL_FILTERED_POOL_CACHE.clone();

        // A one-vertex query has no path to decompose. Represent its label
        // cardinality (and optional predicate selectivity) as a unary PCF so
        // it can use the same AbstractGraph reduction/result-selection path
        // as larger queries.
        if self.inner.edges.is_empty() {
            if self.inner.vertices.len() != 1 {
                return Err(GCardError::InvalidState(format!(
                    "edge-free query must contain exactly one vertex, found {}",
                    self.inner.vertices.len()
                )));
            }
            let vertex = self.inner.vertices.values().next().unwrap();
            let flat_graph = flat_graph.ok_or_else(|| {
                GCardError::InvalidState(
                    "single-vertex query requires FlatGraph statistics".to_string(),
                )
            })?;
            let estimated = if matches!(predicate_apply_type, PredicateApplyType::IGNORE) {
                flat_graph.vertex_count_by_label(&vertex.label) as u64
            } else {
                self.estimate_single_vertex_cardinality(
                    flat_graph,
                    &vertex.label,
                    &vertex.predicates,
                    sample_size,
                )?
            };
            let local_pcf = if estimated == 0 {
                // `Pcf::empty()` is a safe algebra placeholder with one
                // cumulative row, so use an actual zero-row unary factor for
                // a label/predicate combination estimated to be empty.
                Pcf {
                    constants: vec![0.0],
                    right_interval_edges: vec![0.0],
                    cumulative_rows: vec![0.0],
                }
            } else {
                Pcf::from_degree_sequence(&[estimated], 0.01, false)?
            };
            let mut abstract_graph = AbstractGraph::new();
            abstract_graph.add_vertex(vertex.clone());
            abstract_graph.add_local_pcf(vertex.id, local_pcf);
            return Ok(vec![(abstract_graph, 0)]);
        }

        if decomp_trace_enabled() {
            decomp_trace_line("==== GCARD decomposition trace begin ====");
            decomp_trace_line(format!(
                "[query] k={} tree_num={} sample_size={} predicate_apply_type={:?} unit_selectivity_walks={}",
                k, tree_num, sample_size, predicate_apply_type, unit_selectivity_walks
            ));
            decomp_trace_multiline(self.trace_query_summary());
        }

        let t_cycle = std::time::Instant::now();
        let has_cycle = self.has_cycle();
        BUILD_CYCLE_CHECK_NANOS.fetch_add(t_cycle.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if decomp_trace_enabled() {
            decomp_trace_line(format!("[cycle-check] has_cycle={}", has_cycle));
        }

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
                &filtered_pool_cache,
            )?;
            return Ok(abstract_graphs
                .into_iter()
                .map(|abstract_graph| (abstract_graph, 0))
                .collect());
        }

        // ── Cycle case: enumerate spanning trees ──────────────────────────────
        let t_tree = std::time::Instant::now();
        let cardinalities = self.estimate_edge_cardinalities(flat_graph);
        if decomp_trace_enabled() {
            let mut edge_ids: Vec<_> = self.inner.edges.keys().copied().collect();
            edge_ids.sort_unstable();
            decomp_trace_line("[edge-cardinality] estimated query edge cardinalities:");
            for eid in edge_ids {
                decomp_trace_line(format!(
                    "  {}",
                    self.trace_edge_brief(eid, Some(&cardinalities))
                ));
            }
        }
        let trees_with_scores = self.build_k_best_trees(&cardinalities, tree_num.max(1));
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[tree-enum] selected_tree_count={} requested={}",
                trees_with_scores.len(),
                tree_num.max(1)
            ));
            for (idx, (tree, score)) in trees_with_scores.iter().enumerate() {
                let tree_edges: HashSet<EdgeId> = tree.inner.edges.keys().copied().collect();
                decomp_trace_line(format!(
                    "[tree] rank={} score={} edges={}",
                    idx + 1,
                    score,
                    self.trace_edge_set(&tree_edges, Some(&cardinalities))
                ));
            }
        }
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
                    &filtered_pool_cache,
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
        // Share the filter pool cache across all queries — same `(label,
        // predicate-set)` tuple always yields the same filtered vertex list
        // regardless of which query asked, and the upfront scan cost
        // dominates the predicate-aware sampler.
        let filtered_pool_cache: Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        > = GLOBAL_FILTERED_POOL_CACHE.clone();

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
                    &filtered_pool_cache,
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
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
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
                    filtered_pool_cache,
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
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> GCardResult<Vec<AbstractGraph>> {
        let t_pivot = std::time::Instant::now();
        let pivot_nodes = query_graph.find_pivot_nodes();
        let paths = query_graph.find_paths_from_pivots(&pivot_nodes);
        BUILD_PIVOT_PATH_NANOS.fetch_add(t_pivot.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if decomp_trace_enabled() {
            let subgraph_edges: HashSet<EdgeId> = query_graph.inner.edges.keys().copied().collect();
            let mut pivots: Vec<_> = pivot_nodes.iter().copied().collect();
            pivots.sort_unstable();
            decomp_trace_line(format!(
                "[decompose-tree] edges={} pivots={:?} path_count={}",
                query_graph.trace_edge_set(&subgraph_edges, None),
                pivots,
                paths.len()
            ));
            for (idx, path) in paths.iter().enumerate() {
                decomp_trace_line(format!(
                    "[decompose-tree] path{} {}",
                    idx + 1,
                    query_graph.trace_path(path)
                ));
            }
        }

        let t_ae = std::time::Instant::now();
        let limit = Self::split_candidate_limit();
        let mut edge_sets: Vec<Vec<AbstractEdge>> = vec![Vec::new()];
        for (path_idx, path) in paths.into_iter().enumerate() {
            let path_candidates =
                query_graph.build_abstract_edge_candidates_for_path(&path, k, degree_seq_graph)?;
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[cross-product] path_index={} incoming_sets={} path_candidates={} limit={}",
                    path_idx + 1,
                    edge_sets.len(),
                    path_candidates.len(),
                    limit
                ));
            }
            let mut next = Vec::new();
            for existing in &edge_sets {
                let used_original_edges: HashSet<EdgeId> = existing
                    .iter()
                    .flat_map(|edge| edge.original_edge_ids.iter().copied())
                    .collect();
                for candidate in &path_candidates {
                    let overlaps_existing = candidate.iter().any(|edge| {
                        edge.original_edge_ids
                            .iter()
                            .any(|edge_id| used_original_edges.contains(edge_id))
                    });
                    if overlaps_existing {
                        continue;
                    }
                    let mut candidate_original_edges = HashSet::new();
                    let overlaps_within_candidate = candidate.iter().any(|edge| {
                        edge.original_edge_ids
                            .iter()
                            .any(|edge_id| !candidate_original_edges.insert(*edge_id))
                    });
                    if overlaps_within_candidate {
                        continue;
                    }
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
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[cross-product] path_index={} outgoing_sets={}",
                    path_idx + 1,
                    edge_sets.len()
                ));
            }
        }
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[abstract-edge-combo] count={} limit={}",
                edge_sets.len(),
                limit
            ));
            for (idx, edges) in edge_sets.iter().enumerate() {
                decomp_trace_line(format!(
                    "[abstract-edge-combo] idx={} edges={}",
                    idx + 1,
                    Self::trace_abstract_edge_set(edges)
                ));
            }
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
                    filtered_pool_cache,
                )
            })
            .collect::<GCardResult<Vec<_>>>()?;

        if let Some((local_pcfs, consumed_edges)) =
            query_graph.extract_raw_star_local_pcfs(degree_seq_graph)
        {
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[raw-star-mode] accepted consumed_edges={:?} local_pcf_centers={:?}",
                    {
                        let mut ids: Vec<_> = consumed_edges.iter().copied().collect();
                        ids.sort_unstable();
                        ids
                    },
                    {
                        let mut centers: Vec<_> = local_pcfs.keys().copied().collect();
                        centers.sort_unstable();
                        centers
                    }
                ));
            }
            let residual = query_graph.subgraph_without_edges(&consumed_edges);
            let pivot_nodes = residual.find_pivot_nodes();
            let residual_paths = residual.find_paths_from_pivots(&pivot_nodes);
            if decomp_trace_enabled() {
                let mut pivots: Vec<_> = pivot_nodes.iter().copied().collect();
                pivots.sort_unstable();
                decomp_trace_line(format!(
                    "[raw-star-mode] residual pivots={:?} path_count={}",
                    pivots,
                    residual_paths.len()
                ));
            }
            let mut residual_edge_sets: Vec<Vec<AbstractEdge>> = vec![Vec::new()];
            for (path_idx, path) in residual_paths.into_iter().enumerate() {
                let path_candidates =
                    residual.build_abstract_edge_candidates_for_path(&path, k, degree_seq_graph)?;
                if decomp_trace_enabled() {
                    decomp_trace_line(format!(
                        "[raw-star-cross-product] path_index={} incoming_sets={} path_candidates={} limit={}",
                        path_idx + 1,
                        residual_edge_sets.len(),
                        path_candidates.len(),
                        limit
                    ));
                }
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
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[raw-star-combo] residual_combo_count={}",
                    residual_edge_sets.len()
                ));
                for (idx, edges) in residual_edge_sets.iter().enumerate() {
                    decomp_trace_line(format!(
                        "[raw-star-combo] idx={} residual_edges={}",
                        idx + 1,
                        Self::trace_abstract_edge_set(edges)
                    ));
                }
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
                    filtered_pool_cache,
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
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> GCardResult<AbstractGraph> {
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[build-abstract-graph] input_edges={} initial_local_pcf_centers={:?}",
                Self::trace_abstract_edge_set(&all_abstract_edges),
                {
                    let mut centers: Vec<_> = initial_local_pcfs.keys().copied().collect();
                    centers.sort_unstable();
                    centers
                }
            ));
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
                    selectivity_cache,
                    flat_vertex_cache,
                    pred_cache,
                    filtered_pool_cache,
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
        let mut seen_original_edges = HashSet::new();
        for result in results {
            let abstract_edge = result?;
            for edge_id in &abstract_edge.original_edge_ids {
                if !seen_original_edges.insert(*edge_id) {
                    return Err(GCardError::InvalidState(format!(
                        "duplicate original edge {} in abstract graph decomposition",
                        edge_id
                    )));
                }
            }
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
        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[raw-star-extract] catalog_max_star_degree={} max_star_degree={}",
                catalog_max_star_degree, max_star_degree
            ));
        }
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
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[raw-star-extract] center={} label={} candidate_arms={}",
                    center,
                    center_vertex.label,
                    mergeable
                        .iter()
                        .map(|(edge_id, arm)| format!(
                            "edge={} arm_vs={:?} arm_es={:?}",
                            edge_id, arm.vs, arm.es
                        ))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }

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

                if decomp_trace_enabled() {
                    let selected_edges = selected_indices
                        .iter()
                        .map(|&idx| mergeable[idx].0)
                        .collect::<Vec<_>>();
                    decomp_trace_line(format!(
                        "[raw-star-extract] center={} selected_edges={:?} pcf_rows={:.0}",
                        center,
                        selected_edges,
                        pcf.get_num_rows()
                    ));
                }
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

        if decomp_trace_enabled() {
            decomp_trace_line(format!(
                "[post-star-extract] catalog_max_star_degree={} catalog_max_star_length={} max_star_degree={} max_star_length={}",
                catalog_max_star_degree, catalog_max_star_length, max_star_degree, max_star_length
            ));
        }
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
            if decomp_trace_enabled() {
                decomp_trace_line(format!(
                    "[post-star-extract] center={} label={} candidate_arms={}",
                    center,
                    center_label,
                    mergeable
                        .iter()
                        .map(|(idx, arm)| format!(
                            "abstract_idx={} arm_vs={:?} arm_es={:?}",
                            idx, arm.vs, arm.es
                        ))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }

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

                if decomp_trace_enabled() {
                    let selected_edges = selected_indices
                        .iter()
                        .map(|&i| {
                            let edge_idx = mergeable[i].0;
                            format!(
                                "idx={} {}",
                                edge_idx,
                                Self::trace_abstract_edge(&edges[edge_idx])
                            )
                        })
                        .collect::<Vec<_>>();
                    decomp_trace_line(format!(
                        "[post-star-extract] center={} selected_abstract_edges={} pcf_rows={:.0}",
                        center,
                        selected_edges.join(" | "),
                        pcf.get_num_rows()
                    ));
                }
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
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
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
                            degree_seq_graph,
                            abstract_edge,
                            sample_size,
                            flat_vertex_cache,
                            pred_cache,
                            filtered_pool_cache,
                        )?
                    } else {
                        self.compute_selectivity_flat(
                            fg,
                            degree_seq_graph,
                            abstract_edge,
                            sample_size,
                            flat_vertex_cache,
                            pred_cache,
                            filtered_pool_cache,
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

    #[allow(clippy::too_many_arguments)]
    fn compute_selectivity_flat(
        &self,
        flat_graph: &FlatGraph,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        abstract_edge: &AbstractEdge,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> GCardResult<f64> {
        let path_query = self.build_path_query(abstract_edge)?;
        self.compute_selectivity_flat_for_path_query(
            flat_graph,
            degree_seq_graph,
            &path_query,
            &abstract_edge.path_str,
            sample_size,
            flat_vertex_cache,
            pred_cache,
            filtered_pool_cache,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_selectivity_flat_unit_paths(
        &self,
        flat_graph: &FlatGraph,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        abstract_edge: &AbstractEdge,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> GCardResult<f64> {
        let unit_queries = self.build_unit_path_queries(abstract_edge)?;
        let mut selectivity = 1.0f64;

        for (hop_idx, path_query) in unit_queries.iter().enumerate() {
            let hop_desc = format!("{} [unit-hop={}]", abstract_edge.path_str, hop_idx);
            let hop_selectivity = self.compute_selectivity_flat_for_path_query(
                flat_graph,
                degree_seq_graph,
                path_query,
                &hop_desc,
                sample_size,
                flat_vertex_cache,
                pred_cache,
                filtered_pool_cache,
            )?;
            selectivity *= hop_selectivity;
        }

        Ok(selectivity)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_selectivity_flat_for_path_query(
        &self,
        flat_graph: &FlatGraph,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        path_query: &PathQuery,
        path_desc: &str,
        sample_size: usize,
        flat_vertex_cache: &Arc<DashMap<String, Vec<VertexId>>>,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> GCardResult<f64> {
        let plans = FlatCompiledPathQuery::compile_all(path_query, flat_graph)?;
        if plans.is_empty() {
            return Ok(1.0);
        }

        // Ordered label sequences for the path (forward order), used to look up
        // per-anchor structural participation from the catalog.
        let mut node_labels: Vec<String> = Vec::new();
        let mut edge_labels: Vec<String> = Vec::new();
        for element in &path_query.path_elements {
            match element {
                PathElement::Vertex { label, .. } => node_labels.push(label.clone()),
                PathElement::Edge { label, .. } => edge_labels.push(label.clone()),
            }
        }

        // Stats-based plan picker (Alley §5 ChooseSamplingOrder): driven by
        // FlatGraph ColumnStats (predicate selectivity) *and* catalog PCF
        // support (structural participation) — no pilot, no scan.
        let (chosen_plan_idx, chosen_participation) = self.select_best_walk_plan(
            flat_graph,
            degree_seq_graph,
            &plans,
            &node_labels,
            &edge_labels,
            sample_size,
            pred_cache,
            filtered_pool_cache,
        );
        let compiled = &plans[chosen_plan_idx];

        // Pre-walk guard: if even the best anchor is expected to land on fewer
        // than one participating start vertex, uniform sampling would almost
        // certainly dead-end on every walk.  Skip the doomed walk entirely and
        // return the static fallback, keeping the PCF's structural cardinality
        // intact while supplying the predicate multiplier analytically.
        if let Some(p) = chosen_participation {
            if (sample_size as f64) * p < 1.0 {
                crate::procedures::gcard_query::WALK_GUARD_SKIPPED
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let fallback = Self::static_fallback_selectivity(flat_graph, compiled);
                if crate::procedures::gcard_query::GCARD_VERBOSE
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    eprintln!(
                        "[selectivity-debug] path={}, GUARD-SKIP participation={:.3e}, fallback_sel={:.6}",
                        path_desc, p, fallback,
                    );
                }
                return Ok(fallback);
            }
        }

        // Walk-fail-then-calculate (Alley §6 "walk-fail-then-calculate"):
        //
        //   Stage 1: sample uniformly from the start label, no filtering.
        //            If the walks succeed often enough, return that estimate.
        //   Stage 2: only if Stage 1's success rate falls below ζ, materialise
        //            the filter pool for the start vertex's predicate and
        //            re-sample with bias correction.
        //
        // This way we only ever pay the O(|label|) filter-scan price for the
        // small set of *tangled* predicate combinations that uniform sampling
        // can't handle — ad-hoc predicates that aren't "hard" never trigger
        // materialisation, so the cost doesn't grow with workload diversity.
        const FAIL_RATE_TRIGGER: f64 = 0.9;

        let stage1_starts = {
            let label = &compiled.start_label;
            if let Some(cached) = flat_vertex_cache.get(label) {
                cached.clone()
            } else {
                let mut rng = rand::thread_rng();
                let vids = flat_graph.sample_vertices_by_label(label, sample_size, &mut rng);
                flat_vertex_cache.insert(label.clone(), vids.clone());
                vids
            }
        };
        if stage1_starts.is_empty() {
            return Ok(0.0);
        }

        let stage1 = self.run_walk_batch(flat_graph, compiled, &stage1_starts, pred_cache);
        let stage1_conditional = if stage1.sum_struct_weight > 0.0 {
            stage1.sum_pred_weight / stage1.sum_struct_weight
        } else {
            0.0
        };
        // Failure rate: fraction of walks that didn't yield a pred-passing
        // embedding.  This includes both predicate failures and pure
        // structural dead ends (which collapse `sum_struct_weight` to 0).
        let stage1_fail_rate = 1.0 - stage1_conditional;

        let trigger_stage2 =
            !compiled.start_predicates.is_empty() && stage1_fail_rate > FAIL_RATE_TRIGGER;
        if trigger_stage2 {
            crate::procedures::gcard_query::STAGE2_TRIGGERED
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let (
            final_conditional,
            final_struct_success,
            final_sum_struct,
            final_sum_pred,
            start_filter_ratio,
            final_starts_len,
            stage2_used,
            pool_empty,
        ) = if trigger_stage2 {
            // Stage 2: materialise the tangled domain for this (label,
            // predicates) pair and re-sample from it.  The pool is cached in
            // `GLOBAL_FILTERED_POOL_CACHE`, so subsequent queries that share
            // the same predicate hit O(1) here.
            let (pool, ratio) = self.filtered_start_pool(
                flat_graph,
                &compiled.start_label,
                &compiled.start_predicates,
                filtered_pool_cache,
                pred_cache,
            );
            if pool.is_empty() {
                // No vertex matches the start predicate — truly zero, no
                // amount of sampling can recover an embedding.  This is the
                // *only* path to a genuine zero selectivity.
                (0.0, 0, 0.0, 0.0, 0.0, 0, true, true)
            } else {
                let mut rng = rand::thread_rng();
                let starts = sample_without_replacement(pool.as_ref(), sample_size, &mut rng);
                let stage2 = self.run_walk_batch(flat_graph, compiled, &starts, pred_cache);
                let cond = if stage2.sum_struct_weight > 0.0 {
                    stage2.sum_pred_weight / stage2.sum_struct_weight
                } else {
                    0.0
                };
                (
                    cond,
                    stage2.struct_success_sample_count,
                    stage2.sum_struct_weight,
                    stage2.sum_pred_weight,
                    ratio,
                    starts.len(),
                    true,
                    false,
                )
            }
        } else {
            (
                stage1_conditional,
                stage1.struct_success_sample_count,
                stage1.sum_struct_weight,
                stage1.sum_pred_weight,
                1.0,
                stage1_starts.len(),
                false,
                false,
            )
        };

        // Final unconditional selectivity: conditional ratio × P(start_pred).
        // Stage 1 always uses ratio = 1.0 (no filter), so the multiplication
        // is a no-op there.
        let selectivity = final_conditional * start_filter_ratio;

        // Distinguish a *genuine* zero (the start predicate matches no vertex
        // at all → `pool_empty`) from a sampling miss (we found no
        // structurally-valid, pred-passing embedding among the samples).  Only
        // the former collapses the estimate to zero; the latter falls back to
        // the static stats-based predicate selectivity so the PCF's structural
        // cardinality survives instead of being truncated to zero.
        let result = if pool_empty {
            crate::procedures::gcard_query::SELECTIVITY_ZERO
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::procedures::gcard_query::STAGE2_RESULT_ZERO
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            0.0
        } else if selectivity == 0.0 {
            crate::procedures::gcard_query::SELECTIVITY_FALLBACK
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if stage2_used {
                crate::procedures::gcard_query::STAGE2_RESULT_ZERO
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Self::static_fallback_selectivity(flat_graph, compiled)
        } else {
            selectivity
        };

        if crate::procedures::gcard_query::GCARD_VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "[selectivity-debug] path={}, plan_idx={}, start_label={}, samples={}, struct_success={}, sum_struct={:.4}, sum_pred={:.4}, conditional={:.6}, start_filter_ratio={:.6}, sel={:.6}, result={:.6}, stage2={}, pool_empty={}",
                path_desc,
                compiled.start_idx,
                compiled.start_label,
                final_starts_len,
                final_struct_success,
                final_sum_struct,
                final_sum_pred,
                final_conditional,
                start_filter_ratio,
                selectivity,
                result,
                stage2_used,
                pool_empty,
            );
        }

        Ok(result)
    }

    /// Run the batched parallel walk loop with per-start CV adaptation and
    /// global Delta-method CI early stopping.  Returns aggregated weights so
    /// the caller can decide whether to escalate (Stage 2) or accept the
    /// estimate.
    fn run_walk_batch(
        &self,
        flat_graph: &FlatGraph,
        compiled: &FlatCompiledPathQuery<'_>,
        sampled_starts: &[VertexId],
        _pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> WalkBatchResult {
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

        // A plan is "deterministic" when no predicate can fail anywhere along
        // the walk — the start vertex carries no predicates and both segments
        // are predicate-free.  In that case every PILOT_WALKS invocation
        // returns the exact same `(struct_weight, pred_weight)` from the
        // segment short-circuit, so we only run the walk once per start and
        // skip the variance / CV machinery entirely.
        let plan_is_deterministic = compiled.start_predicates.is_empty()
            && Self::segment_is_predicate_free(&compiled.left_segment)
            && Self::segment_is_predicate_free(&compiled.right_segment);
        let walks_per_start = if plan_is_deterministic {
            1
        } else {
            PILOT_WALKS
        };

        // Tells the per-start closure which segments will compute the same
        // exact reachable count on every PILOT walk — we'll do the BFS once
        // up front and pass the result to each walk via the hint args.
        let left_is_pred_free = !compiled.left_segment.is_empty()
            && Self::segment_is_predicate_free(&compiled.left_segment);
        let right_is_pred_free = !compiled.right_segment.is_empty()
            && Self::segment_is_predicate_free(&compiled.right_segment);

        for batch in sampled_starts.chunks(BATCH_SIZE) {
            let batch_results: Vec<(Option<(f64, f64)>, WalkProf)> = batch
                .par_iter()
                .map_init(
                    || {
                        (
                            HashMap::<(usize, VertexId), (usize, usize)>::new(),
                            HashMap::<(u32, u64), bool>::new(),
                        )
                    },
                    |(nbr_cache, local_cache), &start_vid| {
                        // Rayon workers retain the allocated buckets across starts
                        // within this batch. Logical contents remain per-start.
                        nbr_cache.clear();
                        local_cache.clear();
                        let mut rng = rand::thread_rng();
                        let mut prof = WalkProf::default();

                        // One BFS per start, reused by every PILOT walk.  When the
                        // segment isn't predicate-free we leave the hint as `None`
                        // and the walk handles it normally.
                        let left_hint: Option<f64> = if left_is_pred_free {
                            Self::compute_segment_reachable_count(
                                start_vid,
                                &compiled.left_segment,
                                &mut prof,
                            )
                        } else {
                            None
                        };
                        let right_hint: Option<f64> = if right_is_pred_free {
                            Self::compute_segment_reachable_count(
                                start_vid,
                                &compiled.right_segment,
                                &mut prof,
                            )
                        } else {
                            None
                        };

                        // The start vertex is fixed across every pilot/adaptive
                        // walk for this sampled start. Evaluate its predicates
                        // once and pass the result through, instead of repeatedly
                        // probing the local predicate HashMap 5-50 times.
                        let start_pred_ok = match self.evaluate_start_vertex_predicates(
                            flat_graph,
                            start_vid,
                            &compiled.start_predicates,
                            &mut prof,
                            local_cache,
                        ) {
                            Ok(result) => result,
                            Err(_) => {
                                // Preserve the legacy accounting where the first
                                // attempted walk observes the evaluation error.
                                prof.walk_count += 1;
                                return (None, prof);
                            }
                        };

                        // At most MAX_WALKS observations are retained for the per-start
                        // CV calculation. Fixed-size stack buffers avoid two heap
                        // allocations for every sampled start while preserving the
                        // original accumulation order exactly.
                        let mut walk_struct = [0.0f64; MAX_WALKS];
                        let mut walk_pred = [0.0f64; MAX_WALKS];
                        let mut walk_len = 0usize;

                        for _ in 0..walks_per_start {
                            prof.walk_count += 1;
                            let r = self.execute_flat_walk(
                                flat_graph,
                                compiled,
                                start_vid,
                                &mut rng,
                                nbr_cache,
                                &mut prof,
                                local_cache,
                                start_pred_ok,
                                left_hint,
                                right_hint,
                            );
                            let (sw, pw) = match r {
                                Ok(v) => v,
                                Err(_) => return (None, prof),
                            };
                            walk_struct[walk_len] = sw;
                            walk_pred[walk_len] = pw;
                            walk_len += 1;
                        }

                        let pilot_mean =
                            walk_struct[..walk_len].iter().sum::<f64>() / walk_len as f64;
                        if pilot_mean == 0.0 {
                            return (None, prof);
                        }
                        if !plan_is_deterministic && pilot_mean > 0.0 {
                            let variance = walk_struct[..walk_len]
                                .iter()
                                .map(|&w| (w - pilot_mean) * (w - pilot_mean))
                                .sum::<f64>()
                                / (walk_len as f64 - 1.0).max(1.0);
                            let cv = variance.sqrt() / pilot_mean;
                            if cv > CV_THRESHOLD && cv <= CV_UPPER_BOUND {
                                for _ in 0..(MAX_WALKS - PILOT_WALKS) {
                                    prof.walk_count += 1;
                                    let r = self.execute_flat_walk(
                                        flat_graph,
                                        compiled,
                                        start_vid,
                                        &mut rng,
                                        nbr_cache,
                                        &mut prof,
                                        local_cache,
                                        start_pred_ok,
                                        left_hint,
                                        right_hint,
                                    );
                                    let (sw, pw) = match r {
                                        Ok(v) => v,
                                        Err(_) => return (None, prof),
                                    };
                                    walk_struct[walk_len] = sw;
                                    walk_pred[walk_len] = pw;
                                    walk_len += 1;
                                }
                            }
                        }

                        let total_walks = walk_len as f64;
                        let struct_weight =
                            walk_struct[..walk_len].iter().sum::<f64>() / total_walks;
                        let pred_weight = walk_pred[..walk_len].iter().sum::<f64>() / total_walks;
                        let result = if struct_weight > 0.0 {
                            Some((struct_weight, pred_weight))
                        } else {
                            None
                        };
                        (result, prof)
                    },
                )
                .collect();

            total_vertices_visited += batch_results.len();
            // Publish profiling counters once per batch rather than once per
            // sampled start. This removes three contended atomic writes from
            // every Rayon result without changing the accumulated values.
            let batch_walk_count = batch_results.iter().map(|(_, prof)| prof.walk_count).sum();
            let batch_nbr_nanos = batch_results.iter().map(|(_, prof)| prof.nbr_nanos).sum();
            let batch_prop_nanos = batch_results.iter().map(|(_, prof)| prof.prop_nanos).sum();
            crate::procedures::gcard_query::SAMPLING_TOTAL_WALKS
                .fetch_add(batch_walk_count, std::sync::atomic::Ordering::Relaxed);
            crate::procedures::gcard_query::SAMPLING_NBR_NANOS
                .fetch_add(batch_nbr_nanos, std::sync::atomic::Ordering::Relaxed);
            crate::procedures::gcard_query::SAMPLING_PROP_NANOS
                .fetch_add(batch_prop_nanos, std::sync::atomic::Ordering::Relaxed);

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

        WalkBatchResult {
            sum_struct_weight,
            sum_pred_weight,
            struct_success_sample_count,
        }
    }

    /// Estimate the joint selectivity of `predicates` against `label` using
    /// the FlatGraph's column statistics only — *no* materialisation, *no*
    /// per-vertex scan.  Returns a value in `(0, 1]`.
    ///
    /// We assume column-independence (the standard textbook simplification)
    /// and use per-column NDV + min/max from `ColumnStats`:
    ///   - `Eq`:  `1 / ndv`
    ///   - `Ne`:  `1 - 1 / ndv`
    ///   - `Lt`/`Le`/`Gt`/`Ge`:  linear interpolation across `[min, max]`
    ///   - boolean `Eq`: `1 / 2` fallback if more accurate stats are absent.
    ///
    /// This drives plan ranking cheaply, so we never need to actually scan a
    /// label pool just to decide which plan looks best.  When stats are
    /// missing for a column the estimate falls back to `0.5` for that
    /// predicate (uninformative but at least not extreme).
    fn estimate_predicate_selectivity_stats(
        flat_graph: &FlatGraph,
        label: &str,
        predicates: &[ResolvedPredicate],
    ) -> f64 {
        Self::stats_predicate_selectivity(flat_graph, label, false, predicates)
    }

    /// Joint stats-based selectivity of `predicates` against a single
    /// vertex/edge `label`, using the FlatGraph's ColumnStats only (no scan).
    /// `is_edge` selects between vertex and edge stat tables.  Returns a value
    /// in `(0, 1]`; missing stats degrade gracefully to `1.0`/`0.5`.
    fn stats_predicate_selectivity(
        flat_graph: &FlatGraph,
        label: &str,
        is_edge: bool,
        predicates: &[ResolvedPredicate],
    ) -> f64 {
        if predicates.is_empty() {
            return 1.0;
        }
        let table_stats = if is_edge {
            flat_graph.edge_table_stats(label)
        } else {
            flat_graph.vertex_table_stats(label)
        };
        let Some(table_stats) = table_stats else {
            return 1.0;
        };
        let schema = if is_edge {
            flat_graph.edge_prop_schema()
        } else {
            flat_graph.vertex_prop_schema()
        };
        let Some(prop_names) = schema.get(label) else {
            return 1.0;
        };

        let mut joint: f64 = 1.0;
        for rp in predicates {
            let Some(prop_name) = prop_names.get(rp.prop_index) else {
                continue;
            };
            let Some(col) = table_stats.columns.get(prop_name) else {
                joint *= 0.5;
                continue;
            };
            joint *= Self::estimate_single_predicate_stats(col, &rp.op, &rp.value);
        }
        joint.clamp(1e-12, 1.0)
    }

    /// Static, sampling-free estimate of the *whole path*'s predicate
    /// selectivity for the chosen plan — the product of every predicated
    /// position's stats selectivity (start vertex + both segments, vertices via
    /// vertex stats, edges via edge stats), assuming predicate independence.
    ///
    /// This is the fallback returned when sampling fails to find any
    /// structurally-valid embedding: the structural cardinality is already
    /// carried exactly by the PCF, so we only need to supply the predicate
    /// multiplier analytically rather than collapsing the estimate to zero.
    fn static_fallback_selectivity(
        flat_graph: &FlatGraph,
        compiled: &FlatCompiledPathQuery<'_>,
    ) -> f64 {
        let mut sel = Self::stats_predicate_selectivity(
            flat_graph,
            &compiled.start_label,
            false,
            &compiled.start_predicates,
        );
        for segment in [&compiled.left_segment, &compiled.right_segment] {
            for step in segment {
                match step {
                    FlatCompiledStep::Vertex { label, predicates } if !predicates.is_empty() => {
                        sel *=
                            Self::stats_predicate_selectivity(flat_graph, label, false, predicates);
                    }
                    FlatCompiledStep::Edge {
                        edge_label,
                        predicates,
                        ..
                    } if !predicates.is_empty() => {
                        sel *= Self::stats_predicate_selectivity(
                            flat_graph, edge_label, true, predicates,
                        );
                    }
                    _ => {}
                }
            }
        }
        sel.clamp(1e-12, 1.0)
    }

    /// Estimate the *structural participation fraction* of anchoring the walk at
    /// vertex index `anchor` on the path described by `node_labels` /
    /// `edge_labels` — i.e. the probability that a uniformly sampled vertex of
    /// the anchor's label has at least one full structural embedding of the
    /// path.  A high participation means uniform start sampling lands on
    /// participating vertices and rarely dead-ends.
    ///
    /// * Endpoint anchors (`anchor == 0` or `anchor == k`) read the exact support of the full-path
    ///   PCF from that endpoint — one catalog lookup, no independence assumption.
    /// * Interior anchors combine the two half-path PCFs (left half and right half emanating from
    ///   the anchor) under an independence approximation.
    ///
    /// Returns `None` when the catalog has no statistics for the relevant
    /// pattern (so the caller treats participation as unknown rather than zero).
    fn anchor_structural_participation(
        degree_seq_graph: &DegreeSeqGraphCompressed,
        flat_graph: &FlatGraph,
        node_labels: &[String],
        edge_labels: &[String],
        anchor: usize,
    ) -> Option<f64> {
        let k = edge_labels.len();
        let anchor_label = node_labels.get(anchor)?;
        let pop = flat_graph.vertex_count_by_label(anchor_label);
        if pop == 0 {
            return None;
        }
        let pop = pop as f64;

        let support_of = |node_seq: &[String], edge_seq: &[String], target: &str| -> Option<f64> {
            let key = make_alt_key(node_seq, edge_seq);
            let pcf = degree_seq_graph.get_piece_func_by_path(&key, target);
            if pcf.is_empty_placeholder() {
                None
            } else {
                Some(pcf.support())
            }
        };

        if anchor == 0 || anchor == k {
            let s = support_of(node_labels, edge_labels, anchor_label)?;
            Some((s / pop).clamp(0.0, 1.0))
        } else {
            let left_support = support_of(
                &node_labels[0..=anchor],
                &edge_labels[0..anchor],
                anchor_label,
            )?;
            let right_support = support_of(
                &node_labels[anchor..=k],
                &edge_labels[anchor..k],
                anchor_label,
            )?;
            Some(((left_support / pop) * (right_support / pop)).clamp(0.0, 1.0))
        }
    }

    fn estimate_single_predicate_stats(
        col: &crate::procedures::gcard_query::flat_graph::stats::ColumnStats,
        op: &ComparisonOp,
        value: &ScalarValue,
    ) -> f64 {
        let ndv = col.ndv() as f64;
        match op {
            ComparisonOp::Eq => {
                if ndv >= 1.0 {
                    (1.0 / ndv).max(1e-12)
                } else {
                    0.5
                }
            }
            ComparisonOp::Ne => {
                if ndv >= 1.0 {
                    (1.0 - 1.0 / ndv).max(1e-12)
                } else {
                    0.5
                }
            }
            ComparisonOp::Lt | ComparisonOp::Le | ComparisonOp::Gt | ComparisonOp::Ge => {
                Self::estimate_range_predicate_stats(col, op, value).unwrap_or(0.5)
            }
        }
    }

    fn estimate_range_predicate_stats(
        col: &crate::procedures::gcard_query::flat_graph::stats::ColumnStats,
        op: &ComparisonOp,
        value: &ScalarValue,
    ) -> Option<f64> {
        let v = Self::scalar_to_f64(value)?;
        let min = Self::scalar_to_f64(col.min.as_ref()?)?;
        let max = Self::scalar_to_f64(col.max.as_ref()?)?;
        if max <= min {
            return Some(0.5);
        }
        let position = ((v - min) / (max - min)).clamp(0.0, 1.0);
        Some(match op {
            ComparisonOp::Lt | ComparisonOp::Le => position,
            ComparisonOp::Gt | ComparisonOp::Ge => 1.0 - position,
            _ => 0.5,
        })
    }

    fn scalar_to_f64(v: &ScalarValue) -> Option<f64> {
        use ScalarValue::*;
        match v {
            Int8(Some(x)) => Some(*x as f64),
            Int16(Some(x)) => Some(*x as f64),
            Int32(Some(x)) => Some(*x as f64),
            Int64(Some(x)) => Some(*x as f64),
            UInt8(Some(x)) => Some(*x as f64),
            UInt16(Some(x)) => Some(*x as f64),
            UInt32(Some(x)) => Some(*x as f64),
            UInt64(Some(x)) => Some(*x as f64),
            Float32(Some(x)) => Some(x.into_inner() as f64),
            Float64(Some(x)) => Some(x.into_inner()),
            Boolean(Some(b)) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Return the cached (or freshly computed) filtered start pool — vertices
    /// of `label` that pass every predicate in `predicates`.  Returns the full
    /// filtered list (so callers can sample from it) along with the filter
    /// ratio `|filtered| / |total|` used to debias the final selectivity.
    ///
    /// The pool is cached by `(label, sorted_predicate_ids)`.  Predicates
    /// without a `predicate_id` can still be evaluated, but the pool can only
    /// be cached when every predicate has an id, otherwise we'd risk hits
    /// across queries that share an id but differ in value.
    fn filtered_start_pool(
        &self,
        flat_graph: &FlatGraph,
        label: &str,
        predicates: &[ResolvedPredicate],
        filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> (Arc<Vec<VertexId>>, f64) {
        let total = flat_graph.vertex_count_by_label(label);
        if total == 0 {
            return (Arc::new(Vec::new()), 0.0);
        }

        // Cache key encodes the full predicate semantics (prop_index, op,
        // value) so the filtered pool can be safely reused across queries
        // that share the same predicate, regardless of any per-query
        // `predicate_id` numbering.
        let cache_key = if !predicates.is_empty() {
            let mut spec: Vec<(usize, ComparisonOp, ScalarValue)> = predicates
                .iter()
                .map(|p| (p.prop_index, p.op, p.value.clone()))
                .collect();
            spec.sort_by(|a, b| a.0.cmp(&b.0));
            Some((label.to_string(), spec))
        } else {
            None
        };

        if let Some(ref k) = cache_key {
            if let Some(cached) = filtered_pool_cache.get(k) {
                crate::procedures::gcard_query::FILTER_POOL_CACHE_HITS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let pool = cached.clone();
                let ratio = (pool.len() as f64) / (total as f64);
                if pool.is_empty() {
                    crate::procedures::gcard_query::FILTER_POOL_EMPTY
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return (pool, ratio);
            }
        }

        crate::procedures::gcard_query::FILTER_POOL_CACHE_MISSES
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let all_vids = flat_graph.all_vertex_ids_by_label(label);
        // Parallel filter scan over the whole label pool.  Predicate
        // evaluation is done without touching `pred_cache` — DashMap writes
        // serialise across rayon workers and dominate the scan time.
        // Subsequent walks may re-evaluate predicates on the same vertices,
        // but that cost is small compared with the lock contention here.
        let _ = pred_cache; // intentionally unused in the scan body
        let filtered: Vec<VertexId> = all_vids
            .par_iter()
            .filter(|&&vid| self.vertex_passes_predicates_uncached(flat_graph, vid, predicates))
            .copied()
            .collect();
        let pool = Arc::new(filtered);
        let ratio = (pool.len() as f64) / (total as f64);

        if pool.is_empty() {
            crate::procedures::gcard_query::FILTER_POOL_EMPTY
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        if let Some(k) = cache_key {
            filtered_pool_cache.insert(k, pool.clone());
        }

        (pool, ratio)
    }

    /// Cache-free predicate evaluation used inside the parallel filter scan.
    /// Reads `flat_graph.vertex_props` and applies predicates directly with
    /// no DashMap traffic, so rayon workers don't contend on shared locks.
    fn vertex_passes_predicates_uncached(
        &self,
        flat_graph: &FlatGraph,
        vid: VertexId,
        predicates: &[ResolvedPredicate],
    ) -> bool {
        if predicates.is_empty() {
            return true;
        }
        let Some(props) = flat_graph.vertex_props(vid) else {
            return false;
        };
        for rp in predicates {
            let pass = match props.get(rp.prop_index) {
                Some(v) => self.compare_values(v, &rp.op, &rp.value).unwrap_or(false),
                None => false,
            };
            if !pass {
                return false;
            }
        }
        true
    }

    /// Evaluate every vertex predicate against `vid`, using `pred_cache` to
    /// memoise per-vertex predicate decisions.  Returns true only if all
    /// predicates pass.
    fn vertex_passes_all_predicates(
        &self,
        flat_graph: &FlatGraph,
        vid: VertexId,
        predicates: &[ResolvedPredicate],
        pred_cache: &Arc<DashMap<(u32, u64), bool>>,
    ) -> bool {
        if predicates.is_empty() {
            return true;
        }
        // Fast path: every predicate is cached.
        let mut need_props = false;
        for rp in predicates {
            if let Some(pid) = rp.predicate_id {
                match pred_cache.get(&(pid, vid)) {
                    Some(cached) => {
                        if !*cached {
                            return false;
                        }
                    }
                    None => {
                        need_props = true;
                        break;
                    }
                }
            } else {
                need_props = true;
                break;
            }
        }
        if !need_props {
            return true;
        }

        let Some(props) = flat_graph.vertex_props(vid) else {
            return false;
        };
        for rp in predicates {
            if let Some(pid) = rp.predicate_id {
                if let Some(cached) = pred_cache.get(&(pid, vid)) {
                    if !*cached {
                        return false;
                    }
                    continue;
                }
            }
            let pass = match props.get(rp.prop_index) {
                Some(v) => match self.compare_values(v, &rp.op, &rp.value) {
                    Ok(b) => b,
                    Err(_) => false,
                },
                None => false,
            };
            if let Some(pid) = rp.predicate_id {
                pred_cache.insert((pid, vid), pass);
            }
            if !pass {
                return false;
            }
        }
        true
    }

    /// Pilot-evaluate every candidate walk plan and pick the one with the
    /// lowest Delta-method SE² × pilot lookup cost.  Falls back to plan 0 if
    /// no plan produces enough successful samples in the pilot.
    /// Pick the best walk plan using *statistics only* — no pilot walks, no
    /// label scan.  For each plan we estimate the start vertex's predicate
    /// selectivity from `ColumnStats` and pick the plan whose start vertex is
    /// most selective (smallest estimated ratio).  Ties go to the plan with
    /// the smaller start population (cheaper to materialise later if the
    /// walk-fail-then-calculate stage 2 ever triggers).
    /// Pick the single walk plan to sample, considering **both** predicate
    /// selectivity (as before) **and** structural participation (new).
    ///
    /// Structure gates, selectivity refines: an anchor is "viable" when uniform
    /// start sampling is expected to land on enough participating vertices
    /// (`sample_size * participation >= VIABLE_MIN_HITS`).  Among viable anchors
    /// the most predicate-selective start wins (smallest filter pool, cleanest
    /// conditional estimate); if none is viable we pick the best structural
    /// chance so the caller's fallback path runs from the least-bad anchor.
    ///
    /// Returns `(chosen_idx, participation_of_chosen)`.  The participation is
    /// `None` when the catalog has no stats for that anchor's pattern.
    #[allow(clippy::too_many_arguments)]
    fn select_best_walk_plan(
        &self,
        flat_graph: &FlatGraph,
        degree_seq_graph: &DegreeSeqGraphCompressed,
        plans: &[FlatCompiledPathQuery<'_>],
        node_labels: &[String],
        edge_labels: &[String],
        sample_size: usize,
        _pred_cache: &Arc<DashMap<(u32, u64), bool>>,
        _filtered_pool_cache: &Arc<
            DashMap<(String, Vec<(usize, ComparisonOp, ScalarValue)>), Arc<Vec<VertexId>>>,
        >,
    ) -> (usize, Option<f64>) {
        // Minimum expected number of structurally-successful starts for an
        // anchor to count as "viable" — above this, uniform sampling lands on
        // enough participating vertices to yield a real conditional estimate.
        const VIABLE_MIN_HITS: f64 = 5.0;

        struct Cand {
            idx: usize,
            participation: Option<f64>,
            viable: bool,
            ratio: f64,
            pop: usize,
        }

        let is_better = |a: &Cand, b: &Cand| -> bool {
            // Viable anchors always beat non-viable ones.
            if a.viable != b.viable {
                return a.viable;
            }
            let pa = a.participation.unwrap_or(0.0);
            let pb = b.participation.unwrap_or(0.0);
            if a.viable {
                // Both viable: most selective start first, then highest
                // participation, then smallest population.
                if a.ratio != b.ratio {
                    return a.ratio < b.ratio;
                }
                if pa != pb {
                    return pa > pb;
                }
                a.pop < b.pop
            } else {
                // Both non-viable: best structural chance first, then most
                // selective start, then smallest population.
                if pa != pb {
                    return pa > pb;
                }
                if a.ratio != b.ratio {
                    return a.ratio < b.ratio;
                }
                a.pop < b.pop
            }
        };

        let n = sample_size as f64;
        let mut best: Option<Cand> = None;

        for (idx, plan) in plans.iter().enumerate() {
            let ratio = if plan.start_predicates.is_empty() {
                1.0
            } else {
                Self::estimate_predicate_selectivity_stats(
                    flat_graph,
                    &plan.start_label,
                    &plan.start_predicates,
                )
            };
            let pop = flat_graph.vertex_count_by_label(&plan.start_label);
            let participation = Self::anchor_structural_participation(
                degree_seq_graph,
                flat_graph,
                node_labels,
                edge_labels,
                plan.start_idx,
            );
            // Unknown participation is treated as neutral/viable so anchors
            // without catalog stats fall back to the legacy selectivity ranking
            // instead of being penalised to the bottom.
            let viable = match participation {
                Some(p) => n * p >= VIABLE_MIN_HITS,
                None => true,
            };

            let cand = Cand {
                idx,
                participation,
                viable,
                ratio,
                pop,
            };
            if best.as_ref().is_none_or(|b| is_better(&cand, b)) {
                best = Some(cand);
            }
        }

        let best = best.expect("plans is non-empty");
        if crate::procedures::gcard_query::GCARD_VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "[plan-select] plans={}, chosen={}, viable={}, participation={:?}, est_ratio={:.6e}, pop={}, start_label={}",
                plans.len(),
                best.idx,
                best.viable,
                best.participation,
                best.ratio,
                best.pop,
                plans[best.idx].start_label,
            );
        }

        (best.idx, best.participation)
    }

    /// Execute a single Wander Join walk over [`FlatGraph`] using the given
    /// compiled plan. The caller evaluates the start vertex predicates once,
    /// then the walker traverses the left and right segments independently
    /// from the same starting point.
    ///
    /// Returns `(struct_weight, pred_weight)` — the structural weight and the
    /// predicate-filtered weight for this walk.  `(0.0, 0.0)` means the walk
    /// failed (structural dead end somewhere along the path).
    #[allow(clippy::too_many_arguments)]
    fn execute_flat_walk(
        &self,
        flat_graph: &FlatGraph,
        compiled: &FlatCompiledPathQuery<'_>,
        start_vertex: VertexId,
        rng: &mut impl rand::Rng,
        nbr_cache: &mut HashMap<(usize, VertexId), (usize, usize)>,
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
        start_pred_ok: Option<bool>,
        // Pre-computed reachable counts for the predicate-free segments of
        // this plan.  `Some(c)` means "the BFS short-circuit returned `c`,
        // reuse it for free"; `None` means "compute it (or fall back to a
        // walk) like normal".  Set by `run_walk_batch` once per start so the
        // BFS doesn't get repeated across PILOT_WALKS for the same start.
        left_count_hint: Option<f64>,
        right_count_hint: Option<f64>,
    ) -> GCardResult<(f64, f64)> {
        let Some(start_pred_ok) = start_pred_ok else {
            return Ok((0.0, 0.0));
        };

        // Single-vertex query (no edges): only the start vertex matters.
        if compiled.left_segment.is_empty() && compiled.right_segment.is_empty() {
            return Ok((1.0, if start_pred_ok { 1.0 } else { 0.0 }));
        }

        let start_factor = if start_pred_ok { 1.0 } else { 0.0 };

        let mut struct_weight: f64 = 1.0;
        let mut pred_weight: f64 = 1.0;

        if !compiled.left_segment.is_empty() {
            let (ls, lp) = if let Some(c) = left_count_hint {
                (c, c)
            } else if Self::segment_is_predicate_free(&compiled.left_segment) {
                match Self::compute_segment_reachable_count(
                    start_vertex,
                    &compiled.left_segment,
                    prof,
                ) {
                    Some(c) => (c, c),
                    None => self.walk_segment_branched(
                        flat_graph,
                        &compiled.left_segment,
                        0,
                        start_vertex,
                        rng,
                        nbr_cache,
                        prof,
                        local_pred_cache,
                    )?,
                }
            } else {
                self.walk_segment_branched(
                    flat_graph,
                    &compiled.left_segment,
                    0,
                    start_vertex,
                    rng,
                    nbr_cache,
                    prof,
                    local_pred_cache,
                )?
            };
            if ls == 0.0 {
                return Ok((0.0, 0.0));
            }
            struct_weight *= ls;
            pred_weight *= lp;
        }

        if !compiled.right_segment.is_empty() {
            let (rs, rp) = if let Some(c) = right_count_hint {
                (c, c)
            } else if Self::segment_is_predicate_free(&compiled.right_segment) {
                match Self::compute_segment_reachable_count(
                    start_vertex,
                    &compiled.right_segment,
                    prof,
                ) {
                    Some(c) => (c, c),
                    None => self.walk_segment_branched(
                        flat_graph,
                        &compiled.right_segment,
                        0,
                        start_vertex,
                        rng,
                        nbr_cache,
                        prof,
                        local_pred_cache,
                    )?,
                }
            } else {
                self.walk_segment_branched(
                    flat_graph,
                    &compiled.right_segment,
                    0,
                    start_vertex,
                    rng,
                    nbr_cache,
                    prof,
                    local_pred_cache,
                )?
            };
            if rs == 0.0 {
                return Ok((0.0, 0.0));
            }
            struct_weight *= rs;
            pred_weight *= rp;
        }

        pred_weight *= start_factor;
        Ok((struct_weight, pred_weight))
    }

    /// Evaluate start vertex predicates.
    ///
    /// Returns `Some(true)` if predicates pass (or are empty), `Some(false)` if
    /// any predicate fails, and `None` to signal a structural dead end
    /// (preserves the legacy behaviour where missing vertex props at a
    /// predicated step discards the walk entirely).
    fn evaluate_start_vertex_predicates(
        &self,
        flat_graph: &FlatGraph,
        vid: VertexId,
        predicates: &[ResolvedPredicate],
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<Option<bool>> {
        if predicates.is_empty() {
            return Ok(Some(true));
        }
        let t0 = std::time::Instant::now();
        let props = flat_graph.vertex_props(vid);
        prof.prop_nanos += t0.elapsed().as_nanos() as u64;
        let Some(props) = props else {
            return Ok(None);
        };
        for rp in predicates {
            let pass = if let Some(pid) = rp.predicate_id {
                if let Some(&cached) = local_pred_cache.get(&(pid, vid)) {
                    cached
                } else {
                    let p = match props.get(rp.prop_index) {
                        Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                        None => false,
                    };
                    local_pred_cache.insert((pid, vid), p);
                    p
                }
            } else {
                match props.get(rp.prop_index) {
                    Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                    None => false,
                }
            };
            if !pass {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    /// Walk one segment of a compiled plan, starting from `start_vertex` whose
    /// label is `start_label`.  Returns `(weight, pred_ok)` where `weight` is
    /// the product of degrees along the segment, or `(0.0, false)` on a
    /// structural dead end.
    #[allow(clippy::too_many_arguments)]
    /// `true` if every step in `segment` carries no predicates (both vertex
    /// and edge sides).  When that holds for an entire segment the walk
    /// contributes only structural weight — every sampled path passes the
    /// (empty) predicate set trivially — so we can replace the random walk
    /// with a deterministic reachable-count enumeration and get zero
    /// estimation variance for that segment "for free".
    fn segment_is_predicate_free(segment: &[FlatCompiledStep<'_>]) -> bool {
        segment.iter().all(|step| match step {
            FlatCompiledStep::Vertex { predicates, .. } => predicates.is_empty(),
            FlatCompiledStep::Edge { predicates, .. } => predicates.is_empty(),
        })
    }

    /// Compute the *exact* number of structural paths from `start_vertex`
    /// through a predicate-free `segment`.  Iterative BFS over the segment
    /// hops; the last edge only sums fanouts (no need to materialise its
    /// endpoint frontier).  Returns `None` when an intermediate frontier
    /// would exceed `MAX_FRONTIER` — in that case the caller should fall
    /// back to a random walk to keep memory bounded.
    fn compute_segment_reachable_count(
        start_vertex: VertexId,
        segment: &[FlatCompiledStep<'_>],
        prof: &mut WalkProf,
    ) -> Option<f64> {
        const MAX_FRONTIER: usize = 200_000;

        let num_edges = segment
            .iter()
            .filter(|s| matches!(s, FlatCompiledStep::Edge { .. }))
            .count();
        if num_edges == 0 {
            return Some(1.0);
        }

        let mut frontier: Vec<VertexId> = vec![start_vertex];
        let mut edge_idx: usize = 0;

        for step in segment {
            match step {
                FlatCompiledStep::Vertex { .. } => {}
                FlatCompiledStep::Edge { csr, .. } => {
                    let is_last = edge_idx == num_edges - 1;
                    let csr = *csr;

                    let t0 = std::time::Instant::now();
                    if is_last {
                        let total: usize = match csr {
                            Some(csr) => frontier
                                .iter()
                                .map(|&vid| csr.neighbors_slice(vid).len())
                                .sum(),
                            None => 0,
                        };
                        prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                        return Some(total as f64);
                    }

                    let mut next: Vec<VertexId> = Vec::with_capacity(frontier.len() * 4);
                    if let Some(csr) = csr {
                        for &vid in &frontier {
                            for &(nbr, _eid) in csr.neighbors_slice(vid) {
                                next.push(nbr);
                                if next.len() > MAX_FRONTIER {
                                    prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                                    return None;
                                }
                            }
                        }
                    }
                    prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                    frontier = next;
                    edge_idx += 1;
                }
            }
        }

        // Defensive: segment didn't end with an edge step.
        Some(frontier.len() as f64)
    }

    /// Recursive walk over one segment with Alley-style branching.
    ///
    /// At each `Edge` step we sample `⌈b · degree⌉` neighbors without
    /// replacement and recurse on each.  Returns `(struct_weight,
    /// pred_weight)` — both unbiased HT estimates of the expected total
    /// structural / predicate-passing weight from the current position to
    /// the end of the segment.  With `b ≈ 1` the walk degenerates to an
    /// exhaustive enumeration; with very small `b` and degree==1 it
    /// degenerates back to Wander Join's single-pick.
    ///
    /// `pred_weight` is `struct_weight × P(all remaining predicates pass)`
    /// when there's no branching.  With branching it can take fractional
    /// values, since different branches may pass or fail independently.
    #[allow(clippy::too_many_arguments)]
    fn walk_segment_branched(
        &self,
        flat_graph: &FlatGraph,
        segment: &[FlatCompiledStep<'_>],
        depth: usize,
        current_vid: VertexId,
        rng: &mut impl rand::Rng,
        nbr_cache: &mut HashMap<(usize, VertexId), (usize, usize)>,
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<(f64, f64)> {
        // Branching factor (Alley default 1/32: sample ⌈n/32⌉ neighbors at
        // each step, with a floor of 1).  For small degrees this matches
        // Wander Join's single-pick; only high-fanout vertices actually fan
        // out into multiple branches.
        const BRANCH_FACTOR_B: f64 = 1.0 / 32.0;

        if depth >= segment.len() {
            return Ok((1.0, 1.0));
        }

        match &segment[depth] {
            FlatCompiledStep::Vertex { predicates, .. } => {
                let pred_pass = if predicates.is_empty() {
                    true
                } else {
                    match self.evaluate_step_vertex_predicates(
                        flat_graph,
                        current_vid,
                        predicates,
                        prof,
                        local_pred_cache,
                    )? {
                        Some(b) => b,
                        None => return Ok((0.0, 0.0)),
                    }
                };
                let pred_factor = if pred_pass { 1.0 } else { 0.0 };
                let (s, p) = self.walk_segment_branched(
                    flat_graph,
                    segment,
                    depth + 1,
                    current_vid,
                    rng,
                    nbr_cache,
                    prof,
                    local_pred_cache,
                )?;
                Ok((s, p * pred_factor))
            }
            FlatCompiledStep::Edge {
                cache_slot,
                csr,
                predicates,
                ..
            } => {
                let Some(csr) = *csr else {
                    return Ok((0.0, 0.0));
                };
                let cache_key = (*cache_slot, current_vid);

                let bounds = match nbr_cache.entry(cache_key) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let t0 = std::time::Instant::now();
                        let bounds = csr.neighbor_bounds(current_vid);
                        prof.nbr_nanos += t0.elapsed().as_nanos() as u64;
                        *entry.insert(bounds)
                    }
                };
                let nbrs = csr.neighbors_slice_by_bounds(bounds);
                let degree = nbrs.len();
                if degree == 0 {
                    return Ok((0.0, 0.0));
                }

                let k = ((BRANCH_FACTOR_B * degree as f64).ceil() as usize).clamp(1, degree);

                let mut sum_s = 0.0;
                let mut sum_p = 0.0;

                if k == 1 {
                    // This is the common case for degree <= 32. Sampling one
                    // index directly avoids allocating an index or tuple Vec.
                    let (chosen_nbr, chosen_eid) = nbrs[rng.gen_range(0..degree)];
                    (sum_s, sum_p) = self.walk_sampled_branch(
                        flat_graph,
                        segment,
                        depth + 1,
                        chosen_nbr,
                        chosen_eid,
                        predicates,
                        rng,
                        nbr_cache,
                        prof,
                        local_pred_cache,
                    )?;
                } else if k == degree {
                    // Exhaustive branching needs no sampling buffer at all.
                    for &(chosen_nbr, chosen_eid) in nbrs {
                        let (s, p) = self.walk_sampled_branch(
                            flat_graph,
                            segment,
                            depth + 1,
                            chosen_nbr,
                            chosen_eid,
                            predicates,
                            rng,
                            nbr_cache,
                            prof,
                            local_pred_cache,
                        )?;
                        sum_s += s;
                        sum_p += p;
                    }
                } else {
                    // For a true subset, retain only O(k) compact indices;
                    // neighbor tuples remain borrowed from the immutable CSR.
                    for idx in rand::seq::index::sample(rng, degree, k) {
                        let (chosen_nbr, chosen_eid) = nbrs[idx];
                        let (s, p) = self.walk_sampled_branch(
                            flat_graph,
                            segment,
                            depth + 1,
                            chosen_nbr,
                            chosen_eid,
                            predicates,
                            rng,
                            nbr_cache,
                            prof,
                            local_pred_cache,
                        )?;
                        sum_s += s;
                        sum_p += p;
                    }
                }

                let avg_s = sum_s / k as f64;
                let avg_p = sum_p / k as f64;
                Ok((degree as f64 * avg_s, degree as f64 * avg_p))
            }
        }
    }

    /// Evaluate the edge predicate for one selected branch and recurse into
    /// the remainder of the compiled segment.
    #[allow(clippy::too_many_arguments)]
    fn walk_sampled_branch(
        &self,
        flat_graph: &FlatGraph,
        segment: &[FlatCompiledStep<'_>],
        next_depth: usize,
        chosen_nbr: VertexId,
        chosen_eid: EdgeId,
        predicates: &[ResolvedPredicate],
        rng: &mut impl rand::Rng,
        nbr_cache: &mut HashMap<(usize, VertexId), (usize, usize)>,
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<(f64, f64)> {
        let edge_pass = if predicates.is_empty() {
            true
        } else {
            self.evaluate_step_edge_predicates(
                flat_graph,
                chosen_eid,
                predicates,
                prof,
                local_pred_cache,
            )?
            .unwrap_or_default()
        };
        let edge_factor = if edge_pass { 1.0 } else { 0.0 };

        let (struct_weight, pred_weight) = self.walk_segment_branched(
            flat_graph,
            segment,
            next_depth,
            chosen_nbr,
            rng,
            nbr_cache,
            prof,
            local_pred_cache,
        )?;
        Ok((struct_weight, pred_weight * edge_factor))
    }

    /// Evaluate `predicates` against a vertex's properties.  Returns
    /// `Some(true)` if every predicate passes, `Some(false)` if any fails,
    /// and `None` to signal a structural dead end (missing properties for a
    /// predicated step).
    fn evaluate_step_vertex_predicates(
        &self,
        flat_graph: &FlatGraph,
        vid: VertexId,
        predicates: &[ResolvedPredicate],
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<Option<bool>> {
        let t0 = std::time::Instant::now();
        let props = flat_graph.vertex_props(vid);
        prof.prop_nanos += t0.elapsed().as_nanos() as u64;
        let Some(props) = props else {
            return Ok(None);
        };
        for rp in predicates {
            let pass = if let Some(pid) = rp.predicate_id {
                if let Some(&cached) = local_pred_cache.get(&(pid, vid)) {
                    cached
                } else {
                    let p = match props.get(rp.prop_index) {
                        Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                        None => false,
                    };
                    local_pred_cache.insert((pid, vid), p);
                    p
                }
            } else {
                match props.get(rp.prop_index) {
                    Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                    None => false,
                }
            };
            if !pass {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    /// Evaluate `predicates` against an edge's properties.  Same convention
    /// as `evaluate_step_vertex_predicates`.
    fn evaluate_step_edge_predicates(
        &self,
        flat_graph: &FlatGraph,
        eid: EdgeId,
        predicates: &[ResolvedPredicate],
        prof: &mut WalkProf,
        local_pred_cache: &mut HashMap<(u32, u64), bool>,
    ) -> GCardResult<Option<bool>> {
        let t0 = std::time::Instant::now();
        let eprops = flat_graph.edge_props(eid);
        prof.prop_nanos += t0.elapsed().as_nanos() as u64;
        let Some(eprops) = eprops else {
            return Ok(None);
        };
        for rp in predicates {
            let pass = if let Some(pid) = rp.predicate_id {
                if let Some(&cached) = local_pred_cache.get(&(pid, eid)) {
                    cached
                } else {
                    let p = match eprops.get(rp.prop_index) {
                        Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                        None => false,
                    };
                    local_pred_cache.insert((pid, eid), p);
                    p
                }
            } else {
                match eprops.get(rp.prop_index) {
                    Some(v) => self.compare_values(v, &rp.op, &rp.value)?,
                    None => false,
                }
            };
            if !pass {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }
}
