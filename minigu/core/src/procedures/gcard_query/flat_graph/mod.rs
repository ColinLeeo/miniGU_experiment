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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use csr::CsrAdjWithEid;
use dashmap::DashMap;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use rand::Rng;
use rand::seq::index;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use update::{PendingChanges, PendingEdge};

pub(crate) fn sample_without_replacement<T: Clone, R: Rng + ?Sized>(
    values: &[T],
    amount: usize,
    rng: &mut R,
) -> Vec<T> {
    if values.len() <= amount {
        return values.to_vec();
    }
    index::sample(rng, values.len(), amount)
        .into_iter()
        .map(|idx| values[idx].clone())
        .collect()
}

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
    /// label_name → vertex_id → property values indexed by schema position.
    ///
    /// Vertex IDs are label-local in LDBC, so the label must be part of the
    /// key. Every stored row is padded to the label schema width with NULLs.
    vertex_props: HashMap<String, HashMap<VertexId, Vec<ScalarValue>>>,
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

    /// Exact filtered vertex IDs for bounded dimension-table anchors.
    /// Query-local caches cannot reuse these results across a workload, so the
    /// immutable FlatGraph owns the cache. It is excluded from persistence and
    /// invalidated whenever pending graph changes are applied.
    #[serde(skip)]
    predicate_vertex_cache: Arc<DashMap<String, Vec<VertexId>>>,

    /// Exact-value lookup for bounded dimension-table columns.
    ///
    /// Values are represented by their hash; callers re-evaluate predicates
    /// against the property row, so a hash collision can only add candidates,
    /// never change correctness. The index is rebuilt while loading the graph
    /// and excluded from the persisted FlatGraph payload.
    #[serde(skip)]
    bounded_vertex_value_index: Arc<HashMap<String, Vec<HashMap<u64, Vec<VertexId>>>>>,

    /// Exact candidates for structured `LIKE "%(... )%"` predicates.
    ///
    /// Only balanced parenthesized segments from bounded `note` columns are
    /// indexed. Callers re-evaluate the original SQL LIKE predicate, so hash
    /// collisions can add work but cannot change the result.
    #[serde(skip)]
    bounded_structured_token_index: Arc<HashMap<String, Vec<HashMap<u64, Vec<VertexId>>>>>,

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
    vertex_props: HashMap<String, HashMap<VertexId, Vec<ScalarValue>>>,
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
            .insert(label.to_lowercase(), prop_names);
    }

    /// Declare the property schema for an edge label.
    pub fn set_edge_prop_schema(&mut self, edge_label: &str, prop_names: Vec<String>) {
        self.edge_prop_schema
            .insert(edge_label.to_lowercase(), prop_names);
    }

    /// Register an edge type: which src/dst vertex labels it connects.
    pub fn set_edge_type_schema(&mut self, edge_label: &str, src_label: &str, dst_label: &str) {
        self.edge_type_schema.insert(
            edge_label.to_string(),
            (src_label.to_string(), dst_label.to_string()),
        );
    }

    /// Add a vertex.
    pub fn add_vertex(&mut self, vid: VertexId, label: &str, mut props: Vec<ScalarValue>) {
        let label_lc = label.to_lowercase();
        self.vertices_by_label
            .entry(label_lc.clone())
            .or_default()
            .push(vid);
        self.vertex_label_map.insert(vid, label_lc.clone());

        // A row is positional: pad missing trailing values so every schema
        // column retains its index. Interior missing values must already be
        // represented by ScalarValue::Null by the loader.
        if let Some(schema) = self.vertex_prop_schema.get(&label_lc) {
            props.resize(schema.len(), ScalarValue::Null);
            let table = self
                .graph_stats
                .vertex_stats
                .entry(label_lc.clone())
                .or_default();
            table.observe_row(schema, &props);
        }

        if !props.is_empty() {
            self.vertex_props
                .entry(label_lc)
                .or_default()
                .insert(vid, props);
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
        mut props: Vec<ScalarValue>,
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
            props.resize(schema.len(), ScalarValue::Null);
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

        let mut graph = FlatGraph {
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
            predicate_vertex_cache: Arc::new(DashMap::new()),
            bounded_vertex_value_index: Arc::new(HashMap::new()),
            bounded_structured_token_index: Arc::new(HashMap::new()),
            pending: PendingChanges::default(),
        };
        graph.rebuild_bounded_vertex_value_index();
        graph.rebuild_bounded_structured_token_index();
        graph
    }
}

// ── FlatGraph public API ──────────────────────────────────────────────────────

impl FlatGraph {
    const MAX_STRUCTURED_TOKEN_BYTES: usize = 128;
    const MAX_STRUCTURED_TOKEN_INDEX_VERTICES: usize = 5_000_000;
    const MAX_STRUCTURED_TOKEN_REFS_PER_COLUMN: usize = 5_000_000;
    // Include JOB-M's company_name dimension while still excluding million-row
    // fact tables. This adds one bounded dimension, not a general fact index.
    const MAX_VALUE_INDEX_VERTICES: usize = 300_000;

    fn scalar_hash(value: &ScalarValue) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    /// Rebuild exact-value indexes for small dimension tables.
    ///
    /// The returned tuple is `(labels, columns, distinct_hashes, vertex_ids)`
    /// and is used only for load-time observability. Large fact tables are
    /// deliberately excluded so estimator latency does not buy an unbounded
    /// memory footprint.
    fn rebuild_bounded_vertex_value_index(&mut self) -> (usize, usize, usize, usize) {
        let mut index = HashMap::<String, Vec<HashMap<u64, Vec<VertexId>>>>::new();
        let mut indexed_columns = 0usize;
        let mut indexed_values = 0usize;
        let mut indexed_vertex_ids = 0usize;

        for (label, vertex_ids) in &self.vertices_by_label {
            if vertex_ids.len() > Self::MAX_VALUE_INDEX_VERTICES {
                continue;
            }
            let Some(schema) = self.vertex_prop_schema.get(label) else {
                continue;
            };
            let Some(rows) = self.vertex_props.get(label) else {
                continue;
            };
            let mut columns = vec![HashMap::<u64, Vec<VertexId>>::new(); schema.len()];
            for (&vertex_id, properties) in rows {
                for (prop_index, value) in properties.iter().take(schema.len()).enumerate() {
                    columns[prop_index]
                        .entry(Self::scalar_hash(value))
                        .or_default()
                        .push(vertex_id);
                }
            }
            for vertex_ids in columns.iter_mut().flat_map(HashMap::values_mut) {
                vertex_ids.sort_unstable();
            }
            indexed_columns += columns.iter().filter(|column| !column.is_empty()).count();
            indexed_values += columns.iter().map(HashMap::len).sum::<usize>();
            indexed_vertex_ids += columns
                .iter()
                .flat_map(HashMap::values)
                .map(Vec::len)
                .sum::<usize>();
            index.insert(label.clone(), columns);
        }
        let indexed_labels = index.len();
        self.bounded_vertex_value_index = Arc::new(index);
        (
            indexed_labels,
            indexed_columns,
            indexed_values,
            indexed_vertex_ids,
        )
    }

    fn parenthesized_token_hashes(value: &str) -> Vec<u64> {
        let mut starts = Vec::new();
        let mut hashes = HashSet::new();
        for (index, character) in value.char_indices() {
            match character {
                '(' => starts.push(index),
                ')' => {
                    let Some(start) = starts.pop() else {
                        continue;
                    };
                    let end = index + character.len_utf8();
                    if end - start <= Self::MAX_STRUCTURED_TOKEN_BYTES {
                        hashes.insert(Self::scalar_hash(&ScalarValue::String(Some(
                            value[start..end].to_string(),
                        ))));
                    }
                }
                _ => {}
            }
        }
        hashes.into_iter().collect()
    }

    fn structured_like_literal(pattern: &ScalarValue) -> Option<&str> {
        let ScalarValue::String(Some(pattern)) = pattern else {
            return None;
        };
        let literal = pattern.strip_prefix('%')?.strip_suffix('%')?;
        if literal.len() > Self::MAX_STRUCTURED_TOKEN_BYTES
            || !literal.starts_with('(')
            || !literal.ends_with(')')
            || literal.contains('%')
            || literal.contains('_')
        {
            return None;
        }
        Some(literal)
    }

    /// Build a complete, bounded inverted index for parenthesized `note`
    /// tokens. A column that exceeds the reference cap is discarded entirely;
    /// a partial index could incorrectly prove that a value is absent.
    fn rebuild_bounded_structured_token_index(&mut self) -> (usize, usize, usize) {
        let mut index = HashMap::<String, Vec<HashMap<u64, Vec<VertexId>>>>::new();
        let mut indexed_columns = 0usize;
        let mut indexed_values = 0usize;
        let mut indexed_vertex_ids = 0usize;
        let mut labels = self.vertices_by_label.keys().cloned().collect::<Vec<_>>();
        labels.sort_unstable();

        for label in labels {
            let Some(vertex_ids) = self.vertices_by_label.get(&label) else {
                continue;
            };
            if vertex_ids.len() > Self::MAX_STRUCTURED_TOKEN_INDEX_VERTICES {
                continue;
            }
            let Some(schema) = self.vertex_prop_schema.get(&label) else {
                continue;
            };
            let Some(rows) = self.vertex_props.get(&label) else {
                continue;
            };
            let mut columns = vec![HashMap::<u64, Vec<VertexId>>::new(); schema.len()];
            let mut retained_any = false;
            for (prop_index, property_name) in schema.iter().enumerate() {
                if !property_name.eq_ignore_ascii_case("note") {
                    continue;
                }
                let mut column = HashMap::<u64, Vec<VertexId>>::new();
                let mut refs = 0usize;
                let mut exceeded = false;
                for (&vertex_id, properties) in rows {
                    let Some(ScalarValue::String(Some(value))) = properties.get(prop_index) else {
                        continue;
                    };
                    let token_hashes = Self::parenthesized_token_hashes(value);
                    if refs.saturating_add(token_hashes.len())
                        > Self::MAX_STRUCTURED_TOKEN_REFS_PER_COLUMN
                    {
                        exceeded = true;
                        break;
                    }
                    refs += token_hashes.len();
                    for token_hash in token_hashes {
                        column.entry(token_hash).or_default().push(vertex_id);
                    }
                }
                if exceeded {
                    continue;
                }
                for vertex_ids in column.values_mut() {
                    vertex_ids.sort_unstable();
                }
                indexed_columns += 1;
                indexed_values += column.len();
                indexed_vertex_ids += refs;
                columns[prop_index] = column;
                retained_any = true;
            }
            if retained_any {
                index.insert(label, columns);
            }
        }
        self.bounded_structured_token_index = Arc::new(index);
        (indexed_columns, indexed_values, indexed_vertex_ids)
    }

    pub fn export_bincode<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        let bytes = bincode::serialize(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize FlatGraph: {}", e))?;
        fs::write(path.as_ref(), bytes)?;
        Ok(())
    }

    pub fn import_bincode<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let bytes = fs::read(path.as_ref())?;
        let mut graph: Self = bincode::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("failed to deserialize FlatGraph: {}", e))?;
        let (labels, columns, values, vertex_ids) = graph.rebuild_bounded_vertex_value_index();
        let (token_columns, token_values, token_vertex_ids) =
            graph.rebuild_bounded_structured_token_index();
        eprintln!(
            "[database] bounded value index: labels={} columns={} values={} vertex_ids={} structured_token_columns={} structured_token_values={} structured_token_vertex_ids={}",
            labels, columns, values, vertex_ids, token_columns, token_values, token_vertex_ids,
        );
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
        sample_without_replacement(vids, n, rng)
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

    pub fn neighbors_with_eid_for_label(
        &self,
        vertex_label: &str,
        vid: VertexId,
        edge_label: &str,
        outgoing: bool,
    ) -> &[(VertexId, EdgeId)] {
        self.hop_csrs
            .get(&(vertex_label.to_string(), edge_label.to_string(), outgoing))
            .map(|csr| csr.neighbors_slice(vid))
            .unwrap_or(&[])
    }

    /// Resolve a compiled hop to its immutable CSR once, before entering the
    /// random-walk hot path. The string-key lookup is intentionally kept at
    /// plan compilation time so individual walk steps can use the returned
    /// handle directly.
    pub(crate) fn hop_csr_for_label(
        &self,
        vertex_label: &str,
        edge_label: &str,
        outgoing: bool,
    ) -> Option<&CsrAdjWithEid> {
        self.hop_csrs
            .get(&(vertex_label.to_string(), edge_label.to_string(), outgoing))
    }

    pub fn hop_bucket_edge_count(
        &self,
        vertex_label: &str,
        edge_label: &str,
        outgoing: bool,
    ) -> Option<usize> {
        self.hop_csrs
            .get(&(vertex_label.to_string(), edge_label.to_string(), outgoing))
            .map(CsrAdjWithEid::edge_count)
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
    /// The label is required because LDBC vertex IDs are label-local.
    pub fn vertex_props(&self, label: &str, vid: VertexId) -> Option<&[ScalarValue]> {
        self.vertex_props.get(label)?.get(&vid).map(Vec::as_slice)
    }

    /// Property rows for one label without repeating a hash lookup per vertex.
    ///
    /// Selective scans over bounded dimension tables use this view so their
    /// cost is a single sequential hash-table iteration rather than one lookup
    /// through both property maps for every label-local vertex ID.
    pub(crate) fn vertex_property_rows(
        &self,
        label: &str,
    ) -> Option<&HashMap<VertexId, Vec<ScalarValue>>> {
        self.vertex_props.get(label)
    }

    pub(crate) fn predicate_vertex_cache(&self) -> &DashMap<String, Vec<VertexId>> {
        &self.predicate_vertex_cache
    }

    pub(crate) fn vertex_ids_by_exact_value(
        &self,
        label: &str,
        prop_index: usize,
        value: &ScalarValue,
    ) -> Option<&[VertexId]> {
        let hash = Self::scalar_hash(value);
        self.bounded_vertex_value_index
            .get(label)?
            .get(prop_index)?
            .get(&hash)
            .map(Vec::as_slice)
    }

    pub(crate) fn has_exact_vertex_value_index(&self, label: &str, prop_index: usize) -> bool {
        self.bounded_vertex_value_index
            .get(label)
            .and_then(|columns| columns.get(prop_index))
            .is_some_and(|column| !column.is_empty())
    }

    pub(crate) fn structured_like_index_candidates(
        &self,
        label: &str,
        prop_index: usize,
        pattern: &ScalarValue,
    ) -> Option<Vec<VertexId>> {
        let literal = Self::structured_like_literal(pattern)?;
        let column = self
            .bounded_structured_token_index
            .get(label)?
            .get(prop_index)?;
        let hash = Self::scalar_hash(&ScalarValue::String(Some(literal.to_string())));
        Some(column.get(&hash).cloned().unwrap_or_default())
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
        self.predicate_vertex_cache.clear();
        self.bounded_vertex_value_index = Arc::new(HashMap::new());
        self.bounded_structured_token_index = Arc::new(HashMap::new());
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
        for (vid, label, mut props) in pending.inserted_vertices {
            self.vertex_label_map.insert(vid, label.clone());
            self.vertices_by_label
                .entry(label.clone())
                .or_default()
                .push(vid);
            if let Some(schema) = self.vertex_prop_schema.get(&label) {
                props.resize(schema.len(), ScalarValue::Null);
            }
            if !props.is_empty() {
                self.vertex_props
                    .entry(label)
                    .or_default()
                    .insert(vid, props);
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
            for props_by_id in self.vertex_props.values_mut() {
                props_by_id.remove(vid);
            }
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
            let mut props = pe.props.clone();
            if let Some(schema) = self.edge_prop_schema.get(&pe.edge_label) {
                props.resize(schema.len(), ScalarValue::Null);
            }
            if !props.is_empty() {
                self.edge_props.insert(pe.edge_id, props);
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

    /// All edge IDs for a specific edge label.
    pub fn all_edge_ids_by_label(&self, edge_label: &str) -> Vec<EdgeId> {
        let Some((src_label, _)) = self.edge_type_schema.get(edge_label) else {
            return Vec::new();
        };
        let key = (src_label.clone(), edge_label.to_string(), true);
        let Some(csr) = self.hop_csrs.get(&key) else {
            return Vec::new();
        };
        csr.neighbors.iter().map(|&(_, eid)| eid).collect()
    }

    /// Uniformly sample one edge ID for a specific edge label from the outgoing CSR bucket.
    pub fn sample_edge_id_by_label<R: Rng>(&self, edge_label: &str, rng: &mut R) -> Option<EdgeId> {
        let (src_label, _) = self.edge_type_schema.get(edge_label)?;
        let key = (src_label.clone(), edge_label.to_string(), true);
        let csr = self.hop_csrs.get(&key)?;
        if csr.neighbors.is_empty() {
            return None;
        }
        let idx = rng.gen_range(0..csr.neighbors.len());
        Some(csr.neighbors[idx].1)
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
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use minigu_common::value::ScalarValue;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tempfile::tempdir;

    use super::{FlatGraphBuilder, sample_without_replacement};

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

    #[test]
    fn bounded_value_index_returns_exact_candidates_after_roundtrip() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("keyword", vec!["value".to_string()]);
        builder.add_vertex(
            1,
            "keyword",
            vec![ScalarValue::String(Some("sequel".to_string()))],
        );
        builder.add_vertex(
            2,
            "keyword",
            vec![ScalarValue::String(Some("other".to_string()))],
        );
        builder.add_vertex(
            3,
            "keyword",
            vec![ScalarValue::String(Some("sequel".to_string()))],
        );
        let graph = builder.build();
        let sequel = ScalarValue::String(Some("sequel".to_string()));
        assert!(graph.has_exact_vertex_value_index("keyword", 0));
        assert_eq!(
            graph.vertex_ids_by_exact_value("keyword", 0, &sequel),
            Some([1, 3].as_slice())
        );

        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        graph.export_bincode(&path).unwrap();
        let restored = super::FlatGraph::import_bincode(&path).unwrap();
        assert_eq!(
            restored.vertex_ids_by_exact_value("keyword", 0, &sequel),
            Some([1, 3].as_slice())
        );
    }

    #[test]
    fn structured_note_index_returns_exact_like_candidates_after_roundtrip() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema("movie_companies", vec!["note".to_string()]);
        builder.add_vertex(
            1,
            "movie_companies",
            vec![ScalarValue::String(Some(
                "released in (USA) on (VHS)".to_string(),
            ))],
        );
        builder.add_vertex(
            2,
            "movie_companies",
            vec![ScalarValue::String(Some("released in (USA)".to_string()))],
        );
        builder.add_vertex(
            3,
            "movie_companies",
            vec![ScalarValue::String(Some(
                "released in (France)".to_string(),
            ))],
        );
        let graph = builder.build();
        let usa = ScalarValue::String(Some("%(USA)%".to_string()));
        assert_eq!(
            graph.structured_like_index_candidates("movie_companies", 0, &usa),
            Some(vec![1, 2])
        );
        assert_eq!(
            graph.structured_like_index_candidates(
                "movie_companies",
                0,
                &ScalarValue::String(Some("%USA%".to_string())),
            ),
            None
        );

        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        graph.export_bincode(&path).unwrap();
        let restored = super::FlatGraph::import_bincode(&path).unwrap();
        assert_eq!(
            restored.structured_like_index_candidates("movie_companies", 0, &usa),
            Some(vec![1, 2])
        );
    }

    #[test]
    fn sample_vertices_without_replacement_is_distinct_and_reproducible() {
        let mut builder = FlatGraphBuilder::new();
        for vid in 0..1_000 {
            builder.add_vertex(vid, "person", vec![]);
        }
        let graph = builder.build();

        let mut rng_a = StdRng::seed_from_u64(42);
        let actual = graph.sample_vertices_by_label("person", 10, &mut rng_a);
        let mut rng_b = StdRng::seed_from_u64(42);
        let repeated = graph.sample_vertices_by_label("person", 10, &mut rng_b);

        assert_eq!(actual, repeated);
        assert_eq!(actual.len(), 10);
        assert_eq!(actual.iter().copied().collect::<HashSet<_>>().len(), 10);
        assert!(actual.iter().all(|&vid| vid < 1_000));
    }

    #[test]
    fn sample_without_replacement_only_clones_selected_values() {
        #[derive(Debug)]
        struct CloneCounted {
            value: usize,
            clone_count: Arc<AtomicUsize>,
        }

        impl Clone for CloneCounted {
            fn clone(&self) -> Self {
                self.clone_count.fetch_add(1, Ordering::Relaxed);
                Self {
                    value: self.value,
                    clone_count: Arc::clone(&self.clone_count),
                }
            }
        }

        let clone_count = Arc::new(AtomicUsize::new(0));
        let values: Vec<_> = (0..10_000)
            .map(|value| CloneCounted {
                value,
                clone_count: Arc::clone(&clone_count),
            })
            .collect();
        let mut rng = StdRng::seed_from_u64(42);

        let sampled = sample_without_replacement(&values, 32, &mut rng);

        assert_eq!(sampled.len(), 32);
        assert_eq!(clone_count.load(Ordering::Relaxed), 32);
    }

    #[test]
    fn property_rows_are_schema_aligned_and_label_scoped() {
        let mut builder = FlatGraphBuilder::new();
        builder.set_vertex_prop_schema(
            "person",
            vec!["name".to_string(), "middle".to_string(), "city".to_string()],
        );
        builder.set_vertex_prop_schema("forum", vec!["title".to_string()]);
        builder.set_edge_prop_schema("knows", vec!["since".to_string(), "until".to_string()]);
        builder.set_edge_type_schema("knows", "person", "person");

        // The same label-local ID belongs to two labels. The explicit NULL in
        // the middle must not shift `city`, and the short second row must be
        // padded to the schema width.
        builder.add_vertex(
            1,
            "person",
            vec![
                ScalarValue::String(Some("Alice".to_string())),
                ScalarValue::Null,
                ScalarValue::String(Some("Beijing".to_string())),
            ],
        );
        builder.add_vertex(
            2,
            "person",
            vec![ScalarValue::String(Some("Bob".to_string()))],
        );
        builder.add_vertex(
            1,
            "forum",
            vec![ScalarValue::String(Some("Graph".to_string()))],
        );
        builder.add_edge(
            10,
            1,
            "person",
            2,
            "person",
            "knows",
            vec![ScalarValue::Int64(Some(2020))],
        );
        let graph = builder.build();

        assert_eq!(
            graph.vertex_props("person", 1),
            Some(
                [
                    ScalarValue::String(Some("Alice".to_string())),
                    ScalarValue::Null,
                    ScalarValue::String(Some("Beijing".to_string())),
                ]
                .as_slice()
            )
        );
        assert_eq!(
            graph.vertex_props("forum", 1),
            Some([ScalarValue::String(Some("Graph".to_string()))].as_slice())
        );
        assert_eq!(
            graph.vertex_props("person", 2),
            Some(
                [
                    ScalarValue::String(Some("Bob".to_string())),
                    ScalarValue::Null,
                    ScalarValue::Null,
                ]
                .as_slice()
            )
        );
        assert_eq!(
            graph.edge_props(10),
            Some([ScalarValue::Int64(Some(2020)), ScalarValue::Null].as_slice())
        );

        let person_stats = graph.vertex_table_stats("person").unwrap();
        assert_eq!(person_stats.columns["middle"].total_count, 2);
        assert_eq!(person_stats.columns["middle"].null_count, 2);
        assert_eq!(person_stats.columns["city"].total_count, 2);
        assert_eq!(person_stats.columns["city"].null_count, 1);
    }
}
