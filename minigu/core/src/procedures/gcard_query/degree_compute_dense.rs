use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use itertools::Itertools;
use minigu_common::types::{LabelId, VertexId};
use minigu_storage::tp::MemTransaction;
use rayon::prelude::*;

use super::catalog::{AltKey, EdgeCardinality};
use super::flat_graph::FlatGraph;
use crate::procedures::gcard_query::utils::{
    EdgeEndpoints, PathPattern, StarStatKey, build_undirected_adj,
};

type VertexIds = Arc<Vec<VertexId>>;

// ----- Public types -----

/// Dense degree data: parallel arrays (vertex_ids[i] has degree degrees[i]).
/// Uses Arc<Vec<VertexId>> to share vertex ID arrays across patterns with the same HopKey.
pub type DegreeSeq = (VertexIds, Vec<u64>);
pub type PathsByLen = HashMap<usize, HashSet<PathPattern>>;
pub type PatternDegCache = HashMap<PathPattern, HashMap<String, DegreeSeq>>;
pub type StarDegCache = HashMap<StarStatKey, DegreeSeq>;

// ----- Neighbor-cached, dependency-driven computation -----

#[derive(Hash, Eq, PartialEq, Clone, Debug, serde::Serialize, serde::Deserialize)]
struct HopKey {
    vertex_label: String,
    edge_label: String,
    outgoing: bool,
}

// ----- Vec-based neighbor data with dense local IDs -----

#[derive(serde::Serialize, serde::Deserialize)]
struct VecNeighborData {
    src_verts: VertexIds,
    /// Reverse mapping: VertexId → index in src_verts. Built once per HopKey.
    src_vid_to_idx: HashMap<VertexId, u32>,
    dst_verts: Vec<VertexId>,
    flat_neighbors: Vec<u32>,
    offsets: Vec<usize>,
}

impl VecNeighborData {
    #[inline]
    fn neighbors_of(&self, i: usize) -> &[u32] {
        &self.flat_neighbors[self.offsets[i]..self.offsets[i + 1]]
    }

    #[inline]
    fn degree_of(&self, i: usize) -> usize {
        self.offsets[i + 1] - self.offsets[i]
    }
}

struct ChunkResult {
    flat: Vec<VertexId>,
    offsets: Vec<usize>,
}

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
            src_vid_to_idx: HashMap::new(),
            src_verts: Arc::new(src_vert_ids),
            dst_verts: Vec::new(),
            flat_neighbors: Vec::new(),
            offsets: vec![0],
        });
    }

    // Phase 1: Parallel scan with buffer reuse per chunk.
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

    // Phase 3: Build dst_to_local mapping via sort+dedup.
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

    // Phase 5: Build src_vid_to_idx (once per HopKey, replaces per-pattern HashMap construction).
    let src_vid_to_idx: HashMap<VertexId, u32> = src_vert_ids
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect();

    Ok(VecNeighborData {
        src_verts: Arc::new(src_vert_ids),
        src_vid_to_idx,
        dst_verts,
        flat_neighbors,
        offsets: global_offsets,
    })
}

// ----- Dense degree computation (no per-pattern HashMap) -----

/// Len-1: degrees[i] = neighbor count of src_verts[i].
/// Per-vertex work is trivial (offset subtraction), so always sequential.
fn degree_vec_from_data(data: &VecNeighborData) -> Vec<u64> {
    (0..data.src_verts.len())
        .map(|i| data.degree_of(i) as u64)
        .collect()
}

/// Len>1: degrees[i] = sum of suffix_degs for neighbors of src_verts[i].
/// `remap[dst_local_id]` maps current hop's dst local ID → suffix hop's src index
/// (u32::MAX means the dst vertex doesn't exist in suffix, contributing 0).
///
/// This loop is memory-bandwidth bound (AI = 0.0078 ops/byte on the Roofline).
/// The CPU's out-of-order execution provides sufficient MLP (~32 outstanding DRAM
/// requests per core via L2 MSHRs) to saturate DRAM bandwidth despite the
/// remap→suffix_degs pointer-chasing dependency within each iteration.
/// Software prefetching cannot help (verified: <3% improvement).
fn extend_vec_with_suffix(data: &VecNeighborData, suffix_degs: &[u64], remap: &[u32]) -> Vec<u64> {
    let n = data.src_verts.len();
    let mut result = Vec::with_capacity(n);
    for i in 0..n {
        let sum: u64 = data
            .neighbors_of(i)
            .iter()
            .map(|&local_id| {
                let suffix_idx = remap[local_id as usize];
                if suffix_idx == u32::MAX {
                    0
                } else {
                    suffix_degs[suffix_idx as usize]
                }
            })
            .sum();
        result.push(sum);
    }
    result
}

// ----- Pattern dependency tracking -----

#[derive(serde::Serialize, serde::Deserialize)]
struct PatternDeps {
    pattern: PathPattern,
    left_hop: HopKey,
    right_hop: HopKey,
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
}

/// Compute HopKey for the first hop of a pattern.
fn hop_key_for_pattern_start(
    pattern: &PathPattern,
    edges: &HashMap<String, EdgeEndpoints>,
) -> HopKey {
    let edge_info = edges.get(&pattern.es[0]).expect("edge not found");
    HopKey {
        vertex_label: pattern.vs[0].clone(),
        edge_label: pattern.es[0].clone(),
        outgoing: edge_info.src_label == pattern.vs[0],
    }
}

fn enumerate_oriented_arms_in_schema(
    edges: &HashMap<String, EdgeEndpoints>,
    max_len: usize,
) -> HashMap<String, Vec<PathPattern>> {
    let vertex_types: HashSet<String> = edges
        .values()
        .flat_map(|e| [e.src_label.as_str(), e.dst_label.as_str()])
        .map(String::from)
        .collect();
    let adj = build_undirected_adj(edges);
    let mut out: HashMap<String, Vec<PathPattern>> = HashMap::new();

    fn dfs(
        center: &str,
        adj: &HashMap<String, Vec<(String, String)>>,
        max_len: usize,
        node_seq: &mut Vec<String>,
        edge_seq: &mut Vec<String>,
        out: &mut HashMap<String, Vec<PathPattern>>,
    ) {
        let cur_len = edge_seq.len();
        if cur_len > 0 {
            out.entry(center.to_string())
                .or_default()
                .push(PathPattern::new_without_reverse(
                    node_seq.clone(),
                    edge_seq.clone(),
                ));
        }
        if cur_len == max_len {
            return;
        }
        let cur_node = node_seq.last().unwrap().clone();
        if let Some(nbrs) = adj.get(&cur_node) {
            for (edge_name, next_node) in nbrs {
                edge_seq.push(edge_name.clone());
                node_seq.push(next_node.clone());
                dfs(center, adj, max_len, node_seq, edge_seq, out);
                node_seq.pop();
                edge_seq.pop();
            }
        }
    }

    for center in vertex_types {
        let mut node_seq = vec![center.clone()];
        let mut edge_seq = Vec::new();
        dfs(
            &center,
            &adj,
            max_len,
            &mut node_seq,
            &mut edge_seq,
            &mut out,
        );
    }

    for arms in out.values_mut() {
        arms.sort_by(|a, b| a.vs.cmp(&b.vs).then(a.es.cmp(&b.es)));
        arms.dedup_by(|a, b| a.vs == b.vs && a.es == b.es);
    }
    out
}

/// Internal dense cache: PathPattern → HopKey → Vec<u64> (indexed by hop's src_verts).
/// Keyed by HopKey (not vertex label name) to avoid collisions when both endpoints
/// of a pattern share the same vertex label but use different hops
/// (e.g., [Person, knows, Person, likes, Person]).
type InternalDegCache = HashMap<PathPattern, HashMap<HopKey, Vec<u64>>>;

/// Compute dense degree vector for one endpoint of a pattern.
fn compute_endpoint_dense(
    oriented: &PathPattern,
    vec_data: &VecNeighborData,
    cache: &InternalDegCache,
    edges: &HashMap<String, EdgeEndpoints>,
    remap_tables: &HashMap<(HopKey, HopKey), Vec<u32>>,
) -> Result<Vec<u64>, anyhow::Error> {
    if oriented.es.len() == 1 {
        return Ok(degree_vec_from_data(vec_data));
    }

    let suffix = oriented.suffix();
    let suffix_canonical = suffix.canonical();
    let current_hop = hop_key_for_pattern_start(oriented, edges);
    let suffix_hop = hop_key_for_pattern_start(&suffix, edges);

    let suffix_degs = cache
        .get(&suffix_canonical)
        .and_then(|m| m.get(&suffix_hop))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "suffix deg missing for hop {:?} in pattern {}",
                suffix_hop,
                suffix
            )
        })?;
    let remap = remap_tables
        .get(&(current_hop, suffix_hop))
        .ok_or_else(|| anyhow::anyhow!("remap table missing for pattern {}", oriented))?;

    Ok(extend_vec_with_suffix(vec_data, suffix_degs, remap))
}

// ----- Prepared scan data -----

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ScannedHops {
    vec_caches: HashMap<HopKey, VecNeighborData>,
    all_deps: Vec<PatternDeps>,
    /// Pre-computed remap tables: (current_hop, suffix_hop) → Vec<u32>.
    /// remap[dst_local_id] = suffix hop's src index, or u32::MAX if not found.
    remap_tables: HashMap<(HopKey, HopKey), Vec<u32>>,
}

impl ScannedHops {
    pub fn save_bincode(&self, path: &std::path::Path) -> Result<(), anyhow::Error> {
        let data = bincode::serialize(self)
            .map_err(|e| anyhow::anyhow!("serialize ScannedHops: {}", e))?;
        std::fs::write(path, &data)
            .map_err(|e| anyhow::anyhow!("write {}: {}", path.display(), e))?;
        eprintln!(
            "ScannedHops saved ({:.2} MB) to {}",
            data.len() as f64 / 1024.0 / 1024.0,
            path.display()
        );
        Ok(())
    }

    pub fn load_bincode(path: &std::path::Path) -> Result<Self, anyhow::Error> {
        let data =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("read {}: {}", path.display(), e))?;
        let scanned: Self = bincode::deserialize(&data)
            .map_err(|e| anyhow::anyhow!("deserialize ScannedHops: {}", e))?;
        eprintln!(
            "ScannedHops loaded ({:.2} MB) from {}",
            data.len() as f64 / 1024.0 / 1024.0,
            path.display()
        );
        Ok(scanned)
    }

    pub fn mem_usage_bytes(&self) -> usize {
        let vec_cache_bytes: usize = self
            .vec_caches
            .values()
            .map(|d| {
                d.src_verts.len() * std::mem::size_of::<VertexId>()
                    + d.dst_verts.len() * std::mem::size_of::<VertexId>()
                    + d.flat_neighbors.len() * std::mem::size_of::<u32>()
                    + d.offsets.len() * std::mem::size_of::<usize>()
                    + d.src_vid_to_idx.len()
                        * (std::mem::size_of::<VertexId>() + std::mem::size_of::<u32>())
            })
            .sum();
        let remap_bytes: usize = self
            .remap_tables
            .values()
            .map(|r| r.len() * std::mem::size_of::<u32>())
            .sum();
        vec_cache_bytes + remap_bytes
    }
}

// ----- Phase 1: Scan all hops -----

pub fn scan_all_hops(
    txn: &Arc<MemTransaction>,
    edges: &HashMap<String, EdgeEndpoints>,
    label_name_to_id: &HashMap<String, LabelId>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
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

    let mut unique_hops: HashSet<HopKey> = HashSet::new();
    for deps in &all_deps {
        unique_hops.insert(deps.left_hop.clone());
        unique_hops.insert(deps.right_hop.clone());
    }

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

    // Phase 6: Pre-compute remap tables for all (current_hop, suffix_hop) pairs.
    // For each len>=2 pattern, we need remaps for left and right endpoint computation.
    let mut remap_pairs: HashSet<(HopKey, HopKey)> = HashSet::new();
    for deps in &all_deps {
        if deps.pattern.es.len() >= 2 {
            // Left endpoint: current = left_hop, suffix = first hop of pattern.suffix()
            let suffix_left = deps.pattern.suffix();
            let suffix_left_hop = hop_key_for_pattern_start(&suffix_left, edges);
            remap_pairs.insert((deps.left_hop.clone(), suffix_left_hop));

            // Right endpoint: current = right_hop, suffix = first hop of reversed.suffix()
            let reversed = deps.pattern.reversed();
            let suffix_right = reversed.suffix();
            let suffix_right_hop = hop_key_for_pattern_start(&suffix_right, edges);
            remap_pairs.insert((deps.right_hop.clone(), suffix_right_hop));
        }
    }

    let mut remap_tables: HashMap<(HopKey, HopKey), Vec<u32>> = HashMap::new();
    for (current_hop, suffix_hop) in &remap_pairs {
        let current_data = vec_caches.get(current_hop).unwrap();
        let suffix_data = vec_caches.get(suffix_hop).unwrap();
        let remap: Vec<u32> = current_data
            .dst_verts
            .iter()
            .map(|v| {
                suffix_data
                    .src_vid_to_idx
                    .get(v)
                    .copied()
                    .unwrap_or(u32::MAX)
            })
            .collect();
        remap_tables.insert((current_hop.clone(), suffix_hop.clone()), remap);
    }

    Ok(ScannedHops {
        vec_caches,
        all_deps,
        remap_tables,
    })
}

// ----- Phase 2: Pure in-memory compute -----

pub fn compute_from_scanned_hops(
    scanned: &ScannedHops,
    edges: &HashMap<String, EdgeEndpoints>,
    max_k: usize,
    compute_pool: &rayon::ThreadPool,
) -> Result<PatternDegCache, anyhow::Error> {
    let (path_cache, _) =
        compute_from_scanned_hops_with_star(scanned, edges, max_k, 0, 0, compute_pool)?;
    Ok(path_cache)
}

pub fn compute_from_scanned_hops_with_star(
    scanned: &ScannedHops,
    edges: &HashMap<String, EdgeEndpoints>,
    max_k: usize,
    max_star_length: usize,
    max_star_degree: usize,
    compute_pool: &rayon::ThreadPool,
) -> Result<(PatternDegCache, StarDegCache), anyhow::Error> {
    let all_deps = &scanned.all_deps;
    let vec_caches = &scanned.vec_caches;
    let remap_tables = &scanned.remap_tables;

    let mut cache: InternalDegCache = HashMap::new();
    let mut computed_patterns: HashSet<PathPattern> = HashSet::new();

    for len in 1..=max_k {
        let level_deps: Vec<&PatternDeps> = all_deps
            .iter()
            .filter(|d| d.len() == len && !computed_patterns.contains(&d.pattern))
            .collect();

        if level_deps.is_empty() {
            continue;
        }

        let cache_ref = &cache;
        let results: Vec<Result<(usize, Vec<u64>, Vec<u64>), anyhow::Error>> = compute_pool
            .install(|| {
                level_deps
                    .par_iter()
                    .enumerate()
                    .map(|(idx, deps)| {
                        let left_vec_data = vec_caches.get(&deps.left_hop).unwrap();
                        let left_deg = compute_endpoint_dense(
                            &deps.pattern,
                            left_vec_data,
                            cache_ref,
                            edges,
                            remap_tables,
                        )?;

                        let reversed = deps.pattern.reversed();
                        let right_vec_data = vec_caches.get(&deps.right_hop).unwrap();
                        let right_deg = compute_endpoint_dense(
                            &reversed,
                            right_vec_data,
                            cache_ref,
                            edges,
                            remap_tables,
                        )?;

                        Ok((idx, left_deg, right_deg))
                    })
                    .collect()
            });

        for result in results {
            let (idx, left_deg, right_deg) = result?;
            let deps = level_deps[idx];
            let canonical = deps.pattern.canonical();
            let entry = cache.entry(canonical.clone()).or_default();
            entry.insert(deps.left_hop.clone(), left_deg);
            entry.insert(deps.right_hop.clone(), right_deg);
            computed_patterns.insert(canonical);
        }
    }

    let star_output = if max_star_length > 0 && max_star_degree > 0 {
        compute_star_degrees_from_internal(
            scanned,
            edges,
            &cache,
            max_star_length,
            max_star_degree,
            compute_pool,
        )?
    } else {
        HashMap::new()
    };

    // Convert InternalDegCache (Vec<u64>) → PatternDegCache (DegreeSeq).
    // Consumes the cache to move degree Vecs (no clone). Uses Arc for shared src_verts.
    let mut output: PatternDegCache = HashMap::with_capacity(cache.len());
    for (pattern, endpoints) in cache {
        let mut out_endpoints: HashMap<String, DegreeSeq> = HashMap::with_capacity(endpoints.len());
        for (hop_key, degrees) in endpoints {
            let vec_data = vec_caches.get(&hop_key).unwrap();
            let vids = Arc::clone(&vec_data.src_verts);
            // Use the hop's vertex_label as the endpoint name for downstream consumers.
            out_endpoints.insert(hop_key.vertex_label.clone(), (vids, degrees));
        }
        output.insert(pattern, out_endpoints);
    }

    Ok((output, star_output))
}

fn compute_star_degrees_from_internal(
    scanned: &ScannedHops,
    edges: &HashMap<String, EdgeEndpoints>,
    cache: &InternalDegCache,
    max_star_length: usize,
    max_star_degree: usize,
    compute_pool: &rayon::ThreadPool,
) -> Result<StarDegCache, anyhow::Error> {
    let arms_by_center = enumerate_oriented_arms_in_schema(edges, max_star_length);
    let mut tasks: Vec<(String, Vec<PathPattern>)> = Vec::new();

    for (center_label, mut arms) in arms_by_center {
        arms.retain(|p| !p.es.is_empty() && p.es.len() <= max_star_length);
        arms.sort_by(|a, b| a.vs.cmp(&b.vs).then(a.es.cmp(&b.es)));
        if !arms.is_empty() {
            tasks.push((center_label, arms));
        }
    }
    tasks.sort_by(|a, b| a.0.cmp(&b.0));

    let per_center: Vec<Result<Vec<(StarStatKey, DegreeSeq)>, anyhow::Error>> = compute_pool
        .install(|| {
            tasks
                .par_iter()
                .map(|(center_label, arms)| {
                    let mut rows = Vec::new();
                    let max_degree = max_star_degree.min(arms.len());
                    for degree in 2..=max_degree {
                        for comb in arms.iter().combinations(degree) {
                            let mut arm_entries = Vec::with_capacity(degree);
                            for arm in &comb {
                                let hop = hop_key_for_pattern_start(arm, edges);
                                let canonical = arm.canonical();
                                let degs = cache
                                    .get(&canonical)
                                    .and_then(|m| m.get(&hop))
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "degree vector missing for star arm {} at hop {:?}",
                                            arm,
                                            hop
                                        )
                                    })?;
                                let data = scanned.vec_caches.get(&hop).ok_or_else(|| {
                                    anyhow::anyhow!("vec cache missing for star hop {:?}", hop)
                                })?;
                                arm_entries.push((arm, data, degs));
                            }

                            let (_, first_data, first_degs) = arm_entries[0];
                            let vertex_ids = Arc::clone(&first_data.src_verts);
                            let mut star_degs = first_degs.clone();
                            for (_, data, degs) in arm_entries.iter().skip(1) {
                                if data.src_verts.as_slice() == vertex_ids.as_slice() {
                                    for (dst, src) in star_degs.iter_mut().zip(degs.iter()) {
                                        *dst = dst.saturating_mul(*src);
                                    }
                                } else {
                                    for (idx, vertex_id) in vertex_ids.iter().enumerate() {
                                        let rhs = data
                                            .src_vid_to_idx
                                            .get(vertex_id)
                                            .map(|i| degs[*i as usize])
                                            .unwrap_or(0);
                                        star_degs[idx] = star_degs[idx].saturating_mul(rhs);
                                    }
                                }
                            }

                            let key = StarStatKey::new(
                                center_label.clone(),
                                comb.into_iter().cloned().collect(),
                            );
                            rows.push((key, (vertex_ids, star_degs)));
                        }
                    }
                    Ok(rows)
                })
                .collect()
        });

    let mut out = HashMap::new();
    for rows in per_center {
        for (key, seq) in rows? {
            out.insert(key, seq);
        }
    }
    eprintln!("Computed PathCE-style star degree sequences: {}", out.len());
    Ok(out)
}

// ----- Combined entry point -----

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

// ----- Raw adjacency-based hop building (no MemoryGraph needed) -----

/// Raw CSR adjacency data for a single hop, built externally (e.g. from CSV).
pub struct RawHopData {
    /// All source vertices (must be sorted ascending).
    pub src_vids: Vec<VertexId>,
    /// Flat neighbor array: neighbors of src_vids[i] are at
    /// `neighbors_flat[offsets[i]..offsets[i+1]]`.
    pub neighbors_flat: Vec<VertexId>,
    /// CSR offsets. Length = src_vids.len() + 1.
    pub offsets: Vec<usize>,
}

fn build_vec_neighbor_data_from_raw(
    raw: RawHopData,
    scan_pool: &rayon::ThreadPool,
) -> VecNeighborData {
    // Build dst_verts (unique sorted destinations)
    let mut sorted_dsts = raw.neighbors_flat.clone();
    sorted_dsts.sort_unstable();
    sorted_dsts.dedup();
    let dst_verts = sorted_dsts;
    let dst_to_local: HashMap<VertexId, u32> = dst_verts
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect();

    // Remap VertexId → local u32
    let flat_neighbors: Vec<u32> = scan_pool.install(|| {
        raw.neighbors_flat
            .par_iter()
            .map(|vid| dst_to_local[vid])
            .collect()
    });

    let src_vid_to_idx: HashMap<VertexId, u32> = raw
        .src_vids
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect();

    VecNeighborData {
        src_verts: Arc::new(raw.src_vids),
        src_vid_to_idx,
        dst_verts,
        flat_neighbors,
        offsets: raw.offsets,
    }
}

/// Build `ScannedHops` from externally-provided raw adjacency data.
///
/// `get_hop(vertex_label, edge_label, outgoing)` must return a `RawHopData`
/// for the requested hop. This allows building the scan cache from CSV files
/// without loading data into `MemoryGraph`.
pub fn scan_all_hops_from_raw(
    get_hop: &dyn Fn(&str, &str, bool) -> RawHopData,
    edges: &HashMap<String, EdgeEndpoints>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
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

    let mut unique_hops: HashSet<HopKey> = HashSet::new();
    for deps in &all_deps {
        unique_hops.insert(deps.left_hop.clone());
        unique_hops.insert(deps.right_hop.clone());
    }

    let mut vec_caches: HashMap<HopKey, VecNeighborData> = HashMap::new();
    for hop in &unique_hops {
        let raw = get_hop(&hop.vertex_label, &hop.edge_label, hop.outgoing);
        let vec_data = build_vec_neighbor_data_from_raw(raw, scan_pool);
        vec_caches.insert(hop.clone(), vec_data);
    }

    // Build remap tables (identical to scan_all_hops)
    let mut remap_pairs: HashSet<(HopKey, HopKey)> = HashSet::new();
    for deps in &all_deps {
        if deps.pattern.es.len() >= 2 {
            let suffix_left = deps.pattern.suffix();
            let suffix_left_hop = hop_key_for_pattern_start(&suffix_left, edges);
            remap_pairs.insert((deps.left_hop.clone(), suffix_left_hop));

            let reversed = deps.pattern.reversed();
            let suffix_right = reversed.suffix();
            let suffix_right_hop = hop_key_for_pattern_start(&suffix_right, edges);
            remap_pairs.insert((deps.right_hop.clone(), suffix_right_hop));
        }
    }

    let mut remap_tables: HashMap<(HopKey, HopKey), Vec<u32>> = HashMap::new();
    for (current_hop, suffix_hop) in &remap_pairs {
        let current_data = vec_caches.get(current_hop).unwrap();
        let suffix_data = vec_caches.get(suffix_hop).unwrap();
        let remap: Vec<u32> = current_data
            .dst_verts
            .iter()
            .map(|v| {
                suffix_data
                    .src_vid_to_idx
                    .get(v)
                    .copied()
                    .unwrap_or(u32::MAX)
            })
            .collect();
        remap_tables.insert((current_hop.clone(), suffix_hop.clone()), remap);
    }

    Ok(ScannedHops {
        vec_caches,
        all_deps,
        remap_tables,
    })
}

// ----- FlatGraph-based hop building -----

fn raw_hop_data_from_flat_csr(
    verts: &[VertexId],
    csr: Option<&crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid>,
) -> RawHopData {
    match csr {
        Some(csr) => {
            let neighbors_vids = csr.neighbors_vids();
            let csr_verts = csr.verts();
            let csr_offsets = csr.offsets();
            let mut offsets = Vec::with_capacity(verts.len() + 1);
            let mut csr_pos = 0usize;
            for &vid in verts {
                while csr_pos < csr_verts.len() && csr_verts[csr_pos] < vid {
                    csr_pos += 1;
                }
                if csr_pos < csr_verts.len() && csr_verts[csr_pos] == vid {
                    offsets.push(csr_offsets[csr_pos]);
                } else {
                    let cur = if csr_pos < csr_verts.len() {
                        csr_offsets[csr_pos]
                    } else {
                        neighbors_vids.len()
                    };
                    offsets.push(cur);
                }
            }
            offsets.push(neighbors_vids.len());
            RawHopData {
                src_vids: verts.to_vec(),
                neighbors_flat: neighbors_vids,
                offsets,
            }
        }
        None => RawHopData {
            src_vids: verts.to_vec(),
            neighbors_flat: Vec::new(),
            offsets: vec![0; verts.len() + 1],
        },
    }
}

/// Build `ScannedHops` directly from a [`FlatGraph`].
///
/// Extracts neighbor VertexIds from FlatGraph's `CsrAdjWithEid` CSR buckets,
/// dropping edge IDs. This avoids building an intermediate LightGraph.
pub fn scan_all_hops_from_flat_graph(
    flat_graph: &FlatGraph,
    edges: &HashMap<String, EdgeEndpoints>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
    scan_all_hops_from_raw(
        &|vertex_label, edge_label, outgoing| {
            let verts = flat_graph.all_vertex_ids_by_label(vertex_label);
            let key = (vertex_label.to_string(), edge_label.to_string(), outgoing);
            raw_hop_data_from_flat_csr(verts, flat_graph.hop_csrs().get(&key))
        },
        edges,
        schema_path,
        max_k,
        scan_pool,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_csr_raw_hop_offsets_preserve_degrees_before_missing_vertices() {
        let csr = crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid::build(vec![
            (1, 10, 100),
            (3, 30, 300),
        ]);
        let raw = raw_hop_data_from_flat_csr(&[1, 2, 3, 4], Some(&csr));

        assert_eq!(raw.src_vids, vec![1, 2, 3, 4]);
        assert_eq!(raw.neighbors_flat, vec![10, 30]);
        assert_eq!(raw.offsets, vec![0, 1, 1, 2, 2]);
    }

    fn vec_neighbor_data(src_verts: Vec<VertexId>) -> VecNeighborData {
        VecNeighborData {
            src_vid_to_idx: src_verts
                .iter()
                .enumerate()
                .map(|(i, &v)| (v, i as u32))
                .collect(),
            src_verts: Arc::new(src_verts),
            dst_verts: Vec::new(),
            flat_neighbors: Vec::new(),
            offsets: vec![0; 3],
        }
    }

    fn one_hop_arm(center: &str, edge: &str, leaf: &str) -> PathPattern {
        PathPattern::new_without_reverse(
            vec![center.to_string(), leaf.to_string()],
            vec![edge.to_string()],
        )
    }

    #[test]
    fn pathce_style_star_degree_sequences_multiply_distinct_rooted_arms() {
        let center = "a".to_string();
        let vertex_ids = vec![10, 20];
        let arm_specs = [
            ("ab", "b", vec![2, 3]),
            ("ac", "c", vec![5, 7]),
            ("ad", "d", vec![11, 13]),
        ];

        let mut edges = HashMap::new();
        let mut vec_caches = HashMap::new();
        let mut cache: InternalDegCache = HashMap::new();
        let mut arms = Vec::new();

        for (edge_label, leaf_label, degrees) in arm_specs {
            edges.insert(
                edge_label.to_string(),
                EdgeEndpoints {
                    src_label: center.clone(),
                    dst_label: leaf_label.to_string(),
                    cardinality: EdgeCardinality::ManyToMany,
                },
            );

            let arm = one_hop_arm(&center, edge_label, leaf_label);
            let hop = hop_key_for_pattern_start(&arm, &edges);
            vec_caches.insert(hop.clone(), vec_neighbor_data(vertex_ids.clone()));
            cache
                .entry(arm.canonical())
                .or_default()
                .insert(hop, degrees);

            let reverse_arm = one_hop_arm(leaf_label, edge_label, &center);
            let reverse_hop = hop_key_for_pattern_start(&reverse_arm, &edges);
            vec_caches.insert(reverse_hop.clone(), vec_neighbor_data(vec![100, 200]));
            cache
                .entry(reverse_arm.canonical())
                .or_default()
                .insert(reverse_hop, vec![1, 1]);
            arms.push(arm);
        }

        let scanned = ScannedHops {
            vec_caches,
            all_deps: Vec::new(),
            remap_tables: HashMap::new(),
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();

        let star_cache =
            compute_star_degrees_from_internal(&scanned, &edges, &cache, 1, 3, &pool).unwrap();

        let degree_three_key = StarStatKey::new(center.clone(), arms.clone());
        let (actual_vertices, actual_degrees) = star_cache.get(&degree_three_key).unwrap();
        assert_eq!(actual_vertices.as_slice(), vertex_ids.as_slice());
        assert_eq!(actual_degrees, &vec![2 * 5 * 11, 3 * 7 * 13]);

        let repeated_arm_key = StarStatKey::new(
            center,
            vec![arms[0].clone(), arms[0].clone(), arms[0].clone()],
        );
        assert!(
            !star_cache.contains_key(&repeated_arm_key),
            "PathCE-style combinations should not generate repeated-arm stars"
        );
    }
}
