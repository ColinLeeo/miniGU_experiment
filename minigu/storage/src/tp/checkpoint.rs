// checkpoint.rs
// Implementation of checkpoint mechanism for MemoryGraph
//
// This module provides functionality to create and restore checkpoints of a MemoryGraph.
// A checkpoint represents a consistent snapshot of the graph state at a specific point in time.
// It can be used for backup, recovery, or state transfer purposes.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use minigu_common::types::{EdgeId, VertexId};
use minigu_transaction::Timestamp;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::memory_graph::{AdjacencyContainer, MemoryGraph, VersionedEdge, VersionedVertex};
use crate::common::model::edge::{Edge, Neighbor};
use crate::common::model::vertex::Vertex;
use crate::error::StorageResult;

/// Represents a checkpoint of a MemoryGraph at a specific point in time.
///
/// A GraphCheckpoint contains:
/// 1. Metadata about the checkpoint (timestamp, LSN, etc.)
/// 2. Serialized vertices and edges
/// Adjacency list is rebuilt from edges during restore (kept for serialization compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCheckpoint {
    /// Metadata about the checkpoint
    pub metadata: CheckpointMetadata,

    /// Serialized vertices (current version only, no history)
    pub vertices: HashMap<VertexId, SerializedVertex>,

    /// Serialized edges (current version only, no history)
    pub edges: HashMap<EdgeId, SerializedEdge>,

    /// Legacy field — kept for backward-compatible deserialization of old checkpoints.
    /// New checkpoints write an empty map; restore always rebuilds from edges.
    #[serde(default)]
    adjacency_list: HashMap<VertexId, LegacyAdjacency>,
}

/// Legacy adjacency format for backward-compatible deserialization only.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyAdjacency {
    outgoing: Vec<(EdgeId, VertexId)>,
    incoming: Vec<(EdgeId, VertexId)>,
}

/// Metadata about a checkpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// Timestamp when the checkpoint was created
    pub timestamp: u64,

    /// Log sequence number (LSN) at the time of checkpoint
    pub lsn: u64,

    /// Latest commit timestamp at the time of checkpoint
    pub latest_commit_ts: u64,

    /// Checkpoint format version
    pub version: u32,
}

/// Serialized representation of a vertex
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedVertex {
    /// The vertex data
    pub data: Vertex,

    /// Commit timestamp of the vertex
    pub commit_ts: Timestamp,
}

/// Serialized representation of an edge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedEdge {
    /// The edge data
    pub data: Edge,

    /// Commit timestamp of the edge
    pub commit_ts: Timestamp,
}

impl GraphCheckpoint {
    /// Creates a new `GraphCheckpoint` from the current in-memory state of a [`MemoryGraph`].
    ///
    /// This method captures a consistent snapshot of the graph, including:
    /// - The metadata (timestamp, LSN, latest commit timestamp, etc.)
    /// - All vertices and edges (current version only)
    /// - The full adjacency list (both outgoing and incoming edges)
    ///
    /// # Arguments
    ///
    /// * `graph` - A reference-counted pointer to the in-memory [`MemoryGraph`] to be checkpointed.
    ///
    /// # Returns
    ///
    /// A fully materialized `GraphCheckpoint` containing the graph's current state.
    ///
    /// # Panics
    ///
    /// This function may panic if:
    /// - System time is earlier than UNIX_EPOCH (highly unlikely)
    /// - Lock poisoning occurs on internal vertex/edge RwLocks (only if previous panic occurred)
    pub fn new(graph: &Arc<MemoryGraph>) -> Self {
        // Get current LSN (not next_lsn - we don't want to consume an LSN for checkpoint)
        // The checkpoint represents the state "up to and including" the current LSN
        let lsn = graph.persistence.current_lsn();

        // Create metadata
        let metadata = CheckpointMetadata {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            lsn,
            latest_commit_ts: graph
                .txn_manager
                .latest_commit_ts
                .load(std::sync::atomic::Ordering::SeqCst),
            version: 1, // Initial version
        };

        // Serialize vertices
        let mut vertices = HashMap::new();
        for entry in graph.vertices.iter() {
            let versioned_vertex = entry.value();
            let current = versioned_vertex.chain.current.read().unwrap();

            vertices.insert(
                *entry.key(),
                SerializedVertex {
                    data: current.data.clone(),
                    commit_ts: current.commit_ts,
                },
            );
        }

        // Serialize edges
        let mut edges = HashMap::new();
        for entry in graph.edges.iter() {
            let versioned_edge = entry.value();
            let current = versioned_edge.chain.current.read().unwrap();

            edges.insert(
                *entry.key(),
                SerializedEdge {
                    data: current.data.clone(),
                    commit_ts: current.commit_ts,
                },
            );
        }

        Self {
            metadata,
            vertices,
            edges,
            adjacency_list: HashMap::new(),
        }
    }

    /// This method reconstructs an in-memory graph by replaying the serialized state
    /// stored in the checkpoint, including:
    /// - Metadata (log sequence number and latest commit timestamp)
    /// - All current vertices and edges (no historical versions)
    /// - The full adjacency list (outgoing/incoming connections)
    ///
    /// This method is typically used during system recovery, state rehydration,
    /// or startup bootstrapping from the latest persisted checkpoint.
    ///
    /// # Arguments
    ///
    /// * `checkpoint_config` - Configuration options for the graph's checkpoint behavior.
    /// * `wal_config` - Configuration for initializing the graph's write-ahead log (WAL) system.
    ///
    /// # Returns
    ///
    /// A fully reconstructed [`Arc<MemoryGraph>`] containing the state at the time of checkpoint
    /// creation.
    pub fn restore(self, graph: &Arc<MemoryGraph>) -> StorageResult<()> {
        // Set the next LSN to checkpoint LSN + 1
        graph.persistence.set_next_lsn(self.metadata.lsn + 1);

        // Set the latest commit timestamp
        graph.txn_manager.latest_commit_ts.store(
            self.metadata.latest_commit_ts,
            std::sync::atomic::Ordering::SeqCst,
        );

        let t0 = std::time::Instant::now();
        let vertex_count = self.vertices.len();
        let edge_count = self.edges.len();

        // Restore vertices in parallel (consume to avoid clone)
        let vertex_vec: Vec<_> = self.vertices.into_iter().collect();
        vertex_vec.into_par_iter().for_each(|(vid, sv)| {
            let label_id = sv.data.label_id;
            let commit_ts = sv.commit_ts;
            let versioned_vertex = VersionedVertex::new(sv.data);
            versioned_vertex.chain.current.write().unwrap().commit_ts = commit_ts;

            graph.vertices.insert(vid, versioned_vertex);

            graph
                .vertices_by_label
                .entry(label_id)
                .or_insert_with(|| RwLock::new(Vec::new()))
                .write()
                .unwrap()
                .push(vid);
        });

        eprintln!(
            "  checkpoint: restored {} vertices in {:.2}s",
            vertex_count,
            t0.elapsed().as_secs_f64()
        );
        let t1 = std::time::Instant::now();

        // Restore edges in parallel (consume to avoid clone)
        let edge_vec: Vec<_> = self.edges.into_iter().collect();
        edge_vec.into_par_iter().for_each(|(eid, se)| {
            let commit_ts = se.commit_ts;
            let versioned_edge = VersionedEdge::new(se.data);
            versioned_edge.chain.current.write().unwrap().commit_ts = commit_ts;

            graph.edges.insert(eid, versioned_edge);
        });

        eprintln!(
            "  checkpoint: restored {} edges in {:.2}s",
            edge_count,
            t1.elapsed().as_secs_f64()
        );
        let t2 = std::time::Instant::now();

        // Rebuild adjacency list from edges.
        // Strategy: group edges by vertex, then build each vertex's adjacency in parallel.
        // This avoids lock contention on the same SkipSet from multiple threads.
        let edge_tuples: Vec<_> = graph
            .edges
            .iter()
            .map(|entry| {
                let eid = *entry.key();
                let c = entry.value().chain.current.read().unwrap();
                (eid, c.data.label_id(), c.data.src_id, c.data.dst_id)
            })
            .collect();

        // Group by vertex: outgoing edges per src, incoming edges per dst
        let mut outgoing_map: HashMap<
            VertexId,
            Vec<(EdgeId, minigu_common::types::LabelId, VertexId)>,
        > = HashMap::new();
        let mut incoming_map: HashMap<
            VertexId,
            Vec<(EdgeId, minigu_common::types::LabelId, VertexId)>,
        > = HashMap::new();

        for &(eid, label_id, src_id, dst_id) in &edge_tuples {
            outgoing_map
                .entry(src_id)
                .or_default()
                .push((eid, label_id, dst_id));
            incoming_map
                .entry(dst_id)
                .or_default()
                .push((eid, label_id, src_id));
        }
        drop(edge_tuples);

        // Collect all unique vertex IDs that need adjacency entries
        let mut all_vids: Vec<VertexId> = outgoing_map
            .keys()
            .chain(incoming_map.keys())
            .copied()
            .collect();
        all_vids.sort_unstable();
        all_vids.dedup();

        // Build adjacency per vertex in parallel (no contention on the same SkipSet)
        all_vids.into_par_iter().for_each(|vid| {
            let container = AdjacencyContainer::new();

            if let Some(edges) = outgoing_map.get(&vid) {
                for &(eid, label_id, dst_id) in edges {
                    container
                        .outgoing()
                        .insert(Neighbor::new(label_id, dst_id, eid));
                }
            }

            if let Some(edges) = incoming_map.get(&vid) {
                for &(eid, label_id, src_id) in edges {
                    container
                        .incoming()
                        .insert(Neighbor::new(label_id, src_id, eid));
                }
            }

            graph.adjacency_list.insert(vid, container);
        });

        eprintln!(
            "  checkpoint: rebuilt adjacency in {:.2}s",
            t2.elapsed().as_secs_f64()
        );
        eprintln!(
            "  checkpoint: total restore time {:.2}s",
            t0.elapsed().as_secs_f64()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use minigu_common::types::VertexId;
    use minigu_common::value::ScalarValue;
    use minigu_transaction::{GraphTxnManager, IsolationLevel};

    use super::*;
    use crate::tp::memory_graph;

    #[test]
    fn test_checkpoint_creation() {
        // Create a graph with mock data
        let graph = memory_graph::tests::mock_graph();

        // Create checkpoint
        let checkpoint = GraphCheckpoint::new(&graph);

        // Verify checkpoint contents
        assert!(checkpoint.vertices.len() == 4);
        assert!(checkpoint.edges.len() == 4);

        let alice_vid: VertexId = VertexId::from(1u64);
        // Verify vertex data
        let alice_serialized = checkpoint.vertices.get(&alice_vid).unwrap();
        assert_eq!(alice_serialized.data.vid(), alice_vid);
        assert_eq!(
            alice_serialized.data.properties()[0],
            ScalarValue::String(Some("Alice".to_string()))
        );
    }

    #[test]
    fn test_checkpoint_restore() {
        // Create a graph with mock data
        let original_graph = memory_graph::tests::mock_graph();

        // Create checkpoint
        let checkpoint = GraphCheckpoint::new(&original_graph);

        // Create a new empty graph to restore into
        let restored_graph = memory_graph::tests::mock_empty_graph();

        // Restore graph from checkpoint
        checkpoint.restore(&restored_graph).unwrap();
        // Re-create checkpoint for comparison (original was consumed)
        let _checkpoint = GraphCheckpoint::new(&original_graph);

        let origin_txn = original_graph
            .txn_manager()
            .begin_transaction(IsolationLevel::Serializable)
            .unwrap();
        let restore_txn = restored_graph
            .txn_manager()
            .begin_transaction(IsolationLevel::Serializable)
            .unwrap();

        // Check vertices
        let original_alice = original_graph
            .get_vertex(&origin_txn, VertexId::from(1u64))
            .unwrap();
        let restored_alice = restored_graph
            .get_vertex(&restore_txn, VertexId::from(1u64))
            .unwrap();
        assert_eq!(original_alice.vid(), restored_alice.vid());
        assert_eq!(original_alice.properties(), restored_alice.properties());

        let original_bob = original_graph
            .get_vertex(&origin_txn, VertexId::from(2u64))
            .unwrap();
        let restored_bob = restored_graph
            .get_vertex(&restore_txn, VertexId::from(2u64))
            .unwrap();
        assert_eq!(original_bob.vid(), restored_bob.vid());
        assert_eq!(original_bob.properties(), restored_bob.properties());

        // Check edges
        let original_friend_edge = original_graph
            .get_edge(&origin_txn, VertexId::from(1u64))
            .unwrap();
        let restored_friend_edge = restored_graph
            .get_edge(&restore_txn, VertexId::from(1u64))
            .unwrap();
        assert_eq!(original_friend_edge.eid(), restored_friend_edge.eid());
        assert_eq!(
            original_friend_edge.properties(),
            restored_friend_edge.properties()
        );

        let original_follow_edge = original_graph
            .get_edge(&origin_txn, VertexId::from(3u64))
            .unwrap();
        let restored_follow_edge = restored_graph
            .get_edge(&restore_txn, VertexId::from(3u64))
            .unwrap();
        assert_eq!(original_follow_edge.eid(), restored_follow_edge.eid());
        assert_eq!(
            original_follow_edge.properties(),
            restored_follow_edge.properties()
        );

        // Check adjacency list
        let original_alice_adj = original_graph
            .adjacency_list
            .get(&VertexId::from(1u64))
            .unwrap();
        let restored_alice_adj = restored_graph
            .adjacency_list
            .get(&VertexId::from(1u64))
            .unwrap();
        assert_eq!(
            original_alice_adj.outgoing.len(),
            restored_alice_adj.outgoing.len()
        );
        assert_eq!(
            original_alice_adj.incoming.len(),
            restored_alice_adj.incoming.len()
        );
    }
}
