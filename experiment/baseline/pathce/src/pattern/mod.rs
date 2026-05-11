mod canonical;
mod general;
mod path;
mod raw;

use std::cmp::Ordering;
use std::fmt::Debug;

use bytes::BufMut;
pub use canonical::*;
pub use general::*;
use itertools::Itertools;
pub use path::*;
pub use raw::*;
use serde::{Deserialize, Serialize};

use crate::common::{EdgeDirection, LabelId, TagId};

const EDGE_ENCODING_LENGTH: usize = 14;

pub fn encode_vertex(vertex_label_id: LabelId) -> Vec<u8> {
    Vec::from(vertex_label_id.to_le_bytes())
}

pub fn encode_edge(
    src_label_id: LabelId,
    dst_label_id: LabelId,
    edge_label_id: LabelId,
) -> Vec<u8> {
    let mut code = Vec::with_capacity(EDGE_ENCODING_LENGTH);
    code.put_u32(edge_label_id);
    code.put_u32(src_label_id);
    code.put_u32(dst_label_id);
    let (src_rank, dst_rank) = match src_label_id.cmp(&dst_label_id) {
        Ordering::Less => (0, 1),
        _ => (1, 0),
    };
    code.put_u8(src_rank);
    code.put_u8(dst_rank);
    code
}

fn encode_normal<P: GraphPattern>(pattern: &P) -> Vec<u8> {
    let mut code = Vec::with_capacity(pattern.edges().len() * EDGE_ENCODING_LENGTH);
    pattern
        .edges()
        .iter()
        .map(|e| (*e, pattern.get_edge_rank(e.tag_id).unwrap()))
        .sorted_unstable_by_key(|(_, rank)| *rank)
        .for_each(|(e, _)| {
            let edge_label_id = e.label_id;
            let src_tag_id = e.src;
            let src_rank = pattern.get_vertex_rank(src_tag_id).unwrap();
            let src_label_id = pattern.get_vertex(src_tag_id).unwrap().label_id;

            let dst_tag_id = e.dst;
            let dst_rank = pattern.get_vertex_rank(dst_tag_id).unwrap();
            let dst_label_id = pattern.get_vertex(dst_tag_id).unwrap().label_id;

            code.put_u32(edge_label_id);
            code.put_u32(src_label_id);
            code.put_u32(dst_label_id);
            code.put_u8(src_rank);
            code.put_u8(dst_rank);
        });
    code
}

pub trait GraphPattern: Debug + Clone {
    fn vertices(&self) -> &[PatternVertex];
    fn edges(&self) -> &[PatternEdge];

    fn get_vertex(&self, tag_id: TagId) -> Option<PatternVertex>;
    fn get_vertex_rank(&self, tag_id: TagId) -> Option<TagId>;
    fn get_vertex_from_rank(&self, rank: TagId) -> Option<PatternVertex>;

    fn get_edge(&self, tag_id: TagId) -> Option<PatternEdge>;
    fn get_edge_rank(&self, tag_id: TagId) -> Option<TagId>;
    fn get_edge_from_rank(&self, rank: TagId) -> Option<PatternEdge>;

    fn outgoing_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]>;
    fn incoming_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]>;

    fn adjacencies(&self, tag_id: TagId) -> Option<impl Iterator<Item = &PatternAdjacency>> {
        Some(
            self.outgoing_adjacencies(tag_id)?
                .iter()
                .chain(self.incoming_adjacencies(tag_id)?),
        )
    }

    fn get_vertex_out_degree(&self, tag_id: TagId) -> Option<usize> {
        Some(self.outgoing_adjacencies(tag_id)?.len())
    }

    fn get_vertex_in_degree(&self, tag_id: TagId) -> Option<usize> {
        Some(self.incoming_adjacencies(tag_id)?.len())
    }

    fn get_vertex_degree(&self, tag_id: TagId) -> Option<usize> {
        Some(self.get_vertex_in_degree(tag_id)? + self.get_vertex_out_degree(tag_id)?)
    }

    fn min_vertex_label_id(&self) -> Option<LabelId> {
        self.vertices().iter().map(|v| v.label_id).min()
    }

    fn max_vertex_label_id(&self) -> Option<LabelId> {
        self.vertices().iter().map(|v| v.label_id).max()
    }

    fn min_vertex_tag_id(&self) -> Option<TagId> {
        self.vertices().iter().map(|v| v.tag_id).min()
    }

    fn max_vertex_tag_id(&self) -> Option<TagId> {
        self.vertices().iter().map(|v| v.tag_id).max()
    }

    fn min_edge_label_id(&self) -> Option<LabelId> {
        self.edges().iter().map(|e| e.label_id).min()
    }

    fn max_edge_label_id(&self) -> Option<LabelId> {
        self.edges().iter().map(|e| e.label_id).max()
    }

    fn min_edge_tag_id(&self) -> Option<TagId> {
        self.edges().iter().map(|e| e.tag_id).min()
    }

    fn max_edge_tag_id(&self) -> Option<TagId> {
        self.edges().iter().map(|e| e.tag_id).max()
    }

    fn encode(&self) -> Vec<u8> {
        match self.edges().len() {
            0 if self.vertices().is_empty() => vec![],
            0 if self.vertices().len() == 1 => {
                encode_vertex(self.vertices().first().unwrap().label_id)
            }
            0 => unreachable!(),
            1 => {
                let e = self.edges().first().unwrap();
                let src_label_id = self.get_vertex(e.src).unwrap().label_id;
                let dst_label_id = self.get_vertex(e.dst).unwrap().label_id;
                encode_edge(src_label_id, dst_label_id, e.label_id)
            }
            _ => encode_normal(self),
        }
    }

    fn is_cyclic(&self) -> bool {
        // Since the pattern must be connected, we can just use the condition `|E| > |V| - 1`.
        if self.vertices().is_empty() {
            false
        } else {
            self.edges().len() > self.vertices().len() - 1
        }
    }

    fn is_cycle(&self) -> bool {
        if self.vertices().is_empty() {
            return false;
        }
        self.vertices()
            .iter()
            .all(|v| self.get_vertex_degree(v.tag_id()).unwrap() == 2)
    }

    fn is_path(&self) -> bool {
        if self.vertices().is_empty() {
            return false;
        }
        let mut deg1_count = 0;
        let mut deg2_count = 0;
        for v in self.vertices() {
            let deg = self.get_vertex_degree(v.tag_id()).unwrap();
            match deg {
                0 => return self.vertices().len() == 1 && self.edges().is_empty(),
                1 => deg1_count += 1,
                2 => deg2_count += 1,
                _ => return false,
            }
        }
        deg1_count == 2 && deg1_count + deg2_count == self.vertices().len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PatternVertex {
    tag_id: TagId,
    label_id: LabelId,
}

impl From<(TagId, LabelId)> for PatternVertex {
    fn from((tag_id, label_id): (TagId, LabelId)) -> Self {
        Self { tag_id, label_id }
    }
}

impl PatternVertex {
    pub fn new(tag_id: TagId, label_id: LabelId) -> Self {
        Self { tag_id, label_id }
    }

    pub fn tag_id(self) -> TagId {
        self.tag_id
    }

    pub fn label_id(self) -> LabelId {
        self.label_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PatternEdge {
    tag_id: TagId,
    src: TagId,
    dst: TagId,
    label_id: LabelId,
}

impl From<(TagId, TagId, TagId, LabelId)> for PatternEdge {
    fn from((tag_id, src, dst, label_id): (TagId, TagId, TagId, LabelId)) -> Self {
        Self {
            tag_id,
            src,
            dst,
            label_id,
        }
    }
}

impl PatternEdge {
    pub fn new(tag_id: TagId, src: TagId, dst: TagId, label_id: LabelId) -> Self {
        Self {
            tag_id,
            src,
            dst,
            label_id,
        }
    }

    pub fn tag_id(self) -> TagId {
        self.tag_id
    }

    pub fn src(self) -> TagId {
        self.src
    }

    pub fn dst(self) -> TagId {
        self.dst
    }

    pub fn label_id(self) -> LabelId {
        self.label_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PatternAdjacency {
    edge_tag_id: TagId,
    edge_label_id: LabelId,
    neighbor_tag_id: TagId,
    direction: EdgeDirection,
}

impl PatternAdjacency {
    pub fn edge_tag_id(self) -> TagId {
        self.edge_tag_id
    }

    pub fn edge_label_id(self) -> LabelId {
        self.edge_label_id
    }

    pub fn neighbor_tag_id(self) -> TagId {
        self.neighbor_tag_id
    }

    pub fn direction(self) -> EdgeDirection {
        self.direction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_edge() {
        let p = RawPattern::new()
            .push_back_vertex((3, 123))
            .push_back_vertex((4, 124))
            .push_back_edge((0, 3, 4, 7))
            .to_general()
            .unwrap();
        let code1 = encode_normal(&p);
        let code2 = encode_edge(123, 124, 7);
        assert_eq!(code1, code2);

        let p = RawPattern::new()
            .push_back_vertex((3, 125))
            .push_back_vertex((4, 124))
            .push_back_edge((0, 3, 4, 7))
            .to_general()
            .unwrap();
        let code1 = encode_normal(&p);
        let code2 = encode_edge(125, 124, 7);
        assert_eq!(code1, code2);

        let p = RawPattern::new()
            .push_back_vertex((3, 123))
            .push_back_vertex((4, 123))
            .push_back_edge((0, 3, 4, 7))
            .to_general()
            .unwrap();
        let code1 = encode_normal(&p);
        let code2 = encode_edge(123, 123, 7);
        assert_eq!(code1, code2)
    }

    #[test]
    fn test_is_path() {
        let edges: [PatternEdge; 0] = [];
        let p1 = RawPattern::with_vertices_edges([(0, 1)], edges)
            .to_general()
            .unwrap();
        assert!(p1.is_path());

        let p2 = RawPattern::with_vertices_edges([(0, 1), (1, 1)], [(0, 0, 1, 0)])
            .to_general()
            .unwrap();
        assert!(p2.is_path());

        let p3 = RawPattern::with_vertices_edges(
            [(0, 1), (1, 1), (2, 1), (3, 1)],
            [(0, 0, 1, 1), (1, 0, 2, 1), (2, 0, 3, 1)],
        )
        .to_general()
        .unwrap();
        assert!(!p3.is_path());
    }
}
