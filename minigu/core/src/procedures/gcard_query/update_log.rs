//! Lazy update log for GCard statistics.
//!
//! When edges/vertices are inserted or deleted, callers append [`UpdateEntry`] records here
//! instead of immediately recomputing multi-hop degree statistics.
//! [`GCardUpdateLog::compact_and_apply_flat`] later expands all pending entries via FlatGraph
//! traversal, applies the net deltas to [`Statistic`], removes any deleted vertices, and
//! clears the log.
//!
//! Stored type-erased inside [`GraphContainer`] (same pattern as `Statistic` /
//! `DegreeSeqGraphCompressed`).  Consumers in `minigu-core` downcast via
//! `Arc<Mutex<GCardUpdateLog>>`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use minigu_common::types::{LabelId, VertexId};
use rayon::prelude::*;

use crate::procedures::gcard_query::catalog::AltKey;
use crate::procedures::gcard_query::statistic::Statistic;

// ── Entry ─────────────────────────────────────────────────────────────────────

/// A single pending degree-delta record.
///
/// `template` is an [`AltKey`] whose **leftmost** vertex label equals `label`.
/// Example layouts (interleaved vertex / edge labels, same as `make_alt_key`):
///
/// | path hops | AltKey contents                                      |
/// |-----------|------------------------------------------------------|
/// | 1-hop     | `[tracked_label, edge_label, other_label]`           |
/// | 2-hop     | `[tracked_label, e1, mid_label, e2, other_label]`    |
#[derive(Debug, Clone)]
pub struct UpdateEntry {
    /// 路径模板。
    /// 约定 `label` 对应最左端顶点标签，这样扩展时方向更统一。
    pub template: AltKey,
    /// `nodes` 里这些顶点所属的标签。
    pub label: String,
    /// 顶点 -> 度增量。插入为 +1，删除为 -1。
    pub nodes: HashMap<VertexId, i64>,
    /// 还剩多少 hop 需要往外扩展。
    /// 为 0 时表示已经是最终项，不需要继续沿图传播。
    pub res_len: usize,
}

// ── Log ───────────────────────────────────────────────────────────────────────

/// Pending GCard update log stored inside [`GraphContainer`] as
/// `Arc<Mutex<GCardUpdateLog>>` (type-erased).
#[derive(Debug, Default)]
pub struct GCardUpdateLog {
    /// 所有待处理更新项。
    pub entries: Vec<UpdateEntry>,
    /// `(template, label, res_len)` → index in `entries`.
    /// Used to merge repeated updates in O(1) expected time instead of appending
    /// one entry per edge mutation.
    entry_index: HashMap<(AltKey, String, usize), usize>,
    /// 构建 catalog 时使用的最大路径长度 K。
    /// 它决定一条基础更新最多还需要向外传播几层。
    pub max_k: usize,
    /// 自上次 compact 以来被删除的顶点。
    /// 它们既要在统计里移除，也要在传播时跳过。
    pub deleted_vertices: HashSet<VertexId>,
}

impl GCardUpdateLog {
    pub fn new(max_k: usize) -> Self {
        Self {
            entries: Vec::new(),
            entry_index: HashMap::new(),
            max_k,
            deleted_vertices: HashSet::new(),
        }
    }

    // ── Public record helpers ──────────────────────────────────────────────

    /// Record an edge insertion `(u:u_label) -[edge_label]-> (v:v_label)`.
    ///
    /// Appends two entries (one per endpoint); no graph traversal is performed.
    pub fn record_insert_edge(
        &mut self,
        u: VertexId,
        u_label: &str,
        v: VertexId,
        v_label: &str,
        edge_label: &str,
    ) {
        self.push_edge_entries(u, u_label, v, v_label, edge_label, 1);
    }

    /// Record an edge deletion `(u:u_label) -[edge_label]-> (v:v_label)`.
    ///
    /// Appends two entries (one per endpoint); no graph traversal is performed.
    pub fn record_delete_edge(
        &mut self,
        u: VertexId,
        u_label: &str,
        v: VertexId,
        v_label: &str,
        edge_label: &str,
    ) {
        self.push_edge_entries(u, u_label, v, v_label, edge_label, -1);
    }

    /// Record a vertex deletion for `(u:u_label)`.
    ///
    /// `u` is added to `deleted_vertices` so it is skipped during compact traversal and
    /// its statistics are removed at the end of [`compact_and_apply`].
    ///
    /// `neighbors` must be collected **before** the vertex is removed from the graph.
    /// Each element is `(neighbor_id, neighbor_label, edge_label)` for every edge
    /// incident to `u`.  One entry per distinct `(neighbor_label, edge_label)` group is
    /// appended to propagate the deletion effect to the neighbours' multi-hop statistics.
    pub fn record_delete_vertex(
        &mut self,
        u: VertexId,
        u_label: &str,
        neighbors: &[(VertexId, String, String)],
    ) {
        // 顶点删除的影响比分边删除更复杂：
        // 不仅这个点本身要消失，它的邻居在多跳路径上的统计也会被连带影响。
        self.deleted_vertices.insert(u);

        let res_len = self.max_k.saturating_sub(1);

        let mut groups: HashMap<(String, String), Vec<VertexId>> = HashMap::new();
        for (neighbor_id, neighbor_label, edge_label) in neighbors {
            groups
                .entry((neighbor_label.clone(), edge_label.clone()))
                .or_default()
                .push(*neighbor_id);
        }

        for ((neighbor_label, edge_label), neighbor_ids) in groups {
            // 这里按 `(neighbor_label, edge_label)` 聚合，
            // 避免为每条 incident edge 各自生成一条重复模板。
            let template = AltKey::new(vec![
                neighbor_label.clone(),
                edge_label,
                u_label.to_string(),
            ]);
            let nodes: HashMap<VertexId, i64> =
                neighbor_ids.into_iter().map(|id| (id, -1)).collect();
            self.push_or_merge_entry(template, neighbor_label, nodes, res_len);
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    fn push_or_merge_entry(
        &mut self,
        template: AltKey,
        label: String,
        nodes: HashMap<VertexId, i64>,
        res_len: usize,
    ) {
        let key = (template.clone(), label.clone(), res_len);
        if let Some(&idx) = self.entry_index.get(&key) {
            let entry = &mut self.entries[idx];
            for (vid, delta) in nodes {
                let slot = entry.nodes.entry(vid).or_insert(0);
                *slot += delta;
                if *slot == 0 {
                    entry.nodes.remove(&vid);
                }
            }
            return;
        }

        let idx = self.entries.len();
        self.entries.push(UpdateEntry {
            template,
            label,
            nodes,
            res_len,
        });
        self.entry_index.insert(key, idx);
    }

    fn push_edge_entries(
        &mut self,
        u: VertexId,
        u_label: &str,
        v: VertexId,
        v_label: &str,
        edge_label: &str,
        delta: i64,
    ) {
        // 一条边的插入/删除会影响两个端点视角下的统计，
        // 所以这里对两端各记一条 entry。
        let res_len = self.max_k.saturating_sub(1);

        self.push_or_merge_entry(
            AltKey::new(vec![
                u_label.to_string(),
                edge_label.to_string(),
                v_label.to_string(),
            ]),
            u_label.to_string(),
            HashMap::from([(u, delta)]),
            res_len,
        );

        self.push_or_merge_entry(
            AltKey::new(vec![
                v_label.to_string(),
                edge_label.to_string(),
                u_label.to_string(),
            ]),
            v_label.to_string(),
            HashMap::from([(v, delta)]),
            res_len,
        );
    }

    // ── FlatGraph-based compact (no DB dependency) ─────────────────────────

    /// Variant of [`compact_and_apply`] that uses [`FlatGraph`] for graph
    /// traversal instead of `MemoryGraph + MemTransaction`.
    ///
    /// The extension table is derived directly from
    /// `flat_graph.edge_extensions_for_label`, so no external `edge_schema` or
    /// `label_id_to_name` maps are required.
    ///
    /// After this call, invoke `flat_graph.apply_pending()` to materialise any
    /// structural changes into the CSR base.
    pub fn compact_and_apply_flat(
        &mut self,
        flat_graph: &crate::procedures::gcard_query::flat_graph::FlatGraph,
        statistic: &mut Statistic,
    ) -> anyhow::Result<HashSet<(AltKey, String)>> {
        let t_total = std::time::Instant::now();
        // 返回 dirty_keys，供上层只增量刷新受影响的 compressed catalog 项。
        use crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid;

        // 先把“某个标签还能通过哪些边往外扩展”预解析好。
        // 这里顺手把 CSR 引用也绑定进去，避免热路径里重复做 HashMap 查找。
        let hop_csrs = flat_graph.hop_csrs();
        let pending = flat_graph.pending_ref();

        struct ExtEntry<'a> {
            edge_label: String,
            nbr_label: String,
            outgoing: bool,
            csr: Option<&'a CsrAdjWithEid>,
        }

        let label_to_exts: HashMap<String, Vec<ExtEntry<'_>>> = flat_graph
            .all_labels()
            .filter_map(|label| {
                let raw_exts = flat_graph.edge_extensions_for_label(label);
                if raw_exts.is_empty() {
                    return None;
                }
                let exts: Vec<ExtEntry<'_>> = raw_exts
                    .into_iter()
                    .map(|(edge_label, nbr_label, outgoing)| {
                        let key = (label.to_string(), edge_label.clone(), outgoing);
                        let csr = hop_csrs.get(&key);
                        ExtEntry {
                            edge_label,
                            nbr_label,
                            outgoing,
                            csr,
                        }
                    })
                    .collect();
                Some((label.to_string(), exts))
            })
            .collect();

        let deleted = &self.deleted_vertices;
        let deleted_eids = &pending.deleted_edge_ids;
        let deleted_verts = &pending.deleted_vertices;

        // Pre-compute whether deletion sets are empty to skip checks in hot loop.
        let has_deleted_verts = !deleted.is_empty() || !deleted_verts.is_empty();
        let has_deleted_eids = !deleted_eids.is_empty();

        // ── Phase 1: level-by-level parallel expansion ────────────────────

        let mut current_level: Vec<UpdateEntry> = self.entries.drain(..).collect();
        self.entry_index.clear();
        let mut acc: HashMap<(AltKey, String), HashMap<VertexId, i64>> = HashMap::new();
        let mut total_base_neighbor_visits: usize = 0;
        let mut total_pending_neighbor_visits: usize = 0;

        let max_depth = current_level.iter().map(|e| e.res_len).max().unwrap_or(0);

        let t_expand = std::time::Instant::now();
        for depth in (0..=max_depth).rev() {
            let (this_level, rest): (Vec<_>, Vec<_>) =
                current_level.into_iter().partition(|e| e.res_len == depth);

            if depth == 0 {
                for entry in this_level {
                    let slot = acc.entry((entry.template, entry.label)).or_default();
                    if has_deleted_verts {
                        for (vid, delta) in entry.nodes {
                            if !deleted.contains(&vid) {
                                *slot.entry(vid).or_insert(0) += delta;
                            }
                        }
                    } else {
                        for (vid, delta) in entry.nodes {
                            *slot.entry(vid).or_insert(0) += delta;
                        }
                    }
                }
                current_level = rest;
                continue;
            }

            // Per-entry results collected here, then merged below.
            let expanded: Vec<(
                Vec<UpdateEntry>,
                HashMap<(AltKey, String), HashMap<VertexId, i64>>,
                usize,
                usize,
            )> = this_level
                .into_iter()
                .map(|entry| {
                    let mut local_children: Vec<UpdateEntry> = Vec::new();
                    let mut local_acc: HashMap<(AltKey, String), HashMap<VertexId, i64>> =
                        HashMap::new();

                    let Some(exts) = label_to_exts.get(&entry.label) else {
                        let slot = local_acc.entry((entry.template, entry.label)).or_default();
                        for (vid, delta) in entry.nodes {
                            if !has_deleted_verts || !deleted.contains(&vid) {
                                *slot.entry(vid).or_insert(0) += delta;
                            }
                        }
                        return (local_children, local_acc, 0, 0);
                    };

                    // ── Parallel across extensions within a single entry ──
                    // Each extension independently scans all entry.nodes and
                    // collects neighbor deltas — no shared mutable state.
                    let ext_results: Vec<(
                        Option<UpdateEntry>,
                        usize, // base visits
                        usize, // pending visits
                    )> = exts
                        .par_iter()
                        .map(|ext| {
                            let mut group: HashMap<VertexId, i64> = HashMap::new();
                            let mut base_visits = 0usize;
                            let mut pending_visits = 0usize;

                            for (&vid, &delta) in &entry.nodes {
                                if has_deleted_verts && deleted.contains(&vid) {
                                    continue;
                                }

                                if let Some(csr) = ext.csr {
                                    if has_deleted_eids || has_deleted_verts {
                                        for &(nbr_id, eid) in csr.neighbors_slice(vid) {
                                            base_visits += 1;
                                            if !deleted_eids.contains(&eid)
                                                && !deleted.contains(&nbr_id)
                                                && !deleted_verts.contains(&nbr_id)
                                            {
                                                *group.entry(nbr_id).or_insert(0) += delta;
                                            }
                                        }
                                    } else {
                                        for &(nbr_id, _eid) in csr.neighbors_slice(vid) {
                                            base_visits += 1;
                                            *group.entry(nbr_id).or_insert(0) += delta;
                                        }
                                    }
                                }

                                let idx_key = (vid, ext.edge_label.clone());
                                if ext.outgoing {
                                    if let Some(nbrs) = pending.pending_out.get(&idx_key) {
                                        for &nbr_id in nbrs {
                                            pending_visits += 1;
                                            if !has_deleted_verts
                                                || (!deleted.contains(&nbr_id)
                                                    && !deleted_verts.contains(&nbr_id))
                                            {
                                                *group.entry(nbr_id).or_insert(0) += delta;
                                            }
                                        }
                                    }
                                } else if let Some(nbrs) = pending.pending_in.get(&idx_key) {
                                    for &nbr_id in nbrs {
                                        pending_visits += 1;
                                        if !has_deleted_verts
                                            || (!deleted.contains(&nbr_id)
                                                && !deleted_verts.contains(&nbr_id))
                                        {
                                            *group.entry(nbr_id).or_insert(0) += delta;
                                        }
                                    }
                                }
                            }

                            // Build child entry if non-empty.
                            let nodes: HashMap<VertexId, i64> =
                                group.into_iter().filter(|(_, d)| *d != 0).collect();
                            let child = if nodes.is_empty() {
                                None
                            } else {
                                let mut new_parts =
                                    vec![ext.nbr_label.clone(), ext.edge_label.clone()];
                                new_parts.extend_from_slice(&entry.template.raw);
                                Some(UpdateEntry {
                                    template: AltKey::new(new_parts),
                                    label: ext.nbr_label.clone(),
                                    nodes,
                                    res_len: entry.res_len - 1,
                                })
                            };
                            (child, base_visits, pending_visits)
                        })
                        .collect();

                    let mut total_base = 0usize;
                    let mut total_pending = 0usize;
                    for (child, bv, pv) in ext_results {
                        total_base += bv;
                        total_pending += pv;
                        if let Some(entry) = child {
                            local_children.push(entry);
                        }
                    }

                    (local_children, local_acc, total_base, total_pending)
                })
                .collect();

            let mut next_level = rest;
            for (children, local_acc, local_base_visits, local_pending_visits) in expanded {
                next_level.extend(children);
                total_base_neighbor_visits += local_base_visits;
                total_pending_neighbor_visits += local_pending_visits;
                for ((key, label), nodes) in local_acc {
                    let slot = acc.entry((key, label)).or_default();
                    for (vid, delta) in nodes {
                        *slot.entry(vid).or_insert(0) += delta;
                    }
                }
            }
            current_level = next_level;
        }

        for entry in current_level {
            let slot = acc.entry((entry.template, entry.label)).or_default();
            for (vid, delta) in entry.nodes {
                if !deleted.contains(&vid) {
                    *slot.entry(vid).or_insert(0) += delta;
                }
            }
        }

        eprintln!(
            "[compact] neighbor_expand: {:.2}s ({} groups, {} base nbr visits, {} pending nbr visits)",
            t_expand.elapsed().as_secs_f64(),
            acc.len(),
            total_base_neighbor_visits,
            total_pending_neighbor_visits
        );

        // ── Phase 2: apply net deltas to statistic ─────────────────────────
        let t_apply = std::time::Instant::now();
        let mut dirty_keys: HashSet<(AltKey, String)> = HashSet::new();
        let mut grouped_by_label: HashMap<String, Vec<(AltKey, HashMap<VertexId, i64>)>> =
            HashMap::new();
        for ((altkey, label), nodes) in acc {
            grouped_by_label
                .entry(label.clone())
                .or_default()
                .push((altkey.clone(), nodes));
            dirty_keys.insert((altkey, label));
        }
        let apply_delta_ops = statistic.apply_grouped_deltas(grouped_by_label);
        eprintln!(
            "[compact] apply_delta: {:.2}s ({} ops, {} dirty keys)",
            t_apply.elapsed().as_secs_f64(),
            apply_delta_ops,
            dirty_keys.len()
        );

        // ── Phase 3: remove deleted vertices from statistic ────────────────
        // Collect all (altkey, label) that contain deleted vertices.
        let t_delete = std::time::Instant::now();
        let deleted_vertex_count = self.deleted_vertices.len();
        for &vid in &self.deleted_vertices {
            let affected = statistic.keys_for_vertex(vid);
            for key in affected {
                dirty_keys.insert(key);
            }
            statistic.delete_vertex(vid);
        }
        self.deleted_vertices.clear();
        eprintln!(
            "[compact] delete_vertices: {:.2}s ({} deleted vertices, {} dirty keys)",
            t_delete.elapsed().as_secs_f64(),
            deleted_vertex_count,
            dirty_keys.len()
        );
        eprintln!(
            "[compact] total_stat_update: {:.2}s",
            t_total.elapsed().as_secs_f64()
        );

        Ok(dirty_keys)
    }
}

// ── Convenience constructor for Arc<Mutex<GCardUpdateLog>> ────────────────────

/// Build the type-erased value that [`GraphContainer`] stores.
pub fn new_log_arc(max_k: usize) -> Arc<Mutex<GCardUpdateLog>> {
    Arc::new(Mutex::new(GCardUpdateLog::new(max_k)))
}
