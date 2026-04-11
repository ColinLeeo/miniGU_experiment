//! Standalone, read-optimised graph store for GCard sampling and update propagation.
//!
//! [`FlatGraph`] is a pure in-memory graph with no MVCC, no transactions, and no
//! locking.  It replaces `MemoryGraph + MemTransaction` in two hot paths:
//!
//! 1. **Wander Join / predicate sampling** — fast label-indexed vertex sampling, zero-copy CSR
//!    neighbor slices, and direct property access.
//! 2. **GCard update-log compaction** — `neighbors_for_compact` merges the immutable CSR base with
//!    pending inserts/deletes so the update algorithm traverses the correct topology without an
//!    open transaction.
//!
//! # Lifecycle
//!
//! ```text
//! FlatGraph::build(...)           // one-time construction
//!     ↓
//! record_insert/delete_*()        // batch structural changes
//!     ↓
//! GCardUpdateLog::compact_and_apply_flat(flat_graph, ...)   // propagate deltas
//!     ↓
//! apply_pending()                 // rebuild affected CSR buckets
//!     ↓
//! Wander Join / gcard_query       // query using updated graph
//! ```

pub mod csr;
pub mod update;

use std::collections::{HashMap, HashSet};

use csr::CsrAdjWithEid;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use rand::Rng;
use rand::seq::SliceRandom;
use update::{PendingChanges, PendingEdge};

// ── Reverse-lookup metadata stored per edge ───────────────────────────────────

#[derive(Debug, Clone)]
struct EdgeInfo {
    src: VertexId,
    src_label: String,
    dst: VertexId,
    dst_label: String,
    edge_label: String,
}

// ── Main struct ───────────────────────────────────────────────────────────────

/// Standalone, read-optimised graph for GCard.
///
/// No `minigu_storage` dependency.  Thread-safe for concurrent reads once
/// constructed (all fields are immutable except `pending`).
pub struct FlatGraph {
    // ── Topology ──────────────────────────────────────────────────────────────
    /// label_name → sorted vertex IDs.
    vertices_by_label: HashMap<String, Vec<VertexId>>,
    /// vertex_id → label_name.
    vertex_label_map: HashMap<VertexId, String>,
    /// `(src_vertex_label, edge_label, outgoing)` → CSR adjacency with edge IDs.
    hop_csrs: HashMap<(String, String, bool), CsrAdjWithEid>,

    // ── Properties ────────────────────────────────────────────────────────────
    /// vertex_id → property values indexed by schema position.
    vertex_props: HashMap<VertexId, Vec<ScalarValue>>,
    /// label_name → ordered property names (position = column index).
    vertex_prop_schema: HashMap<String, Vec<String>>,
    /// edge_id → property values indexed by schema position.
    edge_props: HashMap<EdgeId, Vec<ScalarValue>>,
    /// edge_label_name → ordered property names.
    edge_prop_schema: HashMap<String, Vec<String>>,

    // ── Edge type schema ──────────────────────────────────────────────────────
    /// edge_label → (src_vertex_label, dst_vertex_label).
    /// Used to enumerate edge extensions for the update-log compact phase.
    edge_type_schema: HashMap<String, (String, String)>,

    // ── Reverse edge lookup ───────────────────────────────────────────────────
    /// edge_id → full endpoint+label metadata.
    /// Required to identify which CSR buckets are affected when an edge is deleted.
    edge_info: HashMap<EdgeId, EdgeInfo>,

    // ── Pending structural changes ─────────────────────────────────────────────
    pending: PendingChanges,
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Incremental builder for [`FlatGraph`].
///
/// Accumulates raw vertices, edges, and schema information, then assembles
/// all CSR buckets in one pass on [`build`](FlatGraphBuilder::build).
#[derive(Default)]
pub struct FlatGraphBuilder {
    vertices_by_label: HashMap<String, Vec<VertexId>>,
    vertex_label_map: HashMap<VertexId, String>,
    vertex_props: HashMap<VertexId, Vec<ScalarValue>>,
    vertex_prop_schema: HashMap<String, Vec<String>>,
    edge_props: HashMap<EdgeId, Vec<ScalarValue>>,
    edge_prop_schema: HashMap<String, Vec<String>>,
    edge_type_schema: HashMap<String, (String, String)>,
    edge_info: HashMap<EdgeId, EdgeInfo>,
    /// (src_label, edge_label, outgoing) → [(src_vid, dst_vid, edge_id)]
    edge_triples: HashMap<(String, String, bool), Vec<(VertexId, VertexId, EdgeId)>>,
}

impl FlatGraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare the property schema for a vertex label (column order matters).
    pub fn set_vertex_prop_schema(&mut self, label: &str, prop_names: Vec<String>) {
        self.vertex_prop_schema
            .insert(label.to_string(), prop_names);
    }

    /// Declare the property schema for an edge label.
    pub fn set_edge_prop_schema(&mut self, edge_label: &str, prop_names: Vec<String>) {
        self.edge_prop_schema
            .insert(edge_label.to_string(), prop_names);
    }

    /// Register an edge type: which src/dst vertex labels it connects.
    pub fn set_edge_type_schema(&mut self, edge_label: &str, src_label: &str, dst_label: &str) {
        self.edge_type_schema.insert(
            edge_label.to_string(),
            (src_label.to_string(), dst_label.to_string()),
        );
    }

    /// Add a vertex.
    pub fn add_vertex(&mut self, vid: VertexId, label: &str, props: Vec<ScalarValue>) {
        let label_lc = label.to_lowercase();
        self.vertices_by_label
            .entry(label_lc.clone())
            .or_default()
            .push(vid);
        self.vertex_label_map.insert(vid, label_lc);
        if !props.is_empty() {
            self.vertex_props.insert(vid, props);
        }
    }

    /// Add a directed edge.
    pub fn add_edge(
        &mut self,
        eid: EdgeId,
        src: VertexId,
        src_label: &str,
        dst: VertexId,
        dst_label: &str,
        edge_label: &str,
        props: Vec<ScalarValue>,
    ) {
        let src_lc = src_label.to_lowercase();
        let dst_lc = dst_label.to_lowercase();
        let el_lc = edge_label.to_lowercase();

        // Outgoing CSR bucket: (src_label, edge_label, true)
        self.edge_triples
            .entry((src_lc.clone(), el_lc.clone(), true))
            .or_default()
            .push((src, dst, eid));

        // Incoming CSR bucket: (dst_label, edge_label, false)
        self.edge_triples
            .entry((dst_lc.clone(), el_lc.clone(), false))
            .or_default()
            .push((dst, src, eid));

        self.edge_info.insert(
            eid,
            EdgeInfo {
                src,
                src_label: src_lc,
                dst,
                dst_label: dst_lc,
                edge_label: el_lc,
            },
        );
        if !props.is_empty() {
            self.edge_props.insert(eid, props);
        }
    }

    /// Consume the builder and produce a [`FlatGraph`].
    ///
    /// Sorts vertex ID lists and constructs all CSR adjacency structures.
    pub fn build(mut self) -> FlatGraph {
        // Sort vertex lists for binary-search and consistent sampling.
        for vids in self.vertices_by_label.values_mut() {
            vids.sort_unstable();
            vids.dedup();
        }

        // Build CSR for each bucket.
        let hop_csrs: HashMap<(String, String, bool), CsrAdjWithEid> = self
            .edge_triples
            .into_iter()
            .map(|(key, triples)| (key, CsrAdjWithEid::build(triples)))
            .collect();

        FlatGraph {
            vertices_by_label: self.vertices_by_label,
            vertex_label_map: self.vertex_label_map,
            hop_csrs,
            vertex_props: self.vertex_props,
            vertex_prop_schema: self.vertex_prop_schema,
            edge_props: self.edge_props,
            edge_prop_schema: self.edge_prop_schema,
            edge_type_schema: self.edge_type_schema,
            edge_info: self.edge_info,
            pending: PendingChanges::default(),
        }
    }
}

// ── FlatGraph public API ──────────────────────────────────────────────────────

impl FlatGraph {
    // ── Vertex queries ────────────────────────────────────────────────────────

    /// All vertex IDs with the given label (sorted), or an empty slice.
    pub fn all_vertex_ids_by_label(&self, label: &str) -> &[VertexId] {
        self.vertices_by_label
            .get(label)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Sample up to `n` vertex IDs of `label` uniformly at random (without replacement).
    pub fn sample_vertices_by_label<R: Rng>(
        &self,
        label: &str,
        n: usize,
        rng: &mut R,
    ) -> Vec<VertexId> {
        let vids = self.all_vertex_ids_by_label(label);
        if vids.len() <= n {
            return vids.to_vec();
        }
        let mut buf = vids.to_vec();
        buf.partial_shuffle(rng, n);
        buf.truncate(n);
        buf
    }

    /// Label name for a vertex, or `None` if the vertex is unknown.
    pub fn vertex_label(&self, vid: VertexId) -> Option<&str> {
        self.vertex_label_map.get(&vid).map(String::as_str)
    }

    /// Number of vertices with the given label.
    pub fn vertex_count_by_label(&self, label: &str) -> usize {
        self.vertices_by_label.get(label).map(Vec::len).unwrap_or(0)
    }

    /// Iterate over all known vertex labels.
    pub fn all_labels(&self) -> impl Iterator<Item = &str> {
        self.vertices_by_label.keys().map(String::as_str)
    }

    // ── Adjacency queries ─────────────────────────────────────────────────────

    /// `(neighbor_vid, edge_id)` pairs for Wander Join steps.
    ///
    /// Returns the **base CSR** slice directly (zero-copy, ignores pending).
    /// Call after `apply_pending` so the CSR reflects the current graph state.
    pub fn neighbors_with_eid(
        &self,
        vid: VertexId,
        edge_label: &str,
        outgoing: bool,
    ) -> &[(VertexId, EdgeId)] {
        let label = match self.vertex_label_map.get(&vid) {
            Some(l) => l.as_str(),
            None => return &[],
        };
        self.hop_csrs
            .get(&(label.to_string(), edge_label.to_string(), outgoing))
            .map(|csr| csr.neighbors_slice(vid))
            .unwrap_or(&[])
    }

    /// Neighbor vertex IDs for update-log compaction traversal.
    ///
    /// Merges base CSR neighbors with pending inserts and respects pending
    /// deletes.  Call this inside `GCardUpdateLog::compact_and_apply_flat` so
    /// the traversal sees the fully updated topology.
    pub fn neighbors_for_compact(
        &self,
        vid: VertexId,
        edge_label: &str,
        outgoing: bool,
    ) -> Vec<VertexId> {
        if self.pending.deleted_vertices.contains(&vid) {
            return Vec::new();
        }
        let label = match self.vertex_label_map.get(&vid) {
            Some(l) => l.as_str(),
            None => return Vec::new(),
        };

        let mut result = Vec::new();

        // Base CSR neighbors (filter out deleted edges and deleted vertices).
        if let Some(csr) = self
            .hop_csrs
            .get(&(label.to_string(), edge_label.to_string(), outgoing))
        {
            for &(nbr, eid) in csr.neighbors_slice(vid) {
                if !self.pending.deleted_edge_ids.contains(&eid)
                    && !self.pending.deleted_vertices.contains(&nbr)
                {
                    result.push(nbr);
                }
            }
        }

        // Pending inserted edges.
        for pe in &self.pending.inserted_edges {
            if outgoing && pe.src == vid && pe.edge_label == edge_label {
                if !self.pending.deleted_vertices.contains(&pe.dst) {
                    result.push(pe.dst);
                }
            } else if !outgoing && pe.dst == vid && pe.edge_label == edge_label {
                if !self.pending.deleted_vertices.contains(&pe.src) {
                    result.push(pe.src);
                }
            }
        }

        result
    }

    /// All `(edge_label, neighbor_label, outgoing)` extension triples reachable
    /// from vertices of `label`.
    ///
    /// Used by `GCardUpdateLog::compact_and_apply_flat` to build its extension
    /// table without needing an external edge schema or `LabelId` maps.
    pub fn edge_extensions_for_label(&self, label: &str) -> Vec<(String, String, bool)> {
        let mut result = Vec::new();
        for (edge_label, (src_lbl, dst_lbl)) in &self.edge_type_schema {
            if src_lbl.as_str() == label {
                result.push((edge_label.clone(), dst_lbl.clone(), true));
            }
            if dst_lbl.as_str() == label {
                result.push((edge_label.clone(), src_lbl.clone(), false));
            }
        }
        result
    }

    // ── Property queries ──────────────────────────────────────────────────────

    /// Property values for a vertex (indexed by schema position), or `None`.
    pub fn vertex_props(&self, vid: VertexId) -> Option<&[ScalarValue]> {
        self.vertex_props.get(&vid).map(Vec::as_slice)
    }

    /// Property values for an edge (indexed by schema position), or `None`.
    pub fn edge_props(&self, eid: EdgeId) -> Option<&[ScalarValue]> {
        self.edge_props.get(&eid).map(Vec::as_slice)
    }

    /// Column index for a vertex property given its label and property name.
    pub fn vertex_prop_index(&self, label: &str, prop_name: &str) -> Option<usize> {
        self.vertex_prop_schema
            .get(label)?
            .iter()
            .position(|s| s == prop_name)
    }

    /// Column index for an edge property.
    pub fn edge_prop_index(&self, edge_label: &str, prop_name: &str) -> Option<usize> {
        self.edge_prop_schema
            .get(edge_label)?
            .iter()
            .position(|s| s == prop_name)
    }

    // ── Update recording ──────────────────────────────────────────────────────

    /// Record a vertex insertion.
    ///
    /// Inserted vertices are immediately considered live in
    /// `neighbors_for_compact`.  Call `apply_pending` to materialise the
    /// structural change into the CSR.
    pub fn record_insert_vertex(&mut self, vid: VertexId, label: &str, props: Vec<ScalarValue>) {
        self.pending
            .inserted_vertices
            .push((vid, label.to_lowercase(), props));
    }

    /// Record an edge insertion.
    pub fn record_insert_edge(
        &mut self,
        edge_id: EdgeId,
        src: VertexId,
        src_label: &str,
        dst: VertexId,
        dst_label: &str,
        edge_label: &str,
        props: Vec<ScalarValue>,
    ) {
        self.pending.inserted_edges.push(PendingEdge {
            edge_id,
            src,
            src_label: src_label.to_lowercase(),
            dst,
            dst_label: dst_label.to_lowercase(),
            edge_label: edge_label.to_lowercase(),
            props,
        });
    }

    /// Record a vertex deletion.
    ///
    /// The vertex is immediately hidden from `neighbors_for_compact`.
    pub fn record_delete_vertex(&mut self, vid: VertexId) {
        self.pending.deleted_vertices.insert(vid);
    }

    /// Record an edge deletion.
    ///
    /// The edge is immediately hidden from `neighbors_for_compact`.
    pub fn record_delete_edge(&mut self, eid: EdgeId) {
        self.pending.deleted_edge_ids.insert(eid);
    }

    // ── Apply pending ─────────────────────────────────────────────────────────

    /// Flush all pending changes into the base CSR and property maps.
    ///
    /// Call **after** `GCardUpdateLog::compact_and_apply_flat` has finished so
    /// the statistical catalog is consistent with the new graph topology before
    /// the next query.
    pub fn apply_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);

        // ── 1. Apply vertex insertions ────────────────────────────────────────
        for (vid, label, props) in pending.inserted_vertices {
            self.vertex_label_map.insert(vid, label.clone());
            self.vertices_by_label.entry(label).or_default().push(vid);
            if !props.is_empty() {
                self.vertex_props.insert(vid, props);
            }
        }

        // ── 2. Apply vertex deletions ─────────────────────────────────────────
        for vid in &pending.deleted_vertices {
            if let Some(label) = self.vertex_label_map.remove(vid) {
                if let Some(vids) = self.vertices_by_label.get_mut(&label) {
                    vids.retain(|&v| v != *vid);
                }
            }
            self.vertex_props.remove(vid);
        }

        // Re-sort vertex lists after insertions/deletions.
        for vids in self.vertices_by_label.values_mut() {
            vids.sort_unstable();
        }

        // ── 3. Determine affected CSR buckets ─────────────────────────────────
        let mut affected: HashSet<(String, String, bool)> = HashSet::new();
        for pe in &pending.inserted_edges {
            affected.insert((pe.src_label.clone(), pe.edge_label.clone(), true));
            affected.insert((pe.dst_label.clone(), pe.edge_label.clone(), false));
        }
        for &eid in &pending.deleted_edge_ids {
            if let Some(info) = self.edge_info.get(&eid) {
                affected.insert((info.src_label.clone(), info.edge_label.clone(), true));
                affected.insert((info.dst_label.clone(), info.edge_label.clone(), false));
            }
        }

        // ── 4. Rebuild affected CSR buckets ───────────────────────────────────
        for bucket_key in &affected {
            let (src_label, edge_label, outgoing) = bucket_key;

            // Collect surviving edges from the existing CSR.
            let mut triples: Vec<(VertexId, VertexId, EdgeId)> = Vec::new();
            if let Some(csr) = self.hop_csrs.get(bucket_key) {
                for &src in &csr.verts {
                    for &(dst, eid) in csr.neighbors_slice(src) {
                        if !pending.deleted_edge_ids.contains(&eid) {
                            triples.push((src, dst, eid));
                        }
                    }
                }
            }

            // Append newly inserted edges belonging to this bucket.
            for pe in &pending.inserted_edges {
                if *outgoing && &pe.src_label == src_label && &pe.edge_label == edge_label {
                    triples.push((pe.src, pe.dst, pe.edge_id));
                } else if !*outgoing && &pe.dst_label == src_label && &pe.edge_label == edge_label {
                    triples.push((pe.dst, pe.src, pe.edge_id));
                }
            }

            // Rebuild (or remove if empty).
            let new_csr = CsrAdjWithEid::build(triples);
            if new_csr.vertex_count() == 0 {
                self.hop_csrs.remove(bucket_key);
            } else {
                self.hop_csrs.insert(bucket_key.clone(), new_csr);
            }
        }

        // ── 5. Update edge_info and edge_props ────────────────────────────────
        for pe in &pending.inserted_edges {
            self.edge_info.insert(
                pe.edge_id,
                EdgeInfo {
                    src: pe.src,
                    src_label: pe.src_label.clone(),
                    dst: pe.dst,
                    dst_label: pe.dst_label.clone(),
                    edge_label: pe.edge_label.clone(),
                },
            );
            if !pe.props.is_empty() {
                self.edge_props.insert(pe.edge_id, pe.props.clone());
            }
        }
        for &eid in &pending.deleted_edge_ids {
            self.edge_info.remove(&eid);
            self.edge_props.remove(&eid);
        }
    }

    // ── Statistics ────────────────────────────────────────────────────────────

    /// Total number of vertices across all labels.
    pub fn total_vertex_count(&self) -> usize {
        self.vertices_by_label.values().map(Vec::len).sum()
    }

    /// Total number of edges (counts each directed edge once).
    pub fn total_edge_count(&self) -> usize {
        self.edge_info.len()
    }
}
