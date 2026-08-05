use std::sync::Arc;
use std::time::Instant;
use std::{fs, io};

use minigu_common::data_chunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;
use rand::{Rng, SeedableRng};

use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::flat_graph::FlatGraph;
use crate::procedures::gcard_query::predicate::ResolvedPredicate;
use crate::procedures::gcard_query::query_graph::QueryGraph;
use crate::procedures::gcard_query::types::{ComparisonOp, PredicateDef, Query};

#[derive(Clone)]
enum WanderJoinNodeKind {
    Vertex {
        query_vertex_id: VertexId,
        label: String,
    },
    Edge {
        label: String,
        src_query_vertex_id: VertexId,
        dst_query_vertex_id: VertexId,
        src_label: String,
        dst_label: String,
    },
}

#[derive(Clone)]
struct WanderJoinNode {
    kind: WanderJoinNodeKind,
    predicates: Vec<ResolvedPredicate>,
}

#[derive(Clone, Copy)]
struct WanderJoinJoin {
    from_node: usize,
    from_col: usize,
    to_node: usize,
    to_col: usize,
}

#[derive(Clone, Copy)]
struct WanderJoinPlanStep {
    node_idx: usize,
    counterpart_node_idx: usize,
    counterpart_col: usize,
    node_col: usize,
}

#[derive(Clone)]
struct WanderJoinPlan {
    start_node_idx: usize,
    steps: Vec<WanderJoinPlanStep>,
}

#[derive(Clone)]
enum WanderJoinBinding {
    Vertex(VertexId),
    Edge {
        src: VertexId,
        dst: VertexId,
        edge_id: EdgeId,
    },
}

impl WanderJoinBinding {
    fn value_at(&self, col: usize) -> Option<VertexId> {
        match self {
            Self::Vertex(vertex_id) => (col == 0).then_some(*vertex_id),
            Self::Edge { src, dst, .. } => match col {
                0 => Some(*src),
                1 => Some(*dst),
                _ => None,
            },
        }
    }
}

pub(crate) fn gcare_wander_join_sample_size(
    query_graph: &QueryGraph,
    flat_graph: &FlatGraph,
    sample_ratio: f64,
) -> usize {
    if sample_ratio <= 0.0 {
        return 0;
    }

    let vertex_term: usize = query_graph
        .inner
        .vertices
        .values()
        .map(|vertex| flat_graph.vertex_count_by_label(&vertex.label))
        .sum();
    let edge_term: usize = query_graph
        .inner
        .edges
        .values()
        .map(|edge| flat_graph.edge_count_by_label(&edge.label))
        .sum();
    let walk_size = query_graph.inner.vertices.len() + query_graph.inner.edges.len();
    if walk_size == 0 {
        return 0;
    }

    let avg = (vertex_term + edge_term) / walk_size;
    let scaled = (avg as f64 * sample_ratio).floor() as usize;
    if scaled == 0 { 1 } else { scaled }
}

pub(crate) fn estimate_cardinality_wander_join_flat(
    query_graph: &QueryGraph,
    flat_graph: &FlatGraph,
    sample_size: usize,
    seed: Option<u64>,
) -> GCardResult<f64> {
    if sample_size == 0 {
        return Ok(0.0);
    }

    let (nodes, joins, plans) = compile_wander_join(query_graph, flat_graph, sample_size)?;
    if plans.is_empty() {
        return Ok(0.0);
    }

    let mut rng = rand::rngs::StdRng::seed_from_u64(seed.unwrap_or(0));
    let mut current_plan_idx = 0usize;
    let mut chosen_plan_idx: Option<usize> = None;
    let mut success_count = vec![0usize; plans.len()];
    let mut plan_estimates: Vec<Vec<f64>> = vec![Vec::new(); plans.len()];
    let mut plan_lookups: Vec<Vec<usize>> = vec![Vec::new(); plans.len()];

    let mut total_estimate = 0.0;
    for _ in 0..sample_size {
        let plan_idx = chosen_plan_idx.unwrap_or(current_plan_idx);
        let (estimate, lookup_count) =
            execute_wander_join_sample(flat_graph, &nodes, &joins, &plans[plan_idx], &mut rng)?;
        total_estimate += estimate;

        plan_estimates[plan_idx].push(estimate);
        plan_lookups[plan_idx].push(lookup_count);
        if estimate > 0.0 {
            success_count[plan_idx] += 1;
        }

        if chosen_plan_idx.is_none() {
            if success_count[plan_idx] >= 100 {
                let mut best_idx = None;
                let mut best_score = f64::INFINITY;
                for idx in 0..plans.len() {
                    if success_count[idx] < 50 || plan_estimates[idx].len() < 2 {
                        continue;
                    }
                    let mean =
                        plan_estimates[idx].iter().sum::<f64>() / plan_estimates[idx].len() as f64;
                    let variance = plan_estimates[idx]
                        .iter()
                        .map(|estimate| {
                            let diff = *estimate - mean;
                            diff * diff
                        })
                        .sum::<f64>()
                        / (plan_estimates[idx].len() as f64 - 1.0);
                    let mean_lookup = plan_lookups[idx].iter().sum::<usize>() as f64
                        / plan_lookups[idx].len() as f64;
                    let score = variance * mean_lookup;
                    if score < best_score {
                        best_score = score;
                        best_idx = Some(idx);
                    }
                }
                if let Some(best_idx) = best_idx {
                    chosen_plan_idx = Some(best_idx);
                } else {
                    current_plan_idx = (current_plan_idx + 1) % plans.len();
                }
            } else {
                current_plan_idx = (current_plan_idx + 1) % plans.len();
            }
        }
    }

    Ok(total_estimate / sample_size as f64)
}

fn compile_wander_join(
    query_graph: &QueryGraph,
    flat_graph: &FlatGraph,
    plan_budget: usize,
) -> GCardResult<(
    Vec<WanderJoinNode>,
    Vec<WanderJoinJoin>,
    Vec<WanderJoinPlan>,
)> {
    let mut nodes = Vec::new();

    let mut vertex_ids: Vec<_> = query_graph.inner.vertices.keys().copied().collect();
    vertex_ids.sort_unstable();
    for vertex_id in vertex_ids {
        let vertex = query_graph.inner.vertices.get(&vertex_id).ok_or_else(|| {
            GCardError::VertexNotFound(format!("WanderJoin vertex {} not found", vertex_id))
        })?;
        let predicates = resolve_vertex_predicates(flat_graph, &vertex.label, &vertex.predicates)?;
        nodes.push(WanderJoinNode {
            kind: WanderJoinNodeKind::Vertex {
                query_vertex_id: vertex_id,
                label: vertex.label.clone(),
            },
            predicates,
        });
    }

    let mut edge_ids: Vec<_> = query_graph.inner.edges.keys().copied().collect();
    edge_ids.sort_unstable();
    for edge_id in edge_ids {
        let edge = query_graph.inner.edges.get(&edge_id).ok_or_else(|| {
            GCardError::EdgeNotFound(format!("WanderJoin edge {} not found", edge_id))
        })?;
        let src_label = query_graph
            .inner
            .vertices
            .get(&edge.src_vertex_id)
            .ok_or_else(|| {
                GCardError::VertexNotFound(format!(
                    "WanderJoin source vertex {} not found",
                    edge.src_vertex_id
                ))
            })?
            .label
            .clone();
        let dst_label = query_graph
            .inner
            .vertices
            .get(&edge.dst_vertex_id)
            .ok_or_else(|| {
                GCardError::VertexNotFound(format!(
                    "WanderJoin destination vertex {} not found",
                    edge.dst_vertex_id
                ))
            })?
            .label
            .clone();
        let predicates = resolve_edge_predicates(flat_graph, &edge.label, &edge.predicates)?;
        nodes.push(WanderJoinNode {
            kind: WanderJoinNodeKind::Edge {
                label: edge.label.clone(),
                src_query_vertex_id: edge.src_vertex_id,
                dst_query_vertex_id: edge.dst_vertex_id,
                src_label,
                dst_label,
            },
            predicates,
        });
    }

    let joins = build_wander_join_constraints(&nodes);
    let plans = build_wander_join_plans(nodes.len(), &joins, plan_budget.max(1));
    Ok((nodes, joins, plans))
}

fn resolve_vertex_predicates(
    flat_graph: &FlatGraph,
    label: &str,
    predicates: &[PredicateDef],
) -> GCardResult<Vec<ResolvedPredicate>> {
    predicates
        .iter()
        .map(|pred| {
            let prop_index = flat_graph
                .vertex_prop_index(label, &pred.property)
                .ok_or_else(|| {
                    GCardError::InvalidData(format!(
                        "Vertex property {} not found on label {}",
                        pred.property, label
                    ))
                })?;
            Ok(ResolvedPredicate::new(prop_index, pred))
        })
        .collect()
}

fn resolve_edge_predicates(
    flat_graph: &FlatGraph,
    label: &str,
    predicates: &[PredicateDef],
) -> GCardResult<Vec<ResolvedPredicate>> {
    predicates
        .iter()
        .map(|pred| {
            let prop_index = flat_graph
                .edge_prop_index(label, &pred.property)
                .ok_or_else(|| {
                    GCardError::InvalidData(format!(
                        "Edge property {} not found on label {}",
                        pred.property, label
                    ))
                })?;
            Ok(ResolvedPredicate::new(prop_index, pred))
        })
        .collect()
}

fn build_wander_join_constraints(nodes: &[WanderJoinNode]) -> Vec<WanderJoinJoin> {
    let mut joins = Vec::new();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let refs_i = wander_join_node_query_refs(&nodes[i]);
            let refs_j = wander_join_node_query_refs(&nodes[j]);
            for (col_i, vertex_i) in refs_i.iter().enumerate() {
                for (col_j, vertex_j) in refs_j.iter().enumerate() {
                    if vertex_i == vertex_j {
                        joins.push(WanderJoinJoin {
                            from_node: i,
                            from_col: col_i,
                            to_node: j,
                            to_col: col_j,
                        });
                        joins.push(WanderJoinJoin {
                            from_node: j,
                            from_col: col_j,
                            to_node: i,
                            to_col: col_i,
                        });
                    }
                }
            }
        }
    }
    joins
}

fn wander_join_node_query_refs(node: &WanderJoinNode) -> Vec<VertexId> {
    match &node.kind {
        WanderJoinNodeKind::Vertex {
            query_vertex_id, ..
        } => vec![*query_vertex_id],
        WanderJoinNodeKind::Edge {
            src_query_vertex_id,
            dst_query_vertex_id,
            ..
        } => vec![*src_query_vertex_id, *dst_query_vertex_id],
    }
}

fn build_wander_join_plans(
    node_count: usize,
    joins: &[WanderJoinJoin],
    max_plans: usize,
) -> Vec<WanderJoinPlan> {
    let mut plans = Vec::new();
    for start_node_idx in 0..node_count {
        if plans.len() >= max_plans {
            break;
        }
        let mut visited = vec![false; node_count];
        visited[start_node_idx] = true;
        let mut steps = Vec::new();
        enumerate_wander_join_plans(
            joins,
            &mut visited,
            start_node_idx,
            &mut steps,
            &mut plans,
            max_plans,
        );
    }
    plans
}

fn enumerate_wander_join_plans(
    joins: &[WanderJoinJoin],
    visited: &mut [bool],
    start_node_idx: usize,
    steps: &mut Vec<WanderJoinPlanStep>,
    plans: &mut Vec<WanderJoinPlan>,
    max_plans: usize,
) {
    if plans.len() >= max_plans {
        return;
    }
    if steps.len() + 1 == visited.len() {
        plans.push(WanderJoinPlan {
            start_node_idx,
            steps: steps.clone(),
        });
        return;
    }

    for join in joins {
        if visited[join.from_node] && !visited[join.to_node] {
            visited[join.to_node] = true;
            steps.push(WanderJoinPlanStep {
                node_idx: join.to_node,
                counterpart_node_idx: join.from_node,
                counterpart_col: join.from_col,
                node_col: join.to_col,
            });
            enumerate_wander_join_plans(joins, visited, start_node_idx, steps, plans, max_plans);
            steps.pop();
            visited[join.to_node] = false;
            if plans.len() >= max_plans {
                return;
            }
        }
    }
}

fn execute_wander_join_sample(
    flat_graph: &FlatGraph,
    nodes: &[WanderJoinNode],
    joins: &[WanderJoinJoin],
    plan: &WanderJoinPlan,
    rng: &mut impl rand::Rng,
) -> GCardResult<(f64, usize)> {
    let mut sampled = Vec::with_capacity(nodes.len());
    let mut node_pos = vec![usize::MAX; nodes.len()];
    let mut inv_prob = 1.0f64;
    let mut lookup_count = 0usize;

    let start_binding =
        sample_wander_join_start_node(flat_graph, &nodes[plan.start_node_idx], rng)?;
    lookup_count += 1;
    let Some((binding, factor)) = start_binding else {
        return Ok((0.0, lookup_count));
    };
    inv_prob *= factor as f64;
    sampled.push(binding);
    node_pos[plan.start_node_idx] = 0;

    for step in &plan.steps {
        let prev_pos = node_pos[step.counterpart_node_idx];
        let Some(bound_value) = sampled[prev_pos].value_at(step.counterpart_col) else {
            return Ok((0.0, lookup_count));
        };
        let next_binding = sample_wander_join_next_node(
            flat_graph,
            &nodes[step.node_idx],
            bound_value,
            step.node_col,
            rng,
        )?;
        lookup_count += 1;
        let Some((binding, factor)) = next_binding else {
            return Ok((0.0, lookup_count));
        };
        inv_prob *= factor as f64;
        node_pos[step.node_idx] = sampled.len();
        sampled.push(binding);
    }

    if !check_wander_join_constraints(&sampled, &node_pos, joins) {
        return Ok((0.0, lookup_count));
    }

    Ok((inv_prob, lookup_count))
}

fn check_wander_join_constraints(
    sampled: &[WanderJoinBinding],
    node_pos: &[usize],
    joins: &[WanderJoinJoin],
) -> bool {
    joins.iter().all(|join| {
        let from_pos = node_pos[join.from_node];
        let to_pos = node_pos[join.to_node];
        if from_pos == usize::MAX || to_pos == usize::MAX {
            return false;
        }
        sampled[from_pos].value_at(join.from_col) == sampled[to_pos].value_at(join.to_col)
    })
}

fn sample_wander_join_start_node(
    flat_graph: &FlatGraph,
    node: &WanderJoinNode,
    rng: &mut impl rand::Rng,
) -> GCardResult<Option<(WanderJoinBinding, usize)>> {
    match &node.kind {
        WanderJoinNodeKind::Vertex { label, .. } => {
            let candidates = flat_graph.all_vertex_ids_by_label(label);
            if candidates.is_empty() {
                return Ok(None);
            }
            let idx = rng.gen_range(0..candidates.len());
            let binding = WanderJoinBinding::Vertex(candidates[idx]);
            if evaluate_wander_join_binding(flat_graph, node, &binding)? {
                Ok(Some((binding, candidates.len())))
            } else {
                Ok(None)
            }
        }
        WanderJoinNodeKind::Edge {
            label,
            src_label,
            dst_label,
            ..
        } => {
            let total_edges = flat_graph.edge_count_by_label(label);
            if total_edges == 0 {
                return Ok(None);
            }
            let Some(data_edge_id) = flat_graph.sample_edge_id_by_label(label, rng) else {
                return Ok(None);
            };
            let Some((src, data_src_label, dst, data_dst_label, _)) =
                flat_graph.edge_endpoints(data_edge_id)
            else {
                return Ok(None);
            };
            if data_src_label != src_label || data_dst_label != dst_label {
                return Ok(None);
            }
            let binding = WanderJoinBinding::Edge {
                src,
                dst,
                edge_id: data_edge_id,
            };
            if evaluate_wander_join_binding(flat_graph, node, &binding)? {
                Ok(Some((binding, total_edges)))
            } else {
                Ok(None)
            }
        }
    }
}

fn sample_wander_join_next_node(
    flat_graph: &FlatGraph,
    node: &WanderJoinNode,
    bound_value: VertexId,
    node_col: usize,
    rng: &mut impl rand::Rng,
) -> GCardResult<Option<(WanderJoinBinding, usize)>> {
    match &node.kind {
        WanderJoinNodeKind::Vertex { label, .. } => {
            if flat_graph.vertex_label(bound_value) != Some(label.as_str()) {
                return Ok(None);
            }
            let binding = WanderJoinBinding::Vertex(bound_value);
            if evaluate_wander_join_binding(flat_graph, node, &binding)? {
                Ok(Some((binding, 1)))
            } else {
                Ok(None)
            }
        }
        WanderJoinNodeKind::Edge {
            label,
            src_label,
            dst_label,
            ..
        } => {
            let outgoing = match node_col {
                0 => true,
                1 => false,
                _ => return Ok(None),
            };
            let neighbors: Vec<(VertexId, EdgeId)> = flat_graph
                .neighbors_with_eid(bound_value, label, outgoing)
                .iter()
                .copied()
                .filter(|(other_vertex, edge_id)| {
                    let endpoint_match = if outgoing {
                        flat_graph.vertex_label(*other_vertex) == Some(dst_label.as_str())
                    } else {
                        flat_graph.vertex_label(*other_vertex) == Some(src_label.as_str())
                    };
                    endpoint_match
                        && matches!(
                            flat_graph.edge_endpoints(*edge_id),
                            Some((_, data_src_label, _, data_dst_label, data_label))
                                if data_label == label
                                    && data_src_label == src_label
                                    && data_dst_label == dst_label
                        )
                })
                .collect();
            if neighbors.is_empty() {
                return Ok(None);
            }
            let idx = rng.gen_range(0..neighbors.len());
            let (other_vertex, data_edge_id) = neighbors[idx];
            let (src, dst) = if outgoing {
                (bound_value, other_vertex)
            } else {
                (other_vertex, bound_value)
            };
            let binding = WanderJoinBinding::Edge {
                src,
                dst,
                edge_id: data_edge_id,
            };
            if evaluate_wander_join_binding(flat_graph, node, &binding)? {
                Ok(Some((binding, neighbors.len())))
            } else {
                Ok(None)
            }
        }
    }
}

fn evaluate_wander_join_binding(
    flat_graph: &FlatGraph,
    node: &WanderJoinNode,
    binding: &WanderJoinBinding,
) -> GCardResult<bool> {
    match (&node.kind, binding) {
        (WanderJoinNodeKind::Vertex { label, .. }, WanderJoinBinding::Vertex(data_vertex)) => {
            evaluate_vertex_predicates(flat_graph, label, *data_vertex, &node.predicates)
        }
        (WanderJoinNodeKind::Edge { .. }, WanderJoinBinding::Edge { edge_id, .. }) => {
            evaluate_edge_predicates(flat_graph, *edge_id, &node.predicates)
        }
        _ => Ok(false),
    }
}

fn evaluate_vertex_predicates(
    flat_graph: &FlatGraph,
    label: &str,
    data_vertex: VertexId,
    predicates: &[ResolvedPredicate],
) -> GCardResult<bool> {
    if predicates.is_empty() {
        return Ok(true);
    }
    let Some(props) = flat_graph.vertex_props(label, data_vertex) else {
        return Ok(false);
    };
    for predicate in predicates {
        let Some(value) = props.get(predicate.prop_index) else {
            return Ok(false);
        };
        if !predicate.evaluate(value)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn evaluate_edge_predicates(
    flat_graph: &FlatGraph,
    data_edge: EdgeId,
    predicates: &[ResolvedPredicate],
) -> GCardResult<bool> {
    if predicates.is_empty() {
        return Ok(true);
    }
    let Some(props) = flat_graph.edge_props(data_edge) else {
        return Ok(false);
    };
    for predicate in predicates {
        let Some(value) = props.get(predicate.prop_index) else {
            return Ok(false);
        };
        if !predicate.evaluate(value)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String,
        LogicalType::Float64,
        LogicalType::Int32,
        LogicalType::UInt64,
    ];

    let schema = Arc::new(DataSchema::new(vec![DataField::new(
        "cardinality".into(),
        LogicalType::Int64,
        false,
    )]));

    Procedure::new(parameters, Some(schema), move |context, args| {
        let graph_ref = context.current_graph.clone().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "current graph is not selected",
            )))
        })?;
        let provider = graph_ref.object().clone();
        let container = provider.downcast_ref::<GraphContainer>().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "only GraphContainer graphs support wander_join_query",
            )))
        })?;

        type FlatGraphType = crate::procedures::gcard_query::flat_graph::FlatGraph;
        let flat_graph_arc: Option<std::sync::Arc<FlatGraphType>> = container
            .gcard_flat_graph()
            .and_then(|arc| std::sync::Arc::downcast::<FlatGraphType>(arc).ok());
        let flat_graph_ref = flat_graph_arc.as_deref().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "FlatGraph not loaded (run load_ldbc first)",
            )))
        })?;

        let query_json_path = args[0]
            .try_as_string()
            .expect("first arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("query_json_path cannot be null"))?
            .to_string();
        let sample_ratio = args[1]
            .to_f64()
            .map_err(|e| anyhow::anyhow!("Failed to convert sample_ratio to f64: {:?}", e))?;
        let iterations = args[2]
            .to_i32()
            .map_err(|e| anyhow::anyhow!("Failed to convert iteration to i32: {:?}", e))?
            as usize;
        let seed = args[3]
            .to_u64()
            .map_err(|e| anyhow::anyhow!("Failed to convert random_seed to u64: {:?}", e))?;

        let query_json = fs::read_to_string(&query_json_path).map_err(|e| {
            anyhow::anyhow!("Failed to read query JSON file {}: {}", query_json_path, e)
        })?;
        let query: Query = serde_json::from_str(&query_json)
            .map_err(|e| anyhow::anyhow!("Failed to parse query JSON: {}", e))?;
        let query_graph: QueryGraph = query
            .build_graph()
            .map_err(|e| anyhow::anyhow!("Failed to build query graph: {}", e))?;

        let sample_size = gcare_wander_join_sample_size(&query_graph, flat_graph_ref, sample_ratio);
        let iterations = iterations.max(1);

        let start = Instant::now();
        let mut estimate_sum = 0.0;
        for iter in 0..iterations {
            let iter_seed = seed.saturating_add(iter as u64);
            let estimate = estimate_cardinality_wander_join_flat(
                &query_graph,
                flat_graph_ref,
                sample_size,
                Some(iter_seed),
            )
            .map_err(|e| anyhow::anyhow!("Wander Join estimation failed: {}", e))?;
            estimate_sum += estimate;
        }
        let estimate = estimate_sum / iterations as f64;
        let elapsed = start.elapsed().as_secs_f64();

        let stem = std::path::Path::new(query_json_path.as_str())
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        println!(
            "{}, cardinality: {}, ratio: {:.6}, iterations: {}, sample_size: {}, estimate_time: {:.6}",
            stem,
            estimate.ceil(),
            sample_ratio,
            iterations,
            sample_size,
            elapsed
        );

        Ok(vec![data_chunk!((Int64, [Some(estimate.ceil() as i64)]))])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn eq_pred(target: &str, id: u32, property: &str, value: ScalarValue) -> PredicateDef {
        PredicateDef {
            predicate_id: None,
            target: target.to_string(),
            id,
            property: property.to_string(),
            op: ComparisonOp::Eq,
            value,
            values: Vec::new(),
        }
    }

    #[test]
    fn wander_join_handles_single_path_with_predicate() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("a", vec!["flag".to_string()]);
        builder.set_vertex_prop_schema("b", vec!["flag".to_string()]);
        builder.set_edge_type_schema("ab", "a", "b");
        builder.add_vertex(1, "a", vec![ScalarValue::Int64(Some(1))]);
        builder.add_vertex(2, "b", vec![ScalarValue::Int64(Some(7))]);
        builder.add_edge(10, 1, "a", 2, "b", "ab", vec![]);
        let flat_graph = builder.build();

        let query = Query {
            vertices: vec![vertex(1, "a"), vertex(2, "b")],
            edges: vec![edge(10, "ab", 1, 2)],
            predicates: vec![eq_pred("vertex", 2, "flag", ScalarValue::Int64(Some(7)))],
        };
        let query_graph = query.build_graph().unwrap();

        let estimate =
            estimate_cardinality_wander_join_flat(&query_graph, &flat_graph, 8, Some(0)).unwrap();
        assert_eq!(estimate, 1.0);
    }

    #[test]
    fn wander_join_handles_cycle_query() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_edge_type_schema("ab", "a", "b");
        builder.set_edge_type_schema("bc", "b", "c");
        builder.set_edge_type_schema("ca", "c", "a");
        builder.add_vertex(1, "a", vec![]);
        builder.add_vertex(2, "b", vec![]);
        builder.add_vertex(3, "c", vec![]);
        builder.add_edge(10, 1, "a", 2, "b", "ab", vec![]);
        builder.add_edge(11, 2, "b", 3, "c", "bc", vec![]);
        builder.add_edge(12, 3, "c", 1, "a", "ca", vec![]);
        let flat_graph = builder.build();

        let query = Query {
            vertices: vec![vertex(1, "a"), vertex(2, "b"), vertex(3, "c")],
            edges: vec![
                edge(10, "ab", 1, 2),
                edge(11, "bc", 2, 3),
                edge(12, "ca", 3, 1),
            ],
            predicates: Vec::new(),
        };
        let query_graph = query.build_graph().unwrap();

        let estimate =
            estimate_cardinality_wander_join_flat(&query_graph, &flat_graph, 8, Some(0)).unwrap();
        assert_eq!(estimate, 1.0);
    }
}
