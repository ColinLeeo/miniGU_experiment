use minigu_common::types::{EdgeId, VertexId};
use serde::{Deserialize, Serialize};

/// Compressed Sparse Row adjacency with packed `(neighbor_vid, edge_id)` pairs.
///
/// Immutable after construction.  Binary-search on `verts` gives O(log n) vertex
/// lookup; the returned neighbor slice is zero-copy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CsrAdjWithEid {
    /// Sorted source vertex IDs.
    pub(super) verts: Vec<VertexId>,
    /// CSR offsets: entry i covers `neighbors[offsets[i]..offsets[i+1]]`.
    pub(super) offsets: Vec<usize>,
    /// Packed `(neighbor_vid, edge_id)` pairs, grouped by source vertex.
    pub(super) neighbors: Vec<(VertexId, EdgeId)>,
}

impl CsrAdjWithEid {
    pub fn build(mut triples: Vec<(VertexId, VertexId, EdgeId)>) -> Self {
        if triples.is_empty() {
            return Self::default();
        }
        triples.sort_unstable_by_key(|&(src, _, _)| src);

        let mut verts: Vec<VertexId> = Vec::new();
        let mut offsets: Vec<usize> = Vec::new();
        let mut neighbors: Vec<(VertexId, EdgeId)> = Vec::with_capacity(triples.len());

        for (src, dst, eid) in triples {
            if verts.last().copied() != Some(src) {
                verts.push(src);
                offsets.push(neighbors.len());
            }
            neighbors.push((dst, eid));
        }
        offsets.push(neighbors.len()); // sentinel

        Self {
            verts,
            offsets,
            neighbors,
        }
    }

    #[inline]
    pub fn neighbors_slice(&self, vid: VertexId) -> &[(VertexId, EdgeId)] {
        match self.verts.binary_search(&vid) {
            Ok(pos) => &self.neighbors[self.offsets[pos]..self.offsets[pos + 1]],
            Err(_) => &[],
        }
    }

    /// Returns `true` if `vid` has at least one outgoing entry in this CSR.
    #[inline]
    pub fn contains_vertex(&self, vid: VertexId) -> bool {
        self.verts.binary_search(&vid).is_ok()
    }

    /// Number of source vertices tracked.
    #[inline]
    pub fn vertex_count(&self) -> usize {
        self.verts.len()
    }

    /// Total number of (neighbor, edge) entries stored.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.neighbors.len()
    }

    /// Sorted source vertex IDs.
    #[inline]
    pub fn verts(&self) -> &[VertexId] {
        &self.verts
    }

    /// CSR offsets.
    #[inline]
    pub fn offsets(&self) -> &[usize] {
        &self.offsets
    }

    /// Packed `(neighbor_vid, edge_id)` entries in CSR order.
    #[inline]
    pub fn neighbor_entries(&self) -> &[(VertexId, EdgeId)] {
        &self.neighbors
    }

    /// Extract only the neighbor vertex IDs (dropping edge IDs).
    pub fn neighbors_vids(&self) -> Vec<VertexId> {
        self.neighbors.iter().map(|&(vid, _)| vid).collect()
    }
}
