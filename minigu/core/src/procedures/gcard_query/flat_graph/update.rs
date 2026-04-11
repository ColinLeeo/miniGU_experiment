use std::collections::HashSet;

use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;

/// Full metadata for a pending edge insertion.
#[derive(Debug, Clone)]
pub struct PendingEdge {
    pub edge_id: EdgeId,
    pub src: VertexId,
    pub src_label: String,
    pub dst: VertexId,
    pub dst_label: String,
    pub edge_label: String,
    pub props: Vec<ScalarValue>,
}

/// Pending structural changes accumulated before [`super::FlatGraph::apply_pending`].
///
/// Changes recorded here are immediately reflected in
/// [`super::FlatGraph::neighbors_for_compact`] (deleted items are hidden, inserted items
/// are visible) so that the GCard update algorithm can propagate deltas over the
/// up-to-date graph topology during compaction.
#[derive(Debug, Default)]
pub struct PendingChanges {
    /// Vertices to be added to the graph.
    pub inserted_vertices: Vec<(VertexId, String, Vec<ScalarValue>)>,
    /// Vertices whose statistics and adjacency entries should be removed.
    pub deleted_vertices: HashSet<VertexId>,
    /// Edges to be added to the graph.
    pub inserted_edges: Vec<PendingEdge>,
    /// Edge IDs to be removed from the graph.
    pub deleted_edge_ids: HashSet<EdgeId>,
}

impl PendingChanges {
    /// Returns `true` if there are no pending changes.
    pub fn is_empty(&self) -> bool {
        self.inserted_vertices.is_empty()
            && self.deleted_vertices.is_empty()
            && self.inserted_edges.is_empty()
            && self.deleted_edge_ids.is_empty()
    }

    /// Discard all pending changes without applying them.
    pub fn clear(&mut self) {
        self.inserted_vertices.clear();
        self.deleted_vertices.clear();
        self.inserted_edges.clear();
        self.deleted_edge_ids.clear();
    }
}
