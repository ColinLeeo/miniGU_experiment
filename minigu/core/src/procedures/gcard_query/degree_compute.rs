use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use minigu_common::types::{LabelId, VertexId};
use minigu_storage::tp::MemTransaction;
use rayon::prelude::*;

use super::catalog::AltKey;
use crate::procedures::gcard_query::utils::{EdgeEndpoints, PathPattern};

// ----- Cache types -----

type DegreeMap = HashMap<VertexId, u64>;
pub type PathsByLen = HashMap<usize, HashSet<PathPattern>>;
pub type PatternDegCache = HashMap<PathPattern, HashMap<String, DegreeMap>>;

// ----- Neighbor-cached, dependency-driven computation -----

/// Key for a one-hop neighbor cache entry.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct HopKey {
    vertex_label: String,
    edge_label: String,
    outgoing: bool,
}

// ----- Vec-based compute (local ID remapping for cache-friendly access) -----

/// Pre-scanned neighbor data with dense local IDs for cache-friendly access.
/// Uses a flat layout: all neighbor local-IDs are stored in one contiguous `Vec<u32>`,
/// with `offsets[i]..offsets[i+1]` giving the slice for `src_verts[i]`.
/// This reduces N small Vec allocations to one large allocation and improves cache locality.
struct VecNeighborData {
    /// Source vertex IDs; index i corresponds to flat_neighbors[offsets[i]..offsets[i+1]].
    src_verts: Vec<VertexId>,
    /// Unique destination vertex IDs; index = local dst ID used in flat_neighbors.
    dst_verts: Vec<VertexId>,
    /// All neighbor local-IDs concatenated.
    flat_neighbors: Vec<u32>,
    /// offsets[i]..offsets[i+1] is the neighbor range for src_verts[i]. Length = src_verts.len() +
    /// 1.
    offsets: Vec<usize>,
}

impl VecNeighborData {
    /// Returns the neighbor slice (local dst IDs) for the i-th source vertex.
    #[inline]
    fn neighbors_of(&self, i: usize) -> &[u32] {
        &self.flat_neighbors[self.offsets[i]..self.offsets[i + 1]]
    }

    /// Returns the degree (neighbor count) of the i-th source vertex.
    #[inline]
    fn degree_of(&self, i: usize) -> usize {
        self.offsets[i + 1] - self.offsets[i]
    }
}

/// Per-chunk intermediate result from parallel neighbor scan.
struct ChunkResult {
    /// Flat neighbor vertex IDs for this chunk.
    flat: Vec<VertexId>,
    /// Per-vertex offsets within `flat`. Length = chunk_size + 1.
    offsets: Vec<usize>,
}

/// Scan the graph once for a given (vertex_label, edge_label, direction) triple and
/// return compact VecNeighborData ready for degree computation.
/// Uses `par_chunks` with buffer reuse to minimize per-vertex allocation overhead.
fn build_hop_data(
    txn: &Arc<MemTransaction>,
    vertex_label_id: LabelId,
    edge_label_id: LabelId,
    outgoing: bool,
    scan_pool: &rayon::ThreadPool,
) -> Result<VecNeighborData, anyhow::Error> {
    let src_vert_ids: Vec<VertexId> = txn.raw_vertex_ids_by_label(vertex_label_id);

    if src_vert_ids.is_empty() {
        return Ok(VecNeighborData {
            src_verts: src_vert_ids,
            dst_verts: Vec::new(),
            flat_neighbors: Vec::new(),
            offsets: vec![0],
        });
    }

    // Phase 1: Parallel scan with buffer reuse per chunk.
    // Each chunk reuses a single `buf` Vec across all its vertices, appending into a flat buffer.
    let chunk_size = (src_vert_ids.len() / rayon::current_num_threads().max(1)).max(256);
    let chunk_results: Vec<ChunkResult> = scan_pool.install(|| {
        src_vert_ids
            .par_chunks(chunk_size)
            .map(|chunk| {
                let mut flat = Vec::new();
                let mut offsets = Vec::with_capacity(chunk.len() + 1);
                let mut buf: Vec<VertexId> = Vec::new();
                for &vid in chunk {
                    buf.clear();
                    txn.raw_neighbors_by_edge_into(vid, edge_label_id, outgoing, &mut buf);
                    offsets.push(flat.len());
                    flat.extend_from_slice(&buf);
                }
                offsets.push(flat.len());
                ChunkResult { flat, offsets }
            })
            .collect()
    });

    // Phase 2: Merge chunks into global flat layout.
    let total_neighbors: usize = chunk_results.iter().map(|c| c.flat.len()).sum();
    let mut global_flat = Vec::with_capacity(total_neighbors);
    let mut global_offsets = Vec::with_capacity(src_vert_ids.len() + 1);

    for chunk in &chunk_results {
        let base = global_flat.len();
        for &off in &chunk.offsets[..chunk.offsets.len() - 1] {
            global_offsets.push(base + off);
        }
        global_flat.extend_from_slice(&chunk.flat);
    }
    global_offsets.push(global_flat.len());
    drop(chunk_results);

    // Phase 3: Build dst_to_local mapping via sort+dedup (cache-friendly, no HashSet overhead).
    let mut sorted_dsts = global_flat.clone();
    sorted_dsts.sort_unstable();
    sorted_dsts.dedup();
    let dst_verts = sorted_dsts;
    let dst_to_local: HashMap<VertexId, u32> = dst_verts
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect();

    // Phase 4: Remap VertexId -> local u32 in parallel.
    let flat_neighbors: Vec<u32> = scan_pool.install(|| {
        global_flat
            .par_iter()
            .map(|vid| dst_to_local[vid])
            .collect()
    });

    Ok(VecNeighborData {
        src_verts: src_vert_ids,
        dst_verts,
        flat_neighbors,
        offsets: global_offsets,
    })
}

/// Vec-based degree count: just count neighbors per source vertex.
fn degree_map_from_vec_data(data: &VecNeighborData) -> DegreeMap {
    data.src_verts
        .iter()
        .enumerate()
        .map(|(i, &vid)| (vid, data.degree_of(i) as u64))
        .collect()
}

fn extend_with_vec_data(data: &VecNeighborData, suffix_deg: &DegreeMap) -> DegreeMap {
    // Build dense suffix_deg Vec indexed by local dst ID (one-time, cheap).
    let suffix_vec: Vec<u64> = data
        .dst_verts
        .iter()
        .map(|v| suffix_deg.get(v).copied().unwrap_or(0))
        .collect();

    let n = data.src_verts.len();
    let mut degrees = vec![0u64; n];
    for i in 0..n {
        degrees[i] = data
            .neighbors_of(i)
            .iter()
            .map(|&local_id| suffix_vec[local_id as usize])
            .sum();
    }

    let mut result = HashMap::with_capacity(n);
    for (i, &vid) in data.src_verts.iter().enumerate() {
        result.insert(vid, degrees[i]);
    }
    result
}

// ----- Pattern dependency tracking -----

/// Pre-computed dependency info for a single PathPattern.
struct PatternDeps {
    pattern: PathPattern,
    left_hop: HopKey,
    right_hop: HopKey,
    /// Canonical suffix patterns that must be computed before this pattern (empty for len-1).
    suffix_keys: HashSet<PathPattern>,
}

impl PatternDeps {
    fn new(pattern: &PathPattern, edges: &HashMap<String, EdgeEndpoints>) -> Self {
        let left_hop = {
            let edge_info = edges.get(&pattern.es[0]).expect("edge not found");
            HopKey {
                vertex_label: pattern.vs[0].clone(),
                edge_label: pattern.es[0].clone(),
                outgoing: edge_info.src_label == pattern.vs[0],
            }
        };
        let right_hop = {
            let last_edge = pattern.es.last().unwrap();
            let last_vertex = pattern.vs.last().unwrap();
            let edge_info = edges.get(last_edge).expect("edge not found");
            HopKey {
                vertex_label: last_vertex.clone(),
                edge_label: last_edge.clone(),
                outgoing: edge_info.src_label == *last_vertex,
            }
        };

        let mut suffix_keys = HashSet::new();
        if pattern.es.len() >= 2 {
            suffix_keys.insert(pattern.suffix().canonical());
            suffix_keys.insert(pattern.reversed().suffix().canonical());
        }

        PatternDeps {
            pattern: pattern.clone(),
            left_hop,
            right_hop,
            suffix_keys,
        }
    }

    fn len(&self) -> usize {
        self.pattern.es.len()
    }

    fn is_ready(
        &self,
        vec_caches: &HashMap<HopKey, VecNeighborData>,
        computed: &HashSet<PathPattern>,
    ) -> bool {
        vec_caches.contains_key(&self.left_hop)
            && vec_caches.contains_key(&self.right_hop)
            && self.suffix_keys.iter().all(|sk| computed.contains(sk))
    }

    fn hop_keys(&self) -> [&HopKey; 2] {
        [&self.left_hop, &self.right_hop]
    }
}

/// Compute degree map for one endpoint of a pattern.
/// `oriented` is the pattern oriented so that this endpoint is the first vertex.
/// For len-1, just count neighbors; for longer patterns, extend with the suffix's cached degrees.
fn compute_endpoint_deg(
    oriented: &PathPattern,
    vec_data: &VecNeighborData,
    cache: &PatternDegCache,
) -> Result<DegreeMap, anyhow::Error> {
    if oriented.es.len() == 1 {
        return Ok(degree_map_from_vec_data(vec_data));
    }
    let suffix = oriented.suffix();
    let suffix_key = &suffix.vs[0];
    let suffix_deg = cache
        .get(&suffix.canonical())
        .and_then(|m| m.get(suffix_key))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "suffix deg missing for {} in pattern {}",
                suffix_key,
                suffix
            )
        })?;
    Ok(extend_with_vec_data(vec_data, suffix_deg))
}

// ----- Prepared scan data (opaque handle for two-phase API) -----

/// All pre-scanned neighbor data and dependency info, ready for pure in-memory compute.
/// Returned by `scan_all_hops` and consumed by `compute_from_scanned_hops`.
pub struct ScannedHops {
    vec_caches: HashMap<HopKey, VecNeighborData>,
    all_deps: Vec<PatternDeps>,
}

impl ScannedHops {
    /// Approximate memory usage of the scanned neighbor data (bytes).
    pub fn mem_usage_bytes(&self) -> usize {
        self.vec_caches
            .values()
            .map(|d| {
                d.src_verts.len() * std::mem::size_of::<VertexId>()
                    + d.dst_verts.len() * std::mem::size_of::<VertexId>()
                    + d.flat_neighbors.len() * std::mem::size_of::<u32>()
                    + d.offsets.len() * std::mem::size_of::<usize>()
            })
            .sum()
    }
}

// ----- Phase 1: Scan all hops -----

/// Scan the graph to build all neighbor data needed for degree computation.
/// After this returns, the graph is no longer needed and can be released.
pub fn scan_all_hops(
    txn: &Arc<MemTransaction>,
    edges: &HashMap<String, EdgeEndpoints>,
    label_name_to_id: &HashMap<String, LabelId>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
    // Collect all unique patterns and build their dependency info.
    let mut seen_alt_keys: HashSet<AltKey> = HashSet::new();
    let mut all_deps: Vec<PatternDeps> = Vec::new();
    for len in 1..=max_k {
        if let Some(patterns) = schema_path.get(&len) {
            for p in patterns {
                if seen_alt_keys.insert(p.to_alt_key()) {
                    all_deps.push(PatternDeps::new(p, edges));
                }
            }
        }
    }

    // Collect all unique HopKeys needed.
    let mut unique_hops: HashSet<HopKey> = HashSet::new();
    for deps in &all_deps {
        unique_hops.insert(deps.left_hop.clone());
        unique_hops.insert(deps.right_hop.clone());
    }

    // Scan graph once per HopKey.
    let mut vec_caches: HashMap<HopKey, VecNeighborData> = HashMap::new();
    for hop in &unique_hops {
        let vertex_label_id = *label_name_to_id
            .get(&hop.vertex_label)
            .ok_or_else(|| anyhow::anyhow!("vertex label not found: {}", hop.vertex_label))?;
        let edge_label_id = *label_name_to_id
            .get(&hop.edge_label)
            .ok_or_else(|| anyhow::anyhow!("edge label not found: {}", hop.edge_label))?;
        let vec_data =
            build_hop_data(txn, vertex_label_id, edge_label_id, hop.outgoing, scan_pool)?;
        vec_caches.insert(hop.clone(), vec_data);
    }

    Ok(ScannedHops {
        vec_caches,
        all_deps,
    })
}

// ----- Phase 2: Pure in-memory compute -----

/// Compute degree caches from pre-scanned neighbor data (no graph access needed).
/// Uses two-level parallelism: pattern-level `par_iter` + vertex-level `into_par_iter`
/// within large patterns, both using the same `compute_pool` via rayon work-stealing.
pub fn compute_from_scanned_hops(
    scanned: &ScannedHops,
    edges: &HashMap<String, EdgeEndpoints>,
    max_k: usize,
    compute_pool: &rayon::ThreadPool,
) -> Result<PatternDegCache, anyhow::Error> {
    let all_deps = &scanned.all_deps;
    let vec_caches = &scanned.vec_caches;

    let mut cache: PatternDegCache = HashMap::new();
    let mut computed_patterns: HashSet<PathPattern> = HashSet::new();

    // Process patterns level by level (length 1, 2, ..., max_k).
    // Within each level, patterns are independent and processed in parallel.
    // Inside each pattern, large vertex sets use nested par_iter via work-stealing.
    for len in 1..=max_k {
        let level_deps: Vec<&PatternDeps> = all_deps
            .iter()
            .filter(|d| d.len() == len && !computed_patterns.contains(&d.pattern))
            .collect();

        if level_deps.is_empty() {
            continue;
        }

        let cache_ref = &cache;
        let results: Vec<Result<(usize, DegreeMap, DegreeMap), anyhow::Error>> = compute_pool
            .install(|| {
                level_deps
                    .par_iter()
                    .enumerate()
                    .map(|(idx, deps)| {
                        let left_vec_data = vec_caches.get(&deps.left_hop).unwrap();
                        let left_deg =
                            compute_endpoint_deg(&deps.pattern, left_vec_data, cache_ref)?;

                        let reversed = deps.pattern.reversed();
                        let right_vec_data = vec_caches.get(&deps.right_hop).unwrap();
                        let right_deg = compute_endpoint_deg(&reversed, right_vec_data, cache_ref)?;

                        Ok((idx, left_deg, right_deg))
                    })
                    .collect()
            });

        for result in results {
            let (idx, left_deg, right_deg) = result?;
            let deps = level_deps[idx];
            let canonical = deps.pattern.canonical();
            let entry = cache.entry(canonical.clone()).or_default();
            entry.insert(deps.pattern.vs[0].clone(), left_deg);
            entry.insert(deps.pattern.vs.last().unwrap().clone(), right_deg);
            computed_patterns.insert(canonical);
        }
    }

    Ok(cache)
}

// ----- Combined entry point (backward-compatible) -----

/// Compute degree caches for all schema path patterns up to `max_k` hops.
/// Combines scan + compute in one call. Use `scan_all_hops` + `compute_from_scanned_hops`
/// for separate timing or to release the graph between phases.
pub fn compute_all_degrees_cached(
    txn: &Arc<MemTransaction>,
    edges: &HashMap<String, EdgeEndpoints>,
    label_name_to_id: &HashMap<String, LabelId>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<PatternDegCache, anyhow::Error> {
    let scanned = scan_all_hops(txn, edges, label_name_to_id, schema_path, max_k, scan_pool)?;
    compute_from_scanned_hops(&scanned, edges, max_k, scan_pool)
}
