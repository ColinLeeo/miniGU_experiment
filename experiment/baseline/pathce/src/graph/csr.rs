use itertools::Itertools;
use serde::{Deserialize, Serialize};

use super::InternalId;
use crate::common::DefaultVertexId;
use crate::error::{GCardError, GCardResult};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Csr {
    offsets: Vec<usize>,
    neighbors: Vec<DefaultVertexId>,
}

impl Csr {
    pub fn get_num_neighbors(&self) -> usize {
        self.neighbors.len()
    }

    pub fn neighbors(&self, vertex_id: InternalId) -> &[DefaultVertexId] {
        let max_vertex_id = self.offsets.len() as u32 - 2;
        if vertex_id > max_vertex_id {
            return &[];
        }
        let start = self.offsets[vertex_id as usize];
        let end = self.offsets[vertex_id as usize + 1];
        &self.neighbors[start..end]
    }

    pub fn from_sorted_edges(
        max_vertex_id: InternalId,
        edges: &[(InternalId, DefaultVertexId)],
    ) -> GCardResult<Self> {
        let mut offsets = vec![0; max_vertex_id as usize + 2];
        let neighbors = edges.iter().map(|(_, neighbor)| *neighbor).collect();

        let mut current_vertex_id = 0;
        let mut current_offset = 0;

        for (src, neighbors) in &edges.iter().chunk_by(|(src, _)| *src) {
            if src < current_vertex_id {
                return Err(GCardError::Graph("edges are not sorted".into()));
            }
            if src > max_vertex_id {
                let err = format!("vertex id {src} exceeds max vertex id {max_vertex_id}");
                return Err(GCardError::Graph(err));
            }
            for vertex_id in current_vertex_id..=src {
                offsets[vertex_id as usize] = current_offset;
            }
            current_vertex_id = src + 1;
            current_offset += neighbors.count();
        }
        offsets
            .iter_mut()
            .skip(current_vertex_id as _)
            .for_each(|offset| *offset = current_offset);
        Ok(Self { offsets, neighbors })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BidirectionalCsr {
    forward: Csr,
    backward: Csr,
}

impl BidirectionalCsr {
    pub fn new(forward: Csr, backward: Csr) -> Self {
        Self { forward, backward }
    }

    pub fn get_num_edges(&self) -> usize {
        self.forward.get_num_neighbors()
    }

    pub fn outgoing_neighbors(&self, vertex_id: InternalId) -> &[DefaultVertexId] {
        self.forward.neighbors(vertex_id)
    }

    pub fn incoming_neighbors(&self, vertex_id: InternalId) -> &[DefaultVertexId] {
        self.backward.neighbors(vertex_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csr() {
        let csr = Csr::from_sorted_edges(6, &[(3, 1), (3, 2), (5, 1)]).unwrap();
        let expected = Csr {
            offsets: vec![0, 0, 0, 0, 2, 2, 3, 3],
            neighbors: vec![1, 2, 1],
        };
        assert_eq!(csr, expected);

        assert_eq!(csr.neighbors(3), &[1, 2]);
        assert!(csr.neighbors(4).is_empty());
    }
}
