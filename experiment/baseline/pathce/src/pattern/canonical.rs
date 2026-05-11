use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::mem;

use itertools::Itertools;

use super::PatternAdjacency;
use crate::common::{LabelId, TagId};
use crate::pattern::GraphPattern;

pub type VertexRankMap = HashMap<TagId, TagId>;
pub type EdgeRankMap = HashMap<TagId, TagId>;

pub fn canonicalize<P: GraphPattern>(pattern: &P) -> (VertexRankMap, EdgeRankMap) {
    let mut canon = Canonicalizer::new(pattern);
    canon.canonicalize();
    let vertex_rank_map: HashMap<_, _> = canon
        .vertex_rank_map
        .into_iter()
        .map(|(tag_id, rank)| (tag_id, rank.unwrap()))
        .collect();
    let edge_rank_map: HashMap<_, _> = canon
        .edge_rank_map
        .into_iter()
        .map(|(tag_id, rank)| (tag_id, rank.unwrap()))
        .collect();
    (vertex_rank_map, edge_rank_map)
}

struct Canonicalizer<'a, P> {
    pattern: &'a P,
    adjacencies_map: BTreeMap<TagId, Vec<PatternAdjacency>>,
    vertex_group_map: BTreeMap<TagId, TagId>,
    vertex_groups: BTreeMap<(LabelId, TagId), Vec<TagId>>,
    vertex_rank_map: BTreeMap<TagId, Option<TagId>>,
    edge_rank_map: BTreeMap<TagId, Option<TagId>>,
    has_converged: bool,
}

impl<'a, P: GraphPattern> Canonicalizer<'a, P> {
    fn new(pattern: &'a P) -> Self {
        let adjacencies_map = pattern
            .vertices()
            .iter()
            .map(|v| {
                (
                    v.tag_id(),
                    pattern
                        .adjacencies(v.tag_id())
                        .unwrap()
                        .copied()
                        .collect_vec(),
                )
            })
            .collect();
        let vertex_group_map = pattern.vertices().iter().map(|v| (v.tag_id(), 0)).collect();
        let vertex_groups = pattern
            .vertices()
            .iter()
            .sorted_unstable_by_key(|v| (v.label_id(), 0))
            .chunk_by(|v| (v.label_id(), 0))
            .into_iter()
            .map(|(key, group)| (key, group.map(|v| v.tag_id()).collect_vec()))
            .collect();
        let vertex_rank_map = pattern
            .vertices()
            .iter()
            .map(|v| (v.tag_id(), None))
            .collect();
        let edge_rank_map = pattern.edges().iter().map(|e| (e.tag_id(), None)).collect();
        let has_converged = false;
        let mut canon = Self {
            pattern,
            adjacencies_map,
            vertex_group_map,
            vertex_groups,
            vertex_rank_map,
            edge_rank_map,
            has_converged,
        };
        canon.sort_vertex_adjacencies();
        canon
    }

    fn sort_vertex_adjacencies(&mut self) {
        let mut adjacencies_map = mem::take(&mut self.adjacencies_map);
        for adjacencies in adjacencies_map.values_mut() {
            adjacencies.sort_unstable_by(|a1, a2| self.cmp_adjacency(a1, a2));
        }
        self.adjacencies_map = adjacencies_map;
    }

    fn refine_vertex_groups(&mut self) {
        let mut updated_vertex_group_map = BTreeMap::new();
        let mut updated_vertex_groups = BTreeMap::new();
        let mut has_converged = true;
        for (&(label_id, initial_group_id), vertex_group) in &self.vertex_groups {
            let mut vertex_group_tmp_vec = vec![initial_group_id; vertex_group.len()];
            for (i, &v1_tag_id) in vertex_group.iter().enumerate() {
                for (j, &v2_tag_id) in vertex_group.iter().enumerate().skip(i + 1) {
                    match self.cmp_vertex(v1_tag_id, v2_tag_id) {
                        Ordering::Greater => vertex_group_tmp_vec[i] += 1,
                        Ordering::Less => vertex_group_tmp_vec[j] += 1,
                        Ordering::Equal => (),
                    }
                }
                let v1_group_id = vertex_group_tmp_vec[i];
                if v1_group_id != initial_group_id {
                    has_converged = false;
                }
                updated_vertex_group_map.insert(v1_tag_id, v1_group_id);
                updated_vertex_groups
                    .entry((label_id, v1_group_id))
                    .and_modify(|vertex_group: &mut Vec<_>| vertex_group.push(v1_tag_id))
                    .or_insert(vec![v1_tag_id]);
            }
        }
        self.vertex_group_map = updated_vertex_group_map;
        self.vertex_groups = updated_vertex_groups;
        self.has_converged = has_converged;
        self.sort_vertex_adjacencies();
    }

    fn cmp_vertex(&self, v1_tag_id: TagId, v2_tag_id: TagId) -> Ordering {
        let v1_label_id = self.pattern.get_vertex(v1_tag_id).unwrap().label_id();
        let v2_label_id = self.pattern.get_vertex(v2_tag_id).unwrap().label_id();
        match v1_label_id.cmp(&v2_label_id) {
            Ordering::Equal => (),
            result => return result,
        }
        let v1_out_degree = self.pattern.outgoing_adjacencies(v1_tag_id).unwrap().len();
        let v2_out_degree = self.pattern.outgoing_adjacencies(v2_tag_id).unwrap().len();
        match v1_out_degree.cmp(&v2_out_degree) {
            Ordering::Equal => (),
            result => return result,
        }
        let v1_in_degree = self.pattern.incoming_adjacencies(v1_tag_id).unwrap().len();
        let v2_in_degree = self.pattern.incoming_adjacencies(v2_tag_id).unwrap().len();
        match v1_in_degree.cmp(&v2_in_degree) {
            Ordering::Equal => (),
            result => return result,
        }
        let v1_adj = self.adjacencies_map.get(&v1_tag_id).unwrap();
        let v2_adj = self.adjacencies_map.get(&v2_tag_id).unwrap();
        for (v1_adj, v2_adj) in v1_adj.iter().zip_eq(v2_adj) {
            match self.cmp_adjacency(v1_adj, v2_adj) {
                Ordering::Equal => (),
                result => return result,
            }
        }
        Ordering::Equal
    }

    fn cmp_adjacency(&self, a1: &PatternAdjacency, a2: &PatternAdjacency) -> Ordering {
        let a1_neighbor = self.pattern.get_vertex(a1.neighbor_tag_id()).unwrap();
        let a2_neighbor = self.pattern.get_vertex(a2.neighbor_tag_id()).unwrap();
        let a1_info = (a1.direction(), a1_neighbor.label_id(), a1.edge_label_id());
        let a2_info = (a2.direction(), a2_neighbor.label_id(), a2.edge_label_id());
        match a1_info.cmp(&a2_info) {
            Ordering::Equal => (),
            result => return result,
        }
        let a1_neighbor_tag_id = a1_neighbor.tag_id();
        let a2_neighbor_tag_id = a2_neighbor.tag_id();
        let a1_neighbor_group_id = self.vertex_group_map.get(&a1_neighbor_tag_id).unwrap();
        let a2_neighbor_group_id = self.vertex_group_map.get(&a2_neighbor_tag_id).unwrap();
        match a1_neighbor_group_id.cmp(a2_neighbor_group_id) {
            Ordering::Equal => (),
            result => return result,
        }
        let a1_neighbor_rank = self.vertex_rank_map.get(&a1_neighbor_tag_id).unwrap();
        let a2_neighbor_rank = self.vertex_rank_map.get(&a2_neighbor_tag_id).unwrap();
        a1_neighbor_rank.cmp(a2_neighbor_rank)
    }

    fn get_pattern_ranking_start_vertex(&self) -> Option<TagId> {
        let min_vertex_label_id = self.pattern.min_vertex_label_id()?;
        self.pattern
            .vertices()
            .iter()
            .filter(|v| v.label_id() == min_vertex_label_id)
            .map(|v| v.tag_id())
            .min_by(|v1_tag_id, v2_tag_id| {
                let v1_group = self.vertex_group_map.get(v1_tag_id).unwrap();
                let v2_group = self.vertex_group_map.get(v2_tag_id).unwrap();
                v1_group.cmp(v2_group)
            })
    }

    fn pattern_ranking_from_vertex(&mut self, tag_id: TagId) {
        let mut next_vertex_rank = 0;
        let mut next_edge_rank = 0;
        self.vertex_rank_map.insert(tag_id, Some(next_vertex_rank));
        next_vertex_rank += 1;
        let mut visited_edges = BTreeSet::new();
        let mut stack = self
            .adjacencies_map
            .get(&tag_id)
            .unwrap()
            .iter()
            .rev()
            .copied()
            .collect_vec();
        while let Some(adj) = stack.pop() {
            let edge_tag_id = adj.edge_tag_id();
            if visited_edges.contains(&edge_tag_id) {
                continue;
            }
            visited_edges.insert(edge_tag_id);
            self.edge_rank_map.insert(edge_tag_id, Some(next_edge_rank));
            next_edge_rank += 1;
            let neighbor_tag_id = adj.neighbor_tag_id();
            if self
                .vertex_rank_map
                .get(&neighbor_tag_id)
                .unwrap()
                .is_none()
            {
                self.vertex_rank_map
                    .insert(neighbor_tag_id, Some(next_vertex_rank));
                next_vertex_rank += 1;
            }
            self.sort_vertex_adjacencies();
            stack.extend(
                self.adjacencies_map
                    .get(&neighbor_tag_id)
                    .unwrap()
                    .iter()
                    .rev()
                    .filter(|adj| !visited_edges.contains(&adj.edge_tag_id)),
            );
        }
    }

    fn pattern_ranking(&mut self) {
        let start_vertex_tag_id = if let Some(tag_id) = self.get_pattern_ranking_start_vertex() {
            tag_id
        } else {
            return;
        };
        // Since we assume the pattern is connected, we can assign ranks to all vertices and edges
        // in a single iteration.
        self.pattern_ranking_from_vertex(start_vertex_tag_id);
        assert!(self.vertex_rank_map.values().all(Option::is_some));
        assert!(self.edge_rank_map.values().all(Option::is_some));
    }

    fn canonicalize(&mut self) {
        while !self.has_converged {
            self.refine_vertex_groups();
        }
        self.pattern_ranking();
    }
}
