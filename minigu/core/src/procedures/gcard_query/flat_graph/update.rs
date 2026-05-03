//! `FlatGraph` 的 pending 变更缓冲区。
//!
//! 设计重点是：
//! 1. 先记录变更，不立即重建 CSR；
//! 2. 但在 compact/update 期间，这些变更又必须“立刻可见”。
//! 所以这里额外维护了一套快速邻居索引。
// 这里变更操作成本会非常的高， 因为内存是连续的，任何一个变更其实会导致数据重写。
use std::collections::{HashMap, HashSet};

use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use serde::{Deserialize, Serialize};

/// Full metadata for a pending edge insertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PendingChanges {
    pub inserted_vertices: Vec<(VertexId, String, Vec<ScalarValue>)>,
    pub deleted_vertices: HashSet<VertexId>,
    pub inserted_edges: Vec<PendingEdge>,
    pub deleted_edge_ids: HashSet<EdgeId>,

    // ── Indexes for fast neighbor lookup on pending edges ─────────────────
    /// (src_vid, edge_label) → [dst_vid, ...]  (outgoing pending edges)
    pub pending_out: HashMap<(VertexId, String), Vec<VertexId>>,
    /// (dst_vid, edge_label) → [src_vid, ...]  (incoming pending edges)
    pub pending_in: HashMap<(VertexId, String), Vec<VertexId>>,
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
        self.pending_out.clear();
        self.pending_in.clear();
    }

    /// Record a pending edge insertion and update the neighbor indexes.
    pub fn insert_edge(&mut self, pe: PendingEdge) {
        self.pending_out
            .entry((pe.src, pe.edge_label.clone()))
            .or_default()
            .push(pe.dst);
        self.pending_in
            .entry((pe.dst, pe.edge_label.clone()))
            .or_default()
            .push(pe.src);
        self.inserted_edges.push(pe);
    }
}
