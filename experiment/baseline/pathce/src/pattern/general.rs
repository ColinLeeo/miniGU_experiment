use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{GraphPattern, PatternAdjacency, PatternEdge, PatternVertex};
use crate::common::TagId;
use crate::pattern::RawPattern;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPattern")]
#[serde(into = "RawPattern")]
pub struct GeneralPattern {
    pub(super) vertices: Vec<PatternVertex>,
    pub(super) edges: Vec<PatternEdge>,
    pub(super) tag_vertex_map: HashMap<TagId, usize>,
    pub(super) tag_edge_map: HashMap<TagId, usize>,
    pub(super) outgoing_adjacencies: HashMap<TagId, Vec<PatternAdjacency>>,
    pub(super) incoming_adjacencies: HashMap<TagId, Vec<PatternAdjacency>>,
    pub(super) vertex_rank_map: HashMap<TagId, TagId>,
    pub(super) edge_rank_map: HashMap<TagId, TagId>,
    pub(super) rank_vertex_map: HashMap<TagId, TagId>,
    pub(super) rank_edge_map: HashMap<TagId, TagId>,
}

impl GraphPattern for GeneralPattern {
    fn vertices(&self) -> &[PatternVertex] {
        &self.vertices
    }

    fn edges(&self) -> &[PatternEdge] {
        &self.edges
    }

    fn get_vertex(&self, tag_id: TagId) -> Option<PatternVertex> {
        self.tag_vertex_map
            .get(&tag_id)
            .map(|idx| self.vertices[*idx])
    }

    fn get_vertex_rank(&self, tag_id: TagId) -> Option<TagId> {
        self.vertex_rank_map.get(&tag_id).copied()
    }

    fn get_vertex_from_rank(&self, rank: TagId) -> Option<PatternVertex> {
        let tag_id = self.rank_vertex_map.get(&rank)?;
        self.get_vertex(*tag_id)
    }

    fn get_edge(&self, tag_id: TagId) -> Option<PatternEdge> {
        self.tag_edge_map.get(&tag_id).map(|idx| self.edges[*idx])
    }

    fn get_edge_rank(&self, tag_id: TagId) -> Option<TagId> {
        self.edge_rank_map.get(&tag_id).copied()
    }

    fn get_edge_from_rank(&self, rank: TagId) -> Option<PatternEdge> {
        let tag_id = self.rank_edge_map.get(&rank)?;
        self.get_edge(*tag_id)
    }

    fn outgoing_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]> {
        self.outgoing_adjacencies.get(&tag_id).map(Vec::as_ref)
    }

    fn incoming_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]> {
        self.incoming_adjacencies.get(&tag_id).map(Vec::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::raw::RawPattern;

    #[test]
    fn test_serde_json() {
        let p = RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 3))
            .push_back_edge((1, 1, 2, 4))
            .to_general()
            .unwrap();
        let expected = "{\"vertices\":[{\"tag_id\":0,\"label_id\":1},{\"tag_id\":1,\"label_id\":1},{\"tag_id\":2,\"label_id\":2}],\"edges\":[{\"tag_id\":0,\"src\":0,\"dst\":1,\"label_id\":3},{\"tag_id\":1,\"src\":1,\"dst\":2,\"label_id\":4}]}";
        assert_eq!(serde_json::to_string(&p).unwrap(), expected);

        let p_new: GeneralPattern = serde_json::from_str(expected).unwrap();
        assert_eq!(p, p_new);
    }

    #[test]
    fn test_encode() {
        let p1 = RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 3))
            .push_back_edge((1, 1, 2, 4))
            .to_general()
            .unwrap();
        let p2 = RawPattern::new()
            .push_back_vertex((2, 1))
            .push_back_vertex((0, 2))
            .push_back_vertex((1, 1))
            .push_back_edge((1, 2, 1, 3))
            .push_back_edge((0, 1, 0, 4))
            .to_general()
            .unwrap();
        assert_eq!(p1.encode(), p2.encode());
    }
}
