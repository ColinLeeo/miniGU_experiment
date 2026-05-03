use std::collections::{HashMap, HashSet, VecDeque};

use minigu_common::types::{EdgeId, VertexId};

use crate::procedures::gcard_query::degreepiecewise::{Pcf, alpha, alpha_refs, beta_right};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::graph::{Endpoints, GraphSkeleton};
use crate::procedures::gcard_query::types::{AbstractEdge, QueryVertex};

impl Endpoints for AbstractEdge {
    fn src(&self) -> VertexId {
        self.src
    }

    fn dst(&self) -> VertexId {
        self.dst
    }
}

pub type AbstractGraph = GraphSkeleton<AbstractEdge>;

impl GraphSkeleton<AbstractEdge> {
    pub fn new() -> Self {
        Self {
            vertices: std::collections::HashMap::new(),
            edges: std::collections::HashMap::new(),
            outgoing_edges: std::collections::HashMap::new(),
            incoming_edges: std::collections::HashMap::new(),
        }
    }

    pub fn add_vertex(&mut self, vertex: QueryVertex) {
        self.vertices.insert(vertex.id, vertex);
    }

    pub fn add_edge(&mut self, edge_id: EdgeId, edge: AbstractEdge) {
        // 抽象图沿用通用图骨架，所以这里除了存边本体，
        // 还要同步维护入/出边索引。
        let (src, dst) = (edge.src, edge.dst);
        self.edges.insert(edge_id, edge);
        self.outgoing_edges.entry(src).or_default().push(edge_id);
        self.incoming_edges.entry(dst).or_default().push(edge_id);
    }

    pub fn remove_edge(&mut self, edge_id: EdgeId) -> Option<AbstractEdge> {
        let edge = self.edges.remove(&edge_id)?;
        let (src, dst) = (edge.src, edge.dst);
        if let Some(v) = self.outgoing_edges.get_mut(&src) {
            v.retain(|&id| id != edge_id);
        }
        if let Some(v) = self.incoming_edges.get_mut(&dst) {
            v.retain(|&id| id != edge_id);
        }
        Some(edge)
    }

    pub fn remove_vertex(&mut self, vertex_id: VertexId) -> Option<QueryVertex> {
        self.vertices.remove(&vertex_id)
    }

    pub fn get_topological_generations(&self, root: VertexId) -> HashMap<VertexId, usize> {
        // 从 root 出发做 BFS，得到“离根的层数”。
        // 后面的自底向上规约会按这个层次来传播 PCF。
        let mut generations = HashMap::new();
        let mut queue = VecDeque::from([(root, 0usize)]);
        let mut seen = HashSet::from([root]);

        while let Some((v, g)) = queue.pop_front() {
            generations.insert(v, g);
            for neighbor in self.get_neighbors(v) {
                if seen.insert(neighbor) {
                    queue.push_back((neighbor, g + 1));
                }
            }
        }
        generations
    }

    pub fn pick_root(&self) -> Option<VertexId> {
        // 选一个“最居中”的根：使最大层数最小。
        // 这样做能减小规约深度，通常也让传播更平衡。
        let vertices: Vec<VertexId> = self.vertices.keys().copied().collect();
        if vertices.is_empty() {
            return None;
        }

        let mut best_root = vertices[0];
        let mut min_max_gen = usize::MAX;

        for &candidate in &vertices {
            let generations = self.get_topological_generations(candidate);
            let max_gen = generations.values().copied().max().unwrap_or(0);
            if max_gen < min_max_gen {
                min_max_gen = max_gen;
                best_root = candidate;
            }
        }
        Some(best_root)
    }

    fn get_parent_edge(
        &self,
        v: VertexId,
        root: VertexId,
        generations: &HashMap<VertexId, usize>,
    ) -> Option<(VertexId, EdgeId)> {
        let cur_gen = *generations.get(&v)?;
        if cur_gen == 0 {
            return None;
        }
        let parent_gen = cur_gen - 1;
        for edge_id in self.get_neighbor_edges(v) {
            let edge = self.edges.get(&edge_id)?;
            let neighbor = if edge.src == v { edge.dst } else { edge.src };
            if generations.get(&neighbor) == Some(&parent_gen) {
                return Some((neighbor, edge_id));
            }
        }
        None
    }

    fn get_children_edges(
        &self,
        v: VertexId,
        generations: &HashMap<VertexId, usize>,
    ) -> Vec<(VertexId, EdgeId)> {
        let cur_gen = match generations.get(&v) {
            Some(&g) => g,
            None => return vec![],
        };
        let child_gen = cur_gen + 1;
        let mut children = Vec::new();
        for edge_id in self.get_neighbor_edges(v) {
            if let Some(edge) = self.edges.get(&edge_id) {
                let neighbor = if edge.src == v { edge.dst } else { edge.src };
                if generations.get(&neighbor) == Some(&child_gen) {
                    children.push((neighbor, edge_id));
                }
            }
        }
        children
    }

    fn get_edge_pcf_at_vertex(&self, edge_id: EdgeId, vertex_id: VertexId) -> Pcf {
        // 同一条抽象边在两端可能对应不同的投影视角，
        // 所以需要根据当前站在哪个顶点来取 src_pcf / dst_pcf。
        if let Some(edge) = self.edges.get(&edge_id) {
            if edge.src == vertex_id {
                edge.src_pcf.as_ref().clone()
            } else {
                edge.dst_pcf.as_ref().clone()
            }
        } else {
            Pcf::empty()
        }
    }

    pub fn get_es(&mut self) -> GCardResult<f64> {
        // `es` 可以理解为当前抽象图估算出来的结果规模。
        // 做法是把抽象图当成一棵（或近似树状）结构，自底向上组合每条边的 PCF。
        if self.vertices.len() <= 1 {
            return Err(GCardError::InvalidState(
                "AbstractGraph must have at least 2 vertices".to_string(),
            ));
        }

        let root = self
            .pick_root()
            .ok_or_else(|| GCardError::InvalidState("Empty graph".to_string()))?;

        let generations = self.get_topological_generations(root);
        let max_gen = generations.values().copied().max().unwrap_or(0);

        // 先把“每一层有哪些点”预建出来，避免后面每层都扫整个 generations。
        let mut gen_to_vertices: Vec<Vec<VertexId>> = vec![Vec::new(); max_gen + 1];
        for (&v, &g) in &generations {
            gen_to_vertices[g].push(v);
        }

        // 再预建父子映射。
        // 后续规约阶段如果每次都现场找 parent/children，会有很多重复查找。
        let mut parent_map: HashMap<VertexId, (VertexId, EdgeId)> = HashMap::new();
        let mut children_map: HashMap<VertexId, Vec<(VertexId, EdgeId)>> = HashMap::new();
        for (&edge_id, edge) in &self.edges {
            let src_gen = generations.get(&edge.src).copied();
            let dst_gen = generations.get(&edge.dst).copied();
            if let (Some(sg), Some(dg)) = (src_gen, dst_gen) {
                if sg + 1 == dg {
                    // src is parent of dst
                    parent_map.insert(edge.dst, (edge.src, edge_id));
                    children_map
                        .entry(edge.src)
                        .or_default()
                        .push((edge.dst, edge_id));
                } else if dg + 1 == sg {
                    // dst is parent of src
                    parent_map.insert(edge.src, (edge.dst, edge_id));
                    children_map
                        .entry(edge.dst)
                        .or_default()
                        .push((edge.src, edge_id));
                }
            }
        }

        let mut child_vertex_pcf: HashMap<VertexId, Pcf> = HashMap::new();

        for cur_gen in (0..=max_gen).rev() {
            // 逆层序遍历：先把叶子规约完，再把信息逐层往根上传。
            for &v in &gen_to_vertices[cur_gen] {
                let parent_opt = parent_map.get(&v);
                let children = children_map.get(&v);

                let parent_to_vertex_pcf = if let Some(&(parent, parent_edge_id)) = parent_opt {
                    self.get_edge_pcf_at_vertex(parent_edge_id, parent)
                } else {
                    Pcf::empty()
                };

                let vertex_to_parent_pcf = if let Some(&(_parent, parent_edge_id)) = parent_opt {
                    self.get_edge_pcf_at_vertex(parent_edge_id, v)
                } else {
                    Pcf::empty()
                };

                let result = if let Some(children) = children {
                    let child_pcfs: Vec<Pcf> = children
                        .iter()
                        .filter_map(|(child_id, _)| child_vertex_pcf.get(child_id).cloned())
                        .collect();
                    let multiplied_child_pcf = if child_pcfs.is_empty() {
                        Pcf::empty()
                    } else {
                        // `alpha_refs` 可以理解成把多个子树的贡献做合并。
                        let refs: Vec<&Pcf> = child_pcfs.iter().collect();
                        alpha_refs(&refs)
                    };
                    let projected = if cur_gen > 0 {
                        // 非根节点需要把“孩子方向的统计”投影回父边所处的坐标系。
                        beta_right(
                            &multiplied_child_pcf,
                            &vertex_to_parent_pcf,
                            &parent_to_vertex_pcf,
                        )
                    } else {
                        multiplied_child_pcf
                    };
                    if cur_gen > 0 {
                        // 然后再把“来自孩子的贡献”和“本节点到父节点的边 PCF”乘起来。
                        let pcf_refs: Vec<&Pcf> = vec![&projected, &parent_to_vertex_pcf];
                        alpha_refs(&pcf_refs)
                    } else {
                        projected
                    }
                } else {
                    // 叶子节点没有孩子，它对父节点的贡献就只来自那条父边。
                    parent_to_vertex_pcf
                };

                if cur_gen > 0 {
                    child_vertex_pcf.insert(v, result);
                } else {
                    // 到根时，不再向上返回 PCF，而是直接把最终估算规模取出来。
                    return Ok(result.get_num_rows());
                }
            }
        }

        Err(GCardError::InvalidState(
            "Reduction did not reach root".to_string(),
        ))
    }

    pub fn get_selectivity(&self) -> f64 {
        let mut selectivity = 1.0;
        for edge in self.edges.values() {
            selectivity *= edge.selectivity;
        }
        selectivity
    }
}
