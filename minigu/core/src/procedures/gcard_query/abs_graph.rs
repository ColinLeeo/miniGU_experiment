use std::collections::{HashMap, HashSet, VecDeque};

use minigu_common::types::{EdgeId, VertexId};

use crate::procedures::gcard_query::degreepiecewise::{
    Pcf, alpha_refs, beta_right, record_effective_alpha_refs, record_effective_beta_right,
};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::graph::{Endpoints, GraphSkeleton};
use crate::procedures::gcard_query::types::{AbstractEdge, FunctionalDirection, QueryVertex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContributionKind {
    Empty,
    FunctionalOnly,
    NonFunctional,
}

impl ContributionKind {
    fn is_non_functional(self) -> bool {
        matches!(self, Self::NonFunctional)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::FunctionalOnly => "functional/n-1",
            Self::NonFunctional => "many-to-many/general",
        }
    }

    fn combine<I>(kinds: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        let mut saw_functional = false;
        for kind in kinds {
            match kind {
                Self::NonFunctional => return Self::NonFunctional,
                Self::FunctionalOnly => saw_functional = true,
                Self::Empty => {}
            }
        }
        if saw_functional {
            Self::FunctionalOnly
        } else {
            Self::Empty
        }
    }
}

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
            local_pcfs: std::collections::HashMap::new(),
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

    pub fn add_local_pcf(&mut self, vertex_id: VertexId, pcf: Pcf) {
        self.local_pcfs.entry(vertex_id).or_default().push(pcf);
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

    fn active_root_candidates_after_functional_leaf_prune(&self) -> HashSet<VertexId> {
        let mut remaining: HashSet<VertexId> = self.vertices.keys().copied().collect();

        loop {
            if remaining.len() <= 1 {
                break;
            }

            let mut to_remove = Vec::new();
            for &vertex_id in &remaining {
                if self.local_pcfs.contains_key(&vertex_id) {
                    continue;
                }

                let incident = self
                    .get_neighbor_edges(vertex_id)
                    .into_iter()
                    .filter_map(|edge_id| {
                        let edge = self.edges.get(&edge_id)?;
                        let neighbor = if edge.src == vertex_id {
                            edge.dst
                        } else {
                            edge.src
                        };
                        remaining.contains(&neighbor).then_some((neighbor, edge_id))
                    })
                    .collect::<Vec<_>>();

                if incident.len() == 1 {
                    let (neighbor, edge_id) = incident[0];
                    if self.edge_is_omittable_functional_from_to(edge_id, neighbor, vertex_id) {
                        to_remove.push(vertex_id);
                    }
                }
            }

            if to_remove.is_empty() {
                break;
            }
            for vertex_id in to_remove {
                remaining.remove(&vertex_id);
            }
        }

        remaining
    }

    pub fn pick_root(&self) -> Option<VertexId> {
        // 选一个“最居中”的根：使最大层数最小。
        // 先剥掉可由 n-1 functional 语义省略的 leaf，再在剩余 active core 上比较，
        // 避免同一个核心因为不同 functional leaf 挂载位置而选到不同 root。
        let vertices: Vec<VertexId> = self.vertices.keys().copied().collect();
        if vertices.is_empty() {
            return None;
        }

        let active_candidates = self.active_root_candidates_after_functional_leaf_prune();
        let candidates: Vec<VertexId> = if active_candidates.is_empty() {
            vertices.clone()
        } else {
            let mut candidates = active_candidates.iter().copied().collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates
        };
        let active_for_scoring: HashSet<VertexId> = candidates.iter().copied().collect();

        let mut best_root = candidates[0];
        let mut best_max_gen = usize::MAX;
        let mut best_local_pcf_count = 0usize;
        let mut best_active_degree = 0usize;
        let mut best_total_degree = 0usize;

        for &candidate in &candidates {
            let generations = self.get_topological_generations(candidate);
            let max_gen = active_for_scoring
                .iter()
                .filter_map(|vertex_id| generations.get(vertex_id).copied())
                .max()
                .unwrap_or(0);
            let local_pcf_count = self.local_pcfs.get(&candidate).map(Vec::len).unwrap_or(0);
            let active_degree = self
                .get_neighbors(candidate)
                .into_iter()
                .filter(|neighbor| active_for_scoring.contains(neighbor))
                .count();
            let total_degree = self.get_degree(candidate);

            // Eccentricity remains the primary criterion. For equally central
            // roots, keep predicate-owned local PCFs in their native coordinate
            // and prefer the denser star center. Both tie-breakers avoid an
            // otherwise unnecessary beta projection without doing more sampling.
            let is_better = max_gen < best_max_gen
                || (max_gen == best_max_gen && local_pcf_count > best_local_pcf_count)
                || (max_gen == best_max_gen
                    && local_pcf_count == best_local_pcf_count
                    && active_degree > best_active_degree)
                || (max_gen == best_max_gen
                    && local_pcf_count == best_local_pcf_count
                    && active_degree == best_active_degree
                    && total_degree > best_total_degree)
                || (max_gen == best_max_gen
                    && local_pcf_count == best_local_pcf_count
                    && active_degree == best_active_degree
                    && total_degree == best_total_degree
                    && candidate < best_root);
            if is_better {
                best_max_gen = max_gen;
                best_local_pcf_count = local_pcf_count;
                best_active_degree = active_degree;
                best_total_degree = total_degree;
                best_root = candidate;
            }
        }
        Some(best_root)
    }

    pub fn describe_plan(&self) -> String {
        let mut out = String::new();
        let root = self.pick_root();
        out.push_str(&format!("  root={:?}\n", root));

        let mut edge_ids: Vec<_> = self.edges.keys().copied().collect();
        edge_ids.sort_unstable();
        out.push_str("  abstract_edges:\n");
        for edge_id in edge_ids {
            if let Some(edge) = self.edges.get(&edge_id) {
                let src_label = self
                    .vertices
                    .get(&edge.src)
                    .map(|v| v.label.as_str())
                    .unwrap_or("?");
                let dst_label = self
                    .vertices
                    .get(&edge.dst)
                    .map(|v| v.label.as_str())
                    .unwrap_or("?");
                out.push_str(&format!(
                    "    ae{}: {}({}) -> {}({}), originals={:?}, path_vertices={:?}, path={}\n",
                    edge_id,
                    edge.src,
                    src_label,
                    edge.dst,
                    dst_label,
                    edge.original_edge_ids,
                    edge.path_vertices,
                    edge.path_str,
                ));
            }
        }

        if !self.local_pcfs.is_empty() {
            let mut vertex_ids: Vec<_> = self.local_pcfs.keys().copied().collect();
            vertex_ids.sort_unstable();
            out.push_str("  local_star_pcfs:\n");
            for vertex_id in vertex_ids {
                let label = self
                    .vertices
                    .get(&vertex_id)
                    .map(|v| v.label.as_str())
                    .unwrap_or("?");
                let count = self
                    .local_pcfs
                    .get(&vertex_id)
                    .map(|v| v.len())
                    .unwrap_or(0);
                out.push_str(&format!(
                    "    {}({}): {} local factors\n",
                    vertex_id, label, count
                ));
            }
        }

        out
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

    fn edge_is_functional_from_to(&self, edge_id: EdgeId, from: VertexId, to: VertexId) -> bool {
        let Some(edge) = self.edges.get(&edge_id) else {
            return false;
        };
        if edge.src == from && edge.dst == to {
            edge.functional.is_functional_src_to_dst()
        } else if edge.dst == from && edge.src == to {
            edge.functional.is_functional_dst_to_src()
        } else {
            false
        }
    }

    fn edge_is_omittable_functional_from_to(
        &self,
        edge_id: EdgeId,
        from: VertexId,
        to: VertexId,
    ) -> bool {
        self.edges
            .get(&edge_id)
            .is_some_and(|edge| edge.predicates.is_empty())
            && self.edge_is_functional_from_to(edge_id, from, to)
    }

    fn edge_contribution_kind_from_to(
        &self,
        edge_id: EdgeId,
        from: VertexId,
        to: VertexId,
    ) -> ContributionKind {
        if !self.edges.contains_key(&edge_id) {
            return ContributionKind::Empty;
        };
        let _ = (from, to);
        ContributionKind::NonFunctional
    }

    fn is_omittable_functional_leaf(
        &self,
        vertex_id: VertexId,
        parent_id: VertexId,
        parent_edge_id: EdgeId,
        has_children: bool,
    ) -> bool {
        !has_children
            && !self.local_pcfs.contains_key(&vertex_id)
            // A functional edge preserves cardinality only before filtering.  Once the
            // contracted path carries a predicate, its sampled PCF is a real unary
            // message at the parent and must participate in the reduction.
            && self.edge_is_omittable_functional_from_to(
                parent_edge_id,
                parent_id,
                vertex_id,
            )
    }

    fn vertex_label(&self, vertex_id: VertexId) -> String {
        self.vertices
            .get(&vertex_id)
            .map(|v| format!("{}({})", vertex_id, v.label))
            .unwrap_or_else(|| vertex_id.to_string())
    }

    fn edge_ref(&self, edge_id: EdgeId) -> String {
        if let Some(edge) = self.edges.get(&edge_id) {
            format!(
                "ae{}:{}->{}",
                edge_id,
                self.vertex_label(edge.src),
                self.vertex_label(edge.dst)
            )
        } else {
            format!("ae{}", edge_id)
        }
    }

    pub fn describe_reduction_trace(&self) -> String {
        let mut out = String::new();
        let Some(root) = self.pick_root() else {
            out.push_str("root=<none>\n");
            return out;
        };
        out.push_str(&format!("root={}\n", self.vertex_label(root)));
        out.push_str("abstract_edges:\n");
        let mut edge_ids: Vec<_> = self.edges.keys().copied().collect();
        edge_ids.sort_unstable();
        for edge_id in edge_ids {
            if let Some(edge) = self.edges.get(&edge_id) {
                let kind = if edge.functional == FunctionalDirection::None {
                    "many-to-many/general"
                } else {
                    "functional/n-1 contracted"
                };
                out.push_str(&format!(
                    "  {} kind={} functional={:?} originals={:?} path_vertices={:?} path={}\n",
                    self.edge_ref(edge_id),
                    kind,
                    edge.functional,
                    edge.original_edge_ids,
                    edge.path_vertices,
                    edge.path_str
                ));
            }
        }

        let generations = self.get_topological_generations(root);
        let max_gen = generations.values().copied().max().unwrap_or(0);
        let mut gen_to_vertices: Vec<Vec<VertexId>> = vec![Vec::new(); max_gen + 1];
        for (&v, &g) in &generations {
            gen_to_vertices[g].push(v);
        }

        let mut parent_map: HashMap<VertexId, (VertexId, EdgeId)> = HashMap::new();
        let mut children_map: HashMap<VertexId, Vec<(VertexId, EdgeId)>> = HashMap::new();
        for (&edge_id, edge) in &self.edges {
            let src_gen = generations.get(&edge.src).copied();
            let dst_gen = generations.get(&edge.dst).copied();
            if let (Some(sg), Some(dg)) = (src_gen, dst_gen) {
                if sg + 1 == dg {
                    parent_map.insert(edge.dst, (edge.src, edge_id));
                    children_map
                        .entry(edge.src)
                        .or_default()
                        .push((edge.dst, edge_id));
                } else if dg + 1 == sg {
                    parent_map.insert(edge.src, (edge.dst, edge_id));
                    children_map
                        .entry(edge.dst)
                        .or_default()
                        .push((edge.src, edge_id));
                }
            }
        }

        out.push_str("reduction:\n");
        let mut child_kinds: HashMap<VertexId, ContributionKind> = HashMap::new();
        let mut child_descs: HashMap<VertexId, String> = HashMap::new();
        for cur_gen in (0..=max_gen).rev() {
            for &v in &gen_to_vertices[cur_gen] {
                let local_kind = if self.local_pcfs.contains_key(&v) {
                    ContributionKind::NonFunctional
                } else {
                    ContributionKind::Empty
                };
                let local_desc = if self.local_pcfs.contains_key(&v) {
                    Some("local-star/predicate".to_string())
                } else {
                    None
                };

                let mut input_kinds = Vec::new();
                let mut input_descs = Vec::new();
                if let Some(children) = children_map.get(&v) {
                    for (child_id, child_edge_id) in children {
                        let kind = child_kinds.get(child_id).copied().unwrap_or_else(|| {
                            self.edge_contribution_kind_from_to(*child_edge_id, v, *child_id)
                        });
                        if kind == ContributionKind::Empty {
                            continue;
                        }
                        let desc = child_descs
                            .get(child_id)
                            .cloned()
                            .unwrap_or_else(|| self.edge_ref(*child_edge_id));
                        input_kinds.push(kind);
                        input_descs.push(desc);
                    }
                }
                if let Some(desc) = local_desc {
                    input_kinds.push(local_kind);
                    input_descs.push(desc);
                }

                let combined_child_kind = ContributionKind::combine(input_kinds.iter().copied());
                let non_functional_inputs = input_kinds
                    .iter()
                    .filter(|kind| kind.is_non_functional())
                    .count();

                if !input_descs.is_empty() {
                    let op = if non_functional_inputs >= 2 {
                        "REAL alpha"
                    } else {
                        "skip-alpha functional/n-1 or unary"
                    };
                    out.push_str(&format!(
                        "  at {} combine_children [{}] => {} ({})\n",
                        self.vertex_label(v),
                        input_descs.join(" + "),
                        op,
                        combined_child_kind.as_str()
                    ));
                }

                if cur_gen == 0 {
                    continue;
                }

                let Some(&(parent, parent_edge_id)) = parent_map.get(&v) else {
                    continue;
                };
                let has_children = children_map
                    .get(&v)
                    .is_some_and(|children| !children.is_empty());
                let omits_parent_edge =
                    self.is_omittable_functional_leaf(v, parent, parent_edge_id, has_children);
                let parent_edge_kind = if omits_parent_edge {
                    ContributionKind::Empty
                } else {
                    self.edge_contribution_kind_from_to(parent_edge_id, parent, v)
                };

                if omits_parent_edge && input_descs.is_empty() {
                    out.push_str(&format!(
                        "  at {} omit_leaf {} => skip functional/n-1\n",
                        self.vertex_label(v),
                        self.edge_ref(parent_edge_id)
                    ));
                }

                let projected_kind = if input_descs.is_empty() {
                    parent_edge_kind
                } else {
                    if combined_child_kind.is_non_functional() {
                        out.push_str(&format!(
                            "  at {} project [{}] across {} to {} => REAL beta\n",
                            self.vertex_label(v),
                            input_descs.join(" + "),
                            self.edge_ref(parent_edge_id),
                            self.vertex_label(parent)
                        ));
                    } else {
                        out.push_str(&format!(
                            "  at {} project [{}] across {} to {} => skip-beta functional/n-1\n",
                            self.vertex_label(v),
                            input_descs.join(" + "),
                            self.edge_ref(parent_edge_id),
                            self.vertex_label(parent)
                        ));
                    }
                    combined_child_kind
                };

                let parent_desc = self.edge_ref(parent_edge_id);
                let result_kind = if input_descs.is_empty() {
                    parent_edge_kind
                } else {
                    let real_alpha =
                        projected_kind.is_non_functional() && parent_edge_kind.is_non_functional();
                    out.push_str(&format!(
                        "  at {} attach_parent [{}] + [{}] => {} ({})\n",
                        self.vertex_label(v),
                        input_descs.join(" + "),
                        parent_desc,
                        if real_alpha {
                            "REAL alpha"
                        } else {
                            "skip-alpha functional/n-1"
                        },
                        ContributionKind::combine([projected_kind, parent_edge_kind]).as_str()
                    ));
                    ContributionKind::combine([projected_kind, parent_edge_kind])
                };

                child_kinds.insert(v, result_kind);
                if input_descs.is_empty() {
                    child_descs.insert(v, parent_desc);
                } else {
                    child_descs.insert(v, format!("{} + {}", input_descs.join(" + "), parent_desc));
                }
            }
        }

        out
    }

    fn multiply_local_and<'a, I>(&'a self, vertex_id: VertexId, pcfs: I) -> Pcf
    where
        I: IntoIterator<Item = &'a Pcf>,
    {
        // Star statistics are unary factors anchored at `vertex_id`, matching PathCE's
        // `star_*` temp table shape: (center, center_mode, count). They should be
        // multiplied with the other factors currently expressed on the same vertex.
        let mut refs: Vec<&Pcf> = Vec::new();
        if let Some(local_pcfs) = self.local_pcfs.get(&vertex_id) {
            refs.extend(local_pcfs.iter());
        }
        refs.extend(pcfs);
        alpha_refs(&refs)
    }

    fn single_edge_cardinality(edge: &AbstractEdge) -> f64 {
        let src_rows = edge.src_pcf.get_num_rows();
        let dst_rows = edge.dst_pcf.get_num_rows();
        match edge.functional {
            FunctionalDirection::SrcToDst => src_rows.max(0.0),
            FunctionalDirection::DstToSrc => dst_rows.max(0.0),
            FunctionalDirection::Both | FunctionalDirection::None => {
                if src_rows <= 0.0 {
                    dst_rows.max(0.0)
                } else if dst_rows <= 0.0 {
                    src_rows.max(0.0)
                } else {
                    src_rows.min(dst_rows)
                }
            }
        }
    }

    pub fn get_es(&mut self) -> GCardResult<f64> {
        // `es` 可以理解为当前抽象图估算出来的结果规模。
        // 做法是把抽象图当成一棵（或近似树状）结构，自底向上组合每条边的 PCF。
        if self.vertices.len() <= 1 {
            if let Some((&vertex_id, _)) = self.vertices.iter().next() {
                if let Some(local_pcfs) = self.local_pcfs.get(&vertex_id) {
                    return Ok(self
                        .multiply_local_and(vertex_id, std::iter::empty())
                        .get_num_rows());
                }
            }
            return Err(GCardError::InvalidState(
                "AbstractGraph must have at least 2 vertices".to_string(),
            ));
        }

        if self.edges.len() == 1 && self.vertices.len() == 2 && self.local_pcfs.is_empty() {
            let edge = self.edges.values().next().ok_or_else(|| {
                GCardError::InvalidState("Single-edge graph must have an edge".to_string())
            })?;
            return Ok(Self::single_edge_cardinality(edge));
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
        let mut child_vertex_kind: HashMap<VertexId, ContributionKind> = HashMap::new();

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

                let (result, result_kind) = if let Some(children) = children {
                    let child_contributions = children
                        .iter()
                        .filter_map(|(child_id, _)| {
                            let kind = child_vertex_kind
                                .get(child_id)
                                .copied()
                                .unwrap_or(ContributionKind::Empty);
                            if kind == ContributionKind::Empty {
                                None
                            } else {
                                child_vertex_pcf
                                    .get(child_id)
                                    .cloned()
                                    .map(|pcf| (pcf, kind))
                            }
                        })
                        .collect::<Vec<_>>();
                    let child_pcfs = child_contributions
                        .iter()
                        .map(|(pcf, _)| pcf.clone())
                        .collect::<Vec<_>>();
                    let child_kinds = child_contributions
                        .iter()
                        .map(|(_, kind)| *kind)
                        .collect::<Vec<_>>();
                    let local_kind = if self.local_pcfs.contains_key(&v) {
                        ContributionKind::NonFunctional
                    } else {
                        ContributionKind::Empty
                    };
                    if child_pcfs.is_empty() && local_kind == ContributionKind::Empty {
                        if cur_gen > 0 {
                            let parent_edge_kind = parent_opt
                                .map(|&(parent, parent_edge_id)| {
                                    self.edge_contribution_kind_from_to(parent_edge_id, parent, v)
                                })
                                .unwrap_or(ContributionKind::Empty);
                            (parent_to_vertex_pcf, parent_edge_kind)
                        } else {
                            (Pcf::empty(), ContributionKind::Empty)
                        }
                    } else {
                        let multiplied_kind = ContributionKind::combine(
                            child_kinds
                                .iter()
                                .copied()
                                .chain(std::iter::once(local_kind)),
                        );
                        let non_functional_child_count = child_kinds
                            .iter()
                            .filter(|kind| kind.is_non_functional())
                            .count()
                            + usize::from(local_kind.is_non_functional());
                        if non_functional_child_count >= 2 {
                            record_effective_alpha_refs();
                        }
                        let multiplied_child_pcf = self.multiply_local_and(v, child_pcfs.iter());
                        let projected = if cur_gen > 0 {
                            // 非根节点需要把“孩子方向的统计”投影回父边所处的坐标系。
                            if multiplied_kind.is_non_functional() {
                                record_effective_beta_right();
                            }
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
                            let parent_edge_kind = parent_opt
                                .map(|&(parent, parent_edge_id)| {
                                    self.edge_contribution_kind_from_to(parent_edge_id, parent, v)
                                })
                                .unwrap_or(ContributionKind::Empty);
                            if usize::from(multiplied_kind.is_non_functional())
                                + usize::from(parent_edge_kind.is_non_functional())
                                >= 2
                            {
                                record_effective_alpha_refs();
                            }
                            let pcf_refs: Vec<&Pcf> = vec![&projected, &parent_to_vertex_pcf];
                            (
                                alpha_refs(&pcf_refs),
                                ContributionKind::combine([multiplied_kind, parent_edge_kind]),
                            )
                        } else {
                            (projected, multiplied_kind)
                        }
                    }
                } else {
                    if cur_gen > 0 {
                        if self.local_pcfs.contains_key(&v) {
                            let local_pcf = self.multiply_local_and(v, std::iter::empty());
                            record_effective_beta_right();
                            let projected = beta_right(
                                &local_pcf,
                                &vertex_to_parent_pcf,
                                &parent_to_vertex_pcf,
                            );
                            let parent_edge_kind = parent_opt
                                .map(|&(parent, parent_edge_id)| {
                                    self.edge_contribution_kind_from_to(parent_edge_id, parent, v)
                                })
                                .unwrap_or(ContributionKind::Empty);
                            if parent_edge_kind.is_non_functional() {
                                record_effective_alpha_refs();
                            }
                            let pcf_refs: Vec<&Pcf> = vec![&projected, &parent_to_vertex_pcf];
                            (alpha_refs(&pcf_refs), ContributionKind::NonFunctional)
                        } else {
                            // 叶子节点没有孩子，它对父节点的贡献就只来自那条父边；
                            // 如果父边是从 n 侧到当前 leaf 1 侧的 functional 边，则该 join
                            // 不改变基数。
                            let (parent, parent_edge_id) =
                                parent_opt.copied().ok_or_else(|| {
                                    GCardError::InvalidState(
                                        "Leaf without root must have a parent edge".to_string(),
                                    )
                                })?;
                            if self.is_omittable_functional_leaf(v, parent, parent_edge_id, false) {
                                (Pcf::empty(), ContributionKind::Empty)
                            } else {
                                (
                                    parent_to_vertex_pcf,
                                    self.edge_contribution_kind_from_to(parent_edge_id, parent, v),
                                )
                            }
                        }
                    } else if self.local_pcfs.contains_key(&v) {
                        (
                            self.multiply_local_and(v, std::iter::empty()),
                            ContributionKind::NonFunctional,
                        )
                    } else {
                        (Pcf::empty(), ContributionKind::Empty)
                    }
                };

                if cur_gen > 0 {
                    child_vertex_pcf.insert(v, result);
                    child_vertex_kind.insert(v, result_kind);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use minigu_common::value::ScalarValue;

    use super::AbstractGraph;
    use crate::procedures::gcard_query::degreepiecewise::{Pcf, alpha_refs, beta_right};
    use crate::procedures::gcard_query::types::{
        AbstractEdge, ComparisonOp, FunctionalDirection, PredicateDef, QueryVertex,
    };

    fn vertex(id: u64, label: &str) -> QueryVertex {
        QueryVertex {
            id,
            label: label.to_string(),
            predicates: Vec::new(),
        }
    }

    fn pcf(constants: Vec<f64>, right_interval_edges: Vec<f64>) -> Pcf {
        let mut cumulative_rows = Vec::new();
        let mut left = 0.0;
        let mut rows = 0.0;
        for (&constant, &right) in constants.iter().zip(&right_interval_edges) {
            rows += constant * (right - left);
            cumulative_rows.push(rows);
            left = right;
        }
        Pcf {
            constants,
            right_interval_edges,
            cumulative_rows,
        }
    }

    fn edge(src: u64, dst: u64, src_pcf: Pcf, dst_pcf: Pcf) -> AbstractEdge {
        edge_with_functional(src, dst, src_pcf, dst_pcf, FunctionalDirection::None)
    }

    fn edge_with_functional(
        src: u64,
        dst: u64,
        src_pcf: Pcf,
        dst_pcf: Pcf,
        functional: FunctionalDirection,
    ) -> AbstractEdge {
        AbstractEdge {
            src,
            dst,
            src_pcf: Arc::new(src_pcf),
            dst_pcf: Arc::new(dst_pcf),
            functional,
            predicates: Vec::new(),
            original_edge_ids: Vec::new(),
            path_vertices: vec![src, dst],
            selectivity: 1.0,
            path_str: String::new(),
        }
    }

    fn with_vertex_predicate(mut edge: AbstractEdge, vertex_id: u32) -> AbstractEdge {
        edge.predicates.push(PredicateDef {
            predicate_id: None,
            target: "vertex".to_string(),
            id: vertex_id,
            property: "flag".to_string(),
            op: ComparisonOp::Eq,
            value: ScalarValue::Boolean(Some(true)),
            values: Vec::new(),
        });
        edge
    }

    #[test]
    fn leaf_local_pcf_is_projected_to_parent_coordinate_before_joining() {
        let parent_side = pcf(vec![2.0], vec![10.0]);
        let leaf_side = pcf(vec![1.0], vec![20.0]);
        let local_star = pcf(vec![10.0], vec![2.0]);
        let root_neutral = pcf(vec![1.0], vec![10.0]);

        let mut graph = AbstractGraph::new();
        graph.add_vertex(vertex(1, "leaf_with_star"));
        graph.add_vertex(vertex(2, "root"));
        graph.add_vertex(vertex(3, "other_leaf"));
        graph.add_edge(1, edge(2, 1, parent_side.clone(), leaf_side.clone()));
        graph.add_edge(2, edge(2, 3, root_neutral.clone(), root_neutral.clone()));
        graph.add_local_pcf(1, local_star.clone());

        let projected = beta_right(&local_star, &leaf_side, &parent_side);
        let expected_leaf = alpha_refs(&[&projected, &parent_side]);
        let expected = alpha_refs(&[&expected_leaf, &root_neutral]).get_num_rows();
        let wrong_coordinate_product =
            alpha_refs(&[&local_star, &parent_side, &root_neutral]).get_num_rows();

        assert_ne!(expected, wrong_coordinate_product);
        assert_eq!(graph.get_es().unwrap(), expected);
    }

    #[test]
    fn functional_leaf_is_omitted_only_in_parent_to_leaf_direction() {
        let active = pcf(vec![2.0], vec![10.0]);
        let functional_parent_side = pcf(vec![9.0], vec![10.0]);
        let functional_leaf_side = pcf(vec![1.0], vec![10.0]);

        let mut forward = AbstractGraph::new();
        forward.add_vertex(vertex(1, "root"));
        forward.add_vertex(vertex(2, "functional_leaf"));
        forward.add_vertex(vertex(3, "active_leaf"));
        forward.add_edge(
            1,
            edge_with_functional(
                1,
                2,
                functional_parent_side.clone(),
                functional_leaf_side.clone(),
                FunctionalDirection::SrcToDst,
            ),
        );
        forward.add_edge(2, edge(1, 3, active.clone(), active.clone()));

        let expected_forward = active.get_num_rows();
        assert_eq!(forward.get_es().unwrap(), expected_forward);

        let mut reverse = AbstractGraph::new();
        reverse.add_vertex(vertex(1, "root"));
        reverse.add_vertex(vertex(2, "functional_leaf"));
        reverse.add_vertex(vertex(3, "active_leaf"));
        reverse.add_edge(
            1,
            edge_with_functional(
                2,
                1,
                functional_leaf_side,
                functional_parent_side.clone(),
                FunctionalDirection::SrcToDst,
            ),
        );
        reverse.add_edge(2, edge(1, 3, active.clone(), active.clone()));

        let expected_reverse = alpha_refs(&[&functional_parent_side, &active]).get_num_rows();
        assert_ne!(expected_reverse, expected_forward);
        assert_eq!(reverse.get_es().unwrap(), expected_reverse);
    }

    #[test]
    fn predicate_filtered_functional_leaves_keep_their_sampled_pcfs() {
        let left_parent_side = pcf(vec![9.0], vec![10.0]);
        let right_parent_side = pcf(vec![7.0], vec![10.0]);
        let leaf_side = pcf(vec![1.0], vec![10.0]);

        let mut graph = AbstractGraph::new();
        graph.add_vertex(vertex(1, "root"));
        graph.add_vertex(vertex(2, "filtered_functional_leaf"));
        graph.add_vertex(vertex(3, "filtered_functional_leaf"));
        graph.add_edge(
            1,
            with_vertex_predicate(
                edge_with_functional(
                    1,
                    2,
                    left_parent_side.clone(),
                    leaf_side.clone(),
                    FunctionalDirection::SrcToDst,
                ),
                2,
            ),
        );
        graph.add_edge(
            2,
            with_vertex_predicate(
                edge_with_functional(
                    1,
                    3,
                    right_parent_side.clone(),
                    leaf_side,
                    FunctionalDirection::SrcToDst,
                ),
                3,
            ),
        );

        let expected = alpha_refs(&[&left_parent_side, &right_parent_side]).get_num_rows();
        assert!(expected > 1.0);
        assert_eq!(graph.get_es().unwrap(), expected);
    }

    #[test]
    fn single_functional_edge_keeps_edge_cardinality() {
        let src_side = pcf(vec![7.0], vec![11.0]);
        let dst_side = pcf(vec![3.0], vec![13.0]);

        let mut graph = AbstractGraph::new();
        graph.add_vertex(vertex(1, "many_side"));
        graph.add_vertex(vertex(2, "one_side"));
        graph.add_edge(
            1,
            edge_with_functional(
                1,
                2,
                src_side.clone(),
                dst_side,
                FunctionalDirection::SrcToDst,
            ),
        );

        assert_eq!(graph.get_es().unwrap(), src_side.get_num_rows());
    }

    #[test]
    fn equally_central_root_prefers_predicate_owned_local_pcf() {
        let neutral = pcf(vec![1.0], vec![10.0]);
        let local = pcf(vec![2.0], vec![5.0]);

        let mut graph = AbstractGraph::new();
        for id in 1..=4 {
            graph.add_vertex(vertex(id, "node"));
        }
        graph.add_edge(1, edge(1, 2, neutral.clone(), neutral.clone()));
        graph.add_edge(2, edge(2, 3, neutral.clone(), neutral.clone()));
        graph.add_edge(3, edge(3, 4, neutral.clone(), neutral.clone()));
        graph.add_local_pcf(3, local);

        // Vertices 2 and 3 have the same eccentricity. Vertex 3 owns the
        // predicate-aware PCF, so rooting there avoids projecting it via beta.
        assert_eq!(graph.pick_root(), Some(3));
    }

    #[test]
    fn equally_central_root_prefers_higher_degree_star_center() {
        let neutral = pcf(vec![1.0], vec![10.0]);

        let mut graph = AbstractGraph::new();
        for id in 1..=5 {
            graph.add_vertex(vertex(id, "node"));
        }
        graph.add_edge(1, edge(1, 2, neutral.clone(), neutral.clone()));
        graph.add_edge(2, edge(2, 3, neutral.clone(), neutral.clone()));
        graph.add_edge(3, edge(3, 4, neutral.clone(), neutral.clone()));
        graph.add_edge(4, edge(3, 5, neutral.clone(), neutral));

        // Vertices 2 and 3 are both centers, but vertex 3 owns the wider star.
        assert_eq!(graph.pick_root(), Some(3));
    }
}
