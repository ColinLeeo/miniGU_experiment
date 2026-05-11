//! GCard 自用的轻量图存储。
//!
//! 它的定位不是通用图存储，而是“面向 GCard 热路径的只读/少写结构”。
//! 相比 `MemoryGraph + MemTransaction`，它去掉了 MVCC、事务、锁等通用能力，
//! 换来更低的邻居访问成本。
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
pub mod stats;
pub mod update;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use csr::CsrAdjWithEid;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use rand::Rng;
use rand::seq::SliceRandom;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use update::{PendingChanges, PendingEdge};

// ── Reverse-lookup metadata stored per edge ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EdgeInfo {
    /// 边两端和标签的反查元数据。
    /// 删除边或按 edge_id 回查类型时要用到。
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
#[derive(Clone, Serialize, Deserialize)]
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

    // ── Column/table statistics ──────────────────────────────────────────────
    /// Per-label, per-property statistics (NDV, min/max) collected at build time.
    #[serde(default)]
    graph_stats: stats::GraphStats,

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
    /// 构建 CSR 之前的暂存三元组。
    edge_triples: HashMap<(String, String, bool), Vec<(VertexId, VertexId, EdgeId)>>,
    /// Statistics collected during construction.
    graph_stats: stats::GraphStats,
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
        self.vertex_label_map.insert(vid, label_lc.clone());

        // Collect column stats.
        if let Some(schema) = self.vertex_prop_schema.get(&label_lc) {
            let table = self.graph_stats.vertex_stats.entry(label_lc).or_default();
            table.observe_row(schema, &props);
        }

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
        // 这里同时写 outgoing / incoming 两套 bucket，
        // 因为后续查询图遍历经常需要双向看邻居。
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
                edge_label: el_lc.clone(),
            },
        );

        // Collect column stats.
        if let Some(schema) = self.edge_prop_schema.get(&el_lc) {
            let table = self.graph_stats.edge_stats.entry(el_lc).or_default();
            table.observe_row(schema, &props);
        }

        if !props.is_empty() {
            self.edge_props.insert(eid, props);
        }
    }

    /// Consume the builder and produce a [`FlatGraph`].
    ///
    /// Sorts vertex ID lists and constructs all CSR adjacency structures.
    pub fn build(mut self) -> FlatGraph {
        // 顶点列表有序化后，采样、二分查找都会更稳定。
        for vids in self.vertices_by_label.values_mut() {
            vids.sort_unstable();
            vids.dedup();
        }

        // 每种 `(label, edge_label, direction)` 独立建一个 CSR bucket。
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
            graph_stats: self.graph_stats,
            pending: PendingChanges::default(),
        }
    }
}

// ── FlatGraph public API ──────────────────────────────────────────────────────

impl FlatGraph {
    pub fn export_bincode<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        let bytes = bincode::serialize(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize FlatGraph: {}", e))?;
        fs::write(path.as_ref(), bytes)?;
        Ok(())
    }

    pub fn import_bincode<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let bytes = fs::read(path.as_ref())?;
        let graph = bincode::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("failed to deserialize FlatGraph: {}", e))?;
        Ok(graph)
    }

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
        let key = (label.to_string(), edge_label.to_string(), outgoing);
        if let Some(csr) = self.hop_csrs.get(&key) {
            for &(nbr, eid) in csr.neighbors_slice(vid) {
                if !self.pending.deleted_edge_ids.contains(&eid)
                    && !self.pending.deleted_vertices.contains(&nbr)
                {
                    result.push(nbr);
                }
            }
        }

        // Pending inserted edges — O(1) lookup via index instead of full scan.
        let idx_key = (vid, edge_label.to_string());
        if outgoing {
            if let Some(nbrs) = self.pending.pending_out.get(&idx_key) {
                for &dst in nbrs {
                    if !self.pending.deleted_vertices.contains(&dst) {
                        result.push(dst);
                    }
                }
            }
        } else if let Some(nbrs) = self.pending.pending_in.get(&idx_key) {
            for &src in nbrs {
                if !self.pending.deleted_vertices.contains(&src) {
                    result.push(src);
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

    // ── Statistics queries ─────────────────────────────────────────────────────

    /// All collected statistics.
    pub fn graph_stats(&self) -> &stats::GraphStats {
        &self.graph_stats
    }

    /// Statistics for a vertex label, if available.
    pub fn vertex_table_stats(&self, label: &str) -> Option<&stats::TableStats> {
        self.graph_stats.vertex_stats.get(label)
    }

    /// Statistics for an edge label, if available.
    pub fn edge_table_stats(&self, edge_label: &str) -> Option<&stats::TableStats> {
        self.graph_stats.edge_stats.get(edge_label)
    }

    /// Column stats for a specific vertex property.
    pub fn vertex_column_stats(&self, label: &str, prop: &str) -> Option<&stats::ColumnStats> {
        self.graph_stats.vertex_stats.get(label)?.columns.get(prop)
    }

    /// Column stats for a specific edge property.
    pub fn edge_column_stats(&self, edge_label: &str, prop: &str) -> Option<&stats::ColumnStats> {
        self.graph_stats
            .edge_stats
            .get(edge_label)?
            .columns
            .get(prop)
    }

    // ── Statistics update ──────────────────────────────────────────────────────

    /// Update vertex statistics after an insert.
    pub fn observe_vertex_stats(&mut self, label: &str, props: &[ScalarValue]) {
        if let Some(schema) = self.vertex_prop_schema.get(label).cloned() {
            let table = self
                .graph_stats
                .vertex_stats
                .entry(label.to_string())
                .or_default();
            table.observe_row(&schema, props);
        }
    }

    /// Update edge statistics after an insert.
    pub fn observe_edge_stats(&mut self, edge_label: &str, props: &[ScalarValue]) {
        if let Some(schema) = self.edge_prop_schema.get(edge_label).cloned() {
            let table = self
                .graph_stats
                .edge_stats
                .entry(edge_label.to_string())
                .or_default();
            table.observe_row(&schema, props);
        }
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
        self.pending.insert_edge(PendingEdge {
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
    /// Read-only access to pending changes.
    pub fn pending_ref(&self) -> &PendingChanges {
        &self.pending
    }

    /// Call **after** `GCardUpdateLog::compact_and_apply_flat` has finished so
    /// the statistical catalog is consistent with the new graph topology before
    /// the next query.
    pub fn apply_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let t_total = std::time::Instant::now();
        let pending = std::mem::take(&mut self.pending);
        eprintln!(
            "[flatgraph] apply_pending start: +{} vertices, -{} vertices, +{} edges, -{} edges",
            pending.inserted_vertices.len(),
            pending.deleted_vertices.len(),
            pending.inserted_edges.len(),
            pending.deleted_edge_ids.len()
        );

        // ── 1. Apply vertex insertions ────────────────────────────────────────
        let t_phase = std::time::Instant::now();
        for (vid, label, props) in pending.inserted_vertices {
            self.vertex_label_map.insert(vid, label.clone());
            self.vertices_by_label.entry(label).or_default().push(vid);
            if !props.is_empty() {
                self.vertex_props.insert(vid, props);
            }
        }

        // ── 2. Apply vertex deletions ─────────────────────────────────────────
        // Collect deleted vertex labels BEFORE removing from vertex_label_map.
        let mut deleted_vertex_labels: HashSet<String> = HashSet::new();
        for vid in &pending.deleted_vertices {
            if let Some(label) = self.vertex_label_map.get(vid) {
                deleted_vertex_labels.insert(label.clone());
            }
        }
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
        eprintln!(
            "[flatgraph] apply_pending phase1 vertices: {:.2}s",
            t_phase.elapsed().as_secs_f64()
        );

        // ── 3. Determine affected CSR buckets ─────────────────────────────────
        let t_phase = std::time::Instant::now();
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
        // Deleted vertices may appear as sources in CSR buckets that have no
        // deleted edges.  Mark all buckets whose src_label matches any deleted
        // vertex's label so they get rebuilt without stale vertex entries.
        for (bucket_key, _) in &self.hop_csrs {
            if deleted_vertex_labels.contains(&bucket_key.0) {
                affected.insert(bucket_key.clone());
            }
        }
        let affected_count = affected.len();
        let affected_keys: Vec<(String, String, bool)> = affected.into_iter().collect();
        eprintln!(
            "[flatgraph] apply_pending phase2 affected_buckets: {:.2}s ({} buckets)",
            t_phase.elapsed().as_secs_f64(),
            affected_count
        );

        // ── 4. Rebuild affected CSR buckets ───────────────────────────────────
        let t_phase = std::time::Instant::now();
        let affected_keys_for_apply = affected_keys.clone();
        let rebuilt_buckets: Vec<((String, String, bool), CsrAdjWithEid)> = affected_keys
            .into_par_iter()
            .filter_map(|bucket_key| {
                let (src_label, edge_label, outgoing) = &bucket_key;

                // Collect surviving edges from the existing CSR.
                // Filter out deleted edges AND edges incident to deleted vertices.
                let mut triples: Vec<(VertexId, VertexId, EdgeId)> = Vec::new();
                if let Some(csr) = self.hop_csrs.get(&bucket_key) {
                    for &src in &csr.verts {
                        if pending.deleted_vertices.contains(&src) {
                            continue;
                        }
                        for &(dst, eid) in csr.neighbors_slice(src) {
                            if !pending.deleted_edge_ids.contains(&eid)
                                && !pending.deleted_vertices.contains(&dst)
                            {
                                triples.push((src, dst, eid));
                            }
                        }
                    }
                }

                // Append newly inserted edges belonging to this bucket.
                for pe in &pending.inserted_edges {
                    if *outgoing && &pe.src_label == src_label && &pe.edge_label == edge_label {
                        triples.push((pe.src, pe.dst, pe.edge_id));
                    } else if !*outgoing
                        && &pe.dst_label == src_label
                        && &pe.edge_label == edge_label
                    {
                        triples.push((pe.dst, pe.src, pe.edge_id));
                    }
                }

                let new_csr = CsrAdjWithEid::build(triples);
                if new_csr.vertex_count() == 0 {
                    None
                } else {
                    Some((bucket_key, new_csr))
                }
            })
            .collect();
        let rebuilt_edge_entries: usize = rebuilt_buckets
            .iter()
            .map(|(_, csr)| csr.edge_count())
            .sum();
        for key in affected_keys_for_apply {
            self.hop_csrs.remove(&key);
        }
        for (bucket_key, new_csr) in rebuilt_buckets {
            self.hop_csrs.insert(bucket_key, new_csr);
        }
        eprintln!(
            "[flatgraph] apply_pending phase3 rebuild_buckets: {:.2}s ({} buckets, {} edge entries)",
            t_phase.elapsed().as_secs_f64(),
            affected_count,
            rebuilt_edge_entries
        );

        // ── 5. Update edge_info and edge_props ────────────────────────────────
        let t_phase = std::time::Instant::now();
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
        eprintln!(
            "[flatgraph] apply_pending phase4 edge_maps: {:.2}s",
            t_phase.elapsed().as_secs_f64()
        );
        eprintln!(
            "[flatgraph] apply_pending total: {:.2}s",
            t_total.elapsed().as_secs_f64()
        );
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

    /// Number of edges for a specific edge label, derived from the outgoing CSR bucket.
    /// O(1) — no full scan needed.
    pub fn edge_count_by_label(&self, edge_label: &str) -> usize {
        if let Some((src_label, _)) = self.edge_type_schema.get(edge_label) {
            let key = (src_label.clone(), edge_label.to_string(), true);
            self.hop_csrs
                .get(&key)
                .map(|csr| csr.edge_count())
                .unwrap_or(0)
        } else {
            0
        }
    }

    // ── Accessors for export / random_update ─────────────────────────────────

    /// All edge IDs currently in the graph.
    pub fn all_edge_ids(&self) -> impl Iterator<Item = EdgeId> + '_ {
        self.edge_info.keys().copied()
    }

    /// Endpoint and label info for an edge, or `None` if not present.
    pub fn edge_endpoints(&self, eid: EdgeId) -> Option<(VertexId, &str, VertexId, &str, &str)> {
        self.edge_info.get(&eid).map(|info| {
            (
                info.src,
                info.src_label.as_str(),
                info.dst,
                info.dst_label.as_str(),
                info.edge_label.as_str(),
            )
        })
    }

    /// CSR adjacency maps: `(vertex_label, edge_label, outgoing)` → CSR.
    pub fn hop_csrs(&self) -> &HashMap<(String, String, bool), CsrAdjWithEid> {
        &self.hop_csrs
    }

    /// Vertex label → sorted vertex IDs map.
    pub fn vertices_by_label_map(&self) -> &HashMap<String, Vec<VertexId>> {
        &self.vertices_by_label
    }

    /// Edge type schema: edge_label → (src_vertex_label, dst_vertex_label).
    pub fn edge_type_schema(&self) -> &HashMap<String, (String, String)> {
        &self.edge_type_schema
    }

    /// Vertex property schema: label → ordered property names.
    pub fn vertex_prop_schema(&self) -> &HashMap<String, Vec<String>> {
        &self.vertex_prop_schema
    }

    /// Edge property schema: edge_label → ordered property names.
    pub fn edge_prop_schema(&self) -> &HashMap<String, Vec<String>> {
        &self.edge_prop_schema
    }

    /// All edges incident to a vertex via CSR lookup (not full scan).
    ///
    /// Returns `(edge_id, neighbor_id, neighbor_label, edge_label)` for each edge.
    pub fn incident_edges(&self, vid: VertexId) -> Vec<(EdgeId, VertexId, String, String)> {
        let label = match self.vertex_label_map.get(&vid) {
            Some(l) => l.as_str(),
            None => return Vec::new(),
        };
        let mut result = Vec::new();
        // Scan all CSR buckets that could contain this vertex as a source.
        for ((src_label, edge_label, outgoing), csr) in &self.hop_csrs {
            if src_label == label {
                for &(nbr, eid) in csr.neighbors_slice(vid) {
                    if *outgoing {
                        let nbr_label = self
                            .vertex_label_map
                            .get(&nbr)
                            .map(String::as_str)
                            .unwrap_or("");
                        result.push((eid, nbr, nbr_label.to_string(), edge_label.clone()));
                    } else {
                        let nbr_label = self
                            .vertex_label_map
                            .get(&nbr)
                            .map(String::as_str)
                            .unwrap_or("");
                        result.push((eid, nbr, nbr_label.to_string(), edge_label.clone()));
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::FlatGraphBuilder;

    #[test]
    fn flat_graph_bincode_roundtrip() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_edge_type_schema("knows", "person", "person");
        builder.add_vertex(1, "person", vec![]);
        builder.add_vertex(2, "person", vec![]);
        builder.add_edge(10, 1, "person", 2, "person", "knows", vec![]);
        let graph = builder.build();

        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        graph.export_bincode(&path).unwrap();

        let restored = super::FlatGraph::import_bincode(&path).unwrap();
        assert_eq!(restored.all_vertex_ids_by_label("person"), &[1, 2]);
        assert_eq!(restored.all_edge_ids().collect::<Vec<_>>(), vec![10]);
        assert_eq!(
            restored.edge_endpoints(10),
            Some((1, "person", 2, "person", "knows"))
        );
    }
}
