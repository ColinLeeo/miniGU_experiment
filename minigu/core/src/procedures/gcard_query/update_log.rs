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
    /// Path template; `label` is always the leftmost vertex.
    pub template: AltKey,
    /// Label of the endpoint nodes recorded in `nodes`.
    pub label: String,
    /// Vertex ID → degree delta.  +1 for insertion, -1 for deletion.
    pub nodes: HashMap<VertexId, i64>,
    /// Remaining expansion depth.  0 means this entry is final (no graph traversal needed
    /// during the apply phase).  Created entries start at `max_k - 1`.
    pub res_len: usize,
}

// ── Log ───────────────────────────────────────────────────────────────────────

/// Pending GCard update log stored inside [`GraphContainer`] as
/// `Arc<Mutex<GCardUpdateLog>>` (type-erased).
#[derive(Debug, Default)]
pub struct GCardUpdateLog {
    /// All pending update entries, in arrival order.
    pub entries: Vec<UpdateEntry>,
    /// Max path length K used when building statistics; determines initial `res_len`.
    pub max_k: usize,
    /// Vertices deleted since the last compact.  Their statistics will be completely
    /// removed from [`Statistic`] during [`compact_and_apply`], and they are skipped
    /// during graph traversal.
    pub deleted_vertices: HashSet<VertexId>,
}

impl GCardUpdateLog {
    pub fn new(max_k: usize) -> Self {
        Self {
            entries: Vec::new(),
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
            let template = AltKey::new(vec![
                neighbor_label.clone(),
                edge_label,
                u_label.to_string(),
            ]);
            let nodes: HashMap<VertexId, i64> =
                neighbor_ids.into_iter().map(|id| (id, -1)).collect();
            self.entries.push(UpdateEntry {
                template,
                label: neighbor_label,
                nodes,
                res_len,
            });
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    fn push_edge_entries(
        &mut self,
        u: VertexId,
        u_label: &str,
        v: VertexId,
        v_label: &str,
        edge_label: &str,
        delta: i64,
    ) {
        let res_len = self.max_k.saturating_sub(1);

        self.entries.push(UpdateEntry {
            template: AltKey::new(vec![
                u_label.to_string(),
                edge_label.to_string(),
                v_label.to_string(),
            ]),
            label: u_label.to_string(),
            nodes: HashMap::from([(u, delta)]),
            res_len,
        });

        self.entries.push(UpdateEntry {
            template: AltKey::new(vec![
                v_label.to_string(),
                edge_label.to_string(),
                u_label.to_string(),
            ]),
            label: v_label.to_string(),
            nodes: HashMap::from([(v, delta)]),
            res_len,
        });
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
        use crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid;

        // ── Build per-label extension table with pre-resolved CSR refs ────
        // label → [(edge_label, neighbor_label, outgoing, Option<&CsrAdjWithEid>)]
        //
        // Pre-resolving CSR references avoids per-call String allocation +
        // HashMap lookup in the hot inner loop (was 98% of CPU time).
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

        // Inline neighbor lookup — no String allocation, no HashMap lookup.
        // Uses pre-resolved CSR reference + pending index.
        let get_neighbors = |vid: VertexId, ext: &ExtEntry<'_>| -> Vec<VertexId> {
            if deleted_verts.contains(&vid) {
                return Vec::new();
            }
            let mut result = Vec::new();
            // Base CSR
            if let Some(csr) = ext.csr {
                for &(nbr, eid) in csr.neighbors_slice(vid) {
                    if !deleted_eids.contains(&eid) && !deleted_verts.contains(&nbr) {
                        result.push(nbr);
                    }
                }
            }
            // Pending inserted edges (O(1) index lookup)
            let idx_key = (vid, ext.edge_label.clone());
            if ext.outgoing {
                if let Some(nbrs) = pending.pending_out.get(&idx_key) {
                    for &dst in nbrs {
                        if !deleted_verts.contains(&dst) {
                            result.push(dst);
                        }
                    }
                }
            } else if let Some(nbrs) = pending.pending_in.get(&idx_key) {
                for &src in nbrs {
                    if !deleted_verts.contains(&src) {
                        result.push(src);
                    }
                }
            }
            result
        };

        // ── Phase 1: level-by-level parallel expansion ────────────────────

        let mut current_level: Vec<UpdateEntry> = self.entries.drain(..).collect();
        let mut acc: HashMap<(AltKey, String), HashMap<VertexId, i64>> = HashMap::new();

        let max_depth = current_level.iter().map(|e| e.res_len).max().unwrap_or(0);

        for depth in (0..=max_depth).rev() {
            let (this_level, rest): (Vec<_>, Vec<_>) =
                current_level.into_iter().partition(|e| e.res_len == depth);

            if depth == 0 {
                for entry in this_level {
                    let slot = acc.entry((entry.template, entry.label)).or_default();
                    for (vid, delta) in entry.nodes {
                        if !deleted.contains(&vid) {
                            *slot.entry(vid).or_insert(0) += delta;
                        }
                    }
                }
                current_level = rest;
                continue;
            }

            let expanded: Vec<(
                Vec<UpdateEntry>,
                HashMap<(AltKey, String), HashMap<VertexId, i64>>,
            )> = this_level
                .into_par_iter()
                .map(|entry| {
                    let mut local_children: Vec<UpdateEntry> = Vec::new();
                    let mut local_acc: HashMap<(AltKey, String), HashMap<VertexId, i64>> =
                        HashMap::new();

                    let Some(exts) = label_to_exts.get(&entry.label) else {
                        let slot = local_acc.entry((entry.template, entry.label)).or_default();
                        for (vid, delta) in entry.nodes {
                            if !deleted.contains(&vid) {
                                *slot.entry(vid).or_insert(0) += delta;
                            }
                        }
                        return (local_children, local_acc);
                    };

                    let mut new_groups: HashMap<(String, String), HashMap<VertexId, i64>> =
                        HashMap::new();
                    for (&vid, &delta) in &entry.nodes {
                        if deleted.contains(&vid) {
                            continue;
                        }
                        for ext in exts {
                            let nbrs = get_neighbors(vid, ext);
                            for nbr_id in nbrs {
                                if !deleted.contains(&nbr_id) {
                                    *new_groups
                                        .entry((ext.nbr_label.clone(), ext.edge_label.clone()))
                                        .or_default()
                                        .entry(nbr_id)
                                        .or_insert(0) += delta;
                                }
                            }
                        }
                    }

                    for ((nbr_label, edge_name), nodes) in new_groups {
                        let nodes: HashMap<VertexId, i64> =
                            nodes.into_iter().filter(|(_, d)| *d != 0).collect();
                        if nodes.is_empty() {
                            continue;
                        }
                        let mut new_parts = vec![nbr_label.clone(), edge_name];
                        new_parts.extend_from_slice(&entry.template.raw);
                        local_children.push(UpdateEntry {
                            template: AltKey::new(new_parts),
                            label: nbr_label,
                            nodes,
                            res_len: entry.res_len - 1,
                        });
                    }

                    (local_children, local_acc)
                })
                .collect();

            let mut next_level = rest;
            for (children, local_acc) in expanded {
                next_level.extend(children);
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
            "[compact] expanded into {} (altkey,label) groups",
            acc.len()
        );

        // ── Phase 2: apply net deltas to statistic ─────────────────────────
        let mut dirty_keys: HashSet<(AltKey, String)> = HashSet::new();
        for ((altkey, label), nodes) in acc {
            for (vertex_id, delta) in &nodes {
                if *delta != 0 {
                    statistic.apply_delta(&label, &altkey, *vertex_id, *delta);
                }
            }
            dirty_keys.insert((altkey, label));
        }

        // ── Phase 3: remove deleted vertices from statistic ────────────────
        // Collect all (altkey, label) that contain deleted vertices.
        for &vid in &self.deleted_vertices {
            let affected = statistic.keys_for_vertex(vid);
            for key in affected {
                dirty_keys.insert(key);
            }
            statistic.delete_vertex(vid);
        }
        self.deleted_vertices.clear();

        Ok(dirty_keys)
    }
}

// ── Convenience constructor for Arc<Mutex<GCardUpdateLog>> ────────────────────

/// Build the type-erased value that [`GraphContainer`] stores.
pub fn new_log_arc(max_k: usize) -> Arc<Mutex<GCardUpdateLog>> {
    Arc::new(Mutex::new(GCardUpdateLog::new(max_k)))
}
