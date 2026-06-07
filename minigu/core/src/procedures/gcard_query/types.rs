use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use serde::{Deserialize, Serialize};

pub use super::degreepiecewise::Pcf;
use crate::procedures::gcard_query::query_graph::{QueryEdge, QueryGraph};

#[derive(Debug, Clone)]
pub struct AbstractEdge {
    pub src: VertexId,
    pub dst: VertexId,
    pub src_pcf: Arc<Pcf>,
    pub dst_pcf: Arc<Pcf>,
    pub functional: FunctionalDirection,
    pub predicates: Vec<PredicateDef>,
    pub original_edge_ids: Vec<EdgeId>,
    pub path_vertices: Vec<VertexId>,
    pub selectivity: f64,
    pub path_str: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionalDirection {
    None,
    SrcToDst,
    DstToSrc,
    Both,
}

impl FunctionalDirection {
    pub fn from_flags(src_to_dst: bool, dst_to_src: bool) -> Self {
        match (src_to_dst, dst_to_src) {
            (true, true) => Self::Both,
            (true, false) => Self::SrcToDst,
            (false, true) => Self::DstToSrc,
            (false, false) => Self::None,
        }
    }

    pub fn is_functional_src_to_dst(self) -> bool {
        matches!(self, Self::SrcToDst | Self::Both)
    }

    pub fn is_functional_dst_to_src(self) -> bool {
        matches!(self, Self::DstToSrc | Self::Both)
    }
}

pub type PredicateId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComparisonOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateLocation {
    Vertex(VertexId),
    Edge(EdgeId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredicateDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate_id: Option<PredicateId>,
    pub target: String,
    pub id: u32,
    pub property: String,
    pub op: ComparisonOp,
    pub value: ScalarValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VertexDef {
    pub id: VertexId,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeDef {
    pub id: EdgeId,
    pub label: String,
    pub src: VertexId,
    pub dst: VertexId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    /// 原始查询 JSON 的三个核心部分：点、边、谓词。
    pub vertices: Vec<VertexDef>,
    pub edges: Vec<EdgeDef>,
    pub predicates: Vec<PredicateDef>,
}

/// 用户自定义的查询图分解方案。
/// 每个 `AbstractEdgeDef` 指定一条抽象边覆盖的路径顶点和原始边。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionDef {
    pub abstract_edges: Vec<AbstractEdgeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractEdgeDef {
    pub path_vertices: Vec<VertexId>,
    pub original_edge_ids: Vec<EdgeId>,
}

#[derive(Debug, Clone)]
pub struct QueryVertex {
    pub id: VertexId,
    pub label: String,
    pub predicates: Vec<PredicateDef>,
}

#[derive(Clone, Debug)]
pub struct CandidateTree {
    pub edge_ids: HashSet<EdgeId>,
    pub total_score: u64,
}

impl PartialEq for CandidateTree {
    fn eq(&self, other: &Self) -> bool {
        self.edge_ids == other.edge_ids
    }
}

impl Eq for CandidateTree {}

impl PartialOrd for CandidateTree {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CandidateTree {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_key = sorted_edge_ids(&self.edge_ids);
        let other_key = sorted_edge_ids(&other.edge_ids);
        self.total_score
            .cmp(&other.total_score)
            .then_with(|| other_key.cmp(&self_key))
    }
}

fn sorted_edge_ids(edge_ids: &HashSet<EdgeId>) -> Vec<EdgeId> {
    let mut ids: Vec<_> = edge_ids.iter().copied().collect();
    ids.sort_unstable();
    ids
}

impl Query {
    pub fn build_graph(self) -> Result<QueryGraph, anyhow::Error> {
        // 这一步是“把 JSON 变成算法内部图结构”的统一入口。
        // 后续算法基本都不直接面向 JSON，而是依赖这里构造出的索引。
        let mut graph = QueryGraph::new();

        let mut vertex_ids = std::collections::HashSet::new();
        for vertex_def in &self.vertices {
            // 显式校验重复 id，避免后面 HashMap 插入时静默覆盖。
            if !vertex_ids.insert(vertex_def.id) {
                return Err(anyhow::anyhow!("Duplicate vertex id: {}", vertex_def.id));
            }
        }

        let mut edge_ids = std::collections::HashSet::new();
        for edge_def in &self.edges {
            if !edge_ids.insert(edge_def.id) {
                return Err(anyhow::anyhow!("Duplicate edge id: {}", edge_def.id));
            }
        }

        for vertex_def in &self.vertices {
            graph.inner.vertices.insert(
                vertex_def.id,
                QueryVertex {
                    id: vertex_def.id,
                    // 统一转小写，减少后续按 label 匹配时的大小写分支。
                    label: vertex_def.label.to_lowercase(),
                    predicates: Vec::new(),
                },
            );
        }

        for edge_def in &self.edges {
            if !graph.inner.vertices.contains_key(&edge_def.src) {
                return Err(anyhow::anyhow!(
                    "Edge {} references non-existent source vertex {}",
                    edge_def.id,
                    edge_def.src
                ));
            }
            if !graph.inner.vertices.contains_key(&edge_def.dst) {
                return Err(anyhow::anyhow!(
                    "Edge {} references non-existent destination vertex {}",
                    edge_def.id,
                    edge_def.dst
                ));
            }

            graph.inner.edges.insert(
                edge_def.id,
                QueryEdge {
                    id: edge_def.id,
                    label: edge_def.label.to_lowercase(),
                    src_vertex_id: edge_def.src,
                    dst_vertex_id: edge_def.dst,
                    predicates: Vec::new(),
                },
            );

            graph
                .inner
                .outgoing_edges
                .entry(edge_def.src)
                .or_insert_with(Vec::new)
                .push(edge_def.id);

            graph
                .inner
                .incoming_edges
                .entry(edge_def.dst)
                .or_insert_with(Vec::new)
                .push(edge_def.id);
        }

        let mut next_predicate_id: PredicateId = 1;
        let mut predicate_ids_used = std::collections::HashSet::new();

        for mut predicate_def in self.predicates {
            let predicate_id = if let Some(id) = predicate_def.predicate_id {
                if !predicate_ids_used.insert(id) {
                    return Err(anyhow::anyhow!("Duplicate predicate id: {}", id));
                }
                id
            } else {
                // 自动分配谓词 id，保证后面可以稳定地通过 id 找到其挂载位置。
                while predicate_ids_used.contains(&next_predicate_id) {
                    next_predicate_id += 1;
                }
                predicate_ids_used.insert(next_predicate_id);
                let id = next_predicate_id;
                next_predicate_id += 1;
                predicate_def.predicate_id = Some(id);
                id
            };

            let (location, idx) = match predicate_def.target.as_str() {
                "vertex" => {
                    let vertex_id = predicate_def.id as VertexId;
                    if let Some(vertex) = graph.inner.vertices.get_mut(&vertex_id) {
                        // `predicate_index` 里不仅记位置，还记该位置上的第几个谓词。
                        // 因为一个点/边上可能挂多个谓词。
                        let idx = vertex.predicates.len();
                        vertex.predicates.push(predicate_def.clone());
                        (PredicateLocation::Vertex(vertex_id), idx)
                    } else {
                        return Err(anyhow::anyhow!(
                            "Predicate references non-existent vertex id: {}",
                            predicate_def.id
                        ));
                    }
                }
                "edge" => {
                    let edge_id = predicate_def.id as EdgeId;
                    if let Some(edge) = graph.inner.edges.get_mut(&edge_id) {
                        let idx = edge.predicates.len();
                        edge.predicates.push(predicate_def.clone());
                        (PredicateLocation::Edge(edge_id), idx)
                    } else {
                        return Err(anyhow::anyhow!(
                            "Predicate references non-existent edge id: {}",
                            predicate_def.id
                        ));
                    }
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Invalid predicate target: {}, must be 'vertex' or 'edge'",
                        predicate_def.target
                    ));
                }
            };
            // 这张反向索引非常关键：
            // 之后很多地方只拿 predicate_id，就能反查它到底挂在哪、在列表里第几个。
            graph.predicate_index.insert(predicate_id, (location, idx));
        }

        Ok(graph)
    }
}
