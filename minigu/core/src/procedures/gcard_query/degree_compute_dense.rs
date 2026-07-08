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

fn cmp_hop_key(a: &HopKey, b: &HopKey) -> std::cmp::Ordering {
    a.vertex_label
        .cmp(&b.vertex_label)
        .then_with(|| a.edge_label.cmp(&b.edge_label))
        .then_with(|| a.outgoing.cmp(&b.outgoing))
}

fn cmp_hop_pair(a: &(HopKey, HopKey), b: &(HopKey, HopKey)) -> std::cmp::Ordering {
    cmp_hop_key(&a.0, &b.0).then_with(|| cmp_hop_key(&a.1, &b.1))
}

fn cmp_pattern_deps(a: &PatternDeps, b: &PatternDeps) -> std::cmp::Ordering {
    a.pattern
        .to_string()
        .cmp(&b.pattern.to_string())
        .then_with(|| cmp_hop_key(&a.left_hop, &b.left_hop))
        .then_with(|| cmp_hop_key(&a.right_hop, &b.right_hop))
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
type RemapKey = (HopKey, HopKey);

/// Compute dense degree vector for one endpoint of a pattern.
fn compute_endpoint_dense(
    oriented: &PathPattern,
    vec_data: &VecNeighborData,
    cache: &InternalDegCache,
    edges: &HashMap<String, EdgeEndpoints>,
    remap_tables: &HashMap<RemapKey, Vec<u32>>,
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
    let remap_key = (current_hop, suffix_hop);
    let remap = remap_tables
        .get(&remap_key)
        .ok_or_else(|| anyhow::anyhow!("remap table missing for pattern {}", oriented))?;

    Ok(extend_vec_with_suffix(vec_data, suffix_degs, remap))
}

// ----- Prepared scan data -----

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ScannedHops {
    vec_caches: HashMap<HopKey, VecNeighborData>,
    all_deps: Vec<PatternDeps>,
    /// Pre-computed remap tables: (current_hop, suffix_hop) -> Vec<u32>.
    /// remap[dst_local_id] = suffix hop's src index, or u32::MAX if not found.
    remap_tables: HashMap<RemapKey, Vec<u32>>,
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

fn collect_remap_pairs(
    all_deps: &[PatternDeps],
    edges: &HashMap<String, EdgeEndpoints>,
) -> Vec<(HopKey, HopKey)> {
    let mut remap_pairs: HashSet<(HopKey, HopKey)> = HashSet::new();
    for deps in all_deps {
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
    let mut remap_pairs: Vec<_> = remap_pairs.into_iter().collect();
    remap_pairs.sort_by(cmp_hop_pair);
    remap_pairs
}

fn remap_parallel_enabled() -> bool {
    std::env::var("GCARD_REMAP_PARALLEL")
        .ok()
        .and_then(|s| match s.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Some(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

fn remap_inner_parallel_enabled() -> bool {
    std::env::var("GCARD_REMAP_INNER_PARALLEL")
        .ok()
        .and_then(|s| match s.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Some(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

fn remap_inner_parallel_threshold() -> usize {
    std::env::var("GCARD_REMAP_INNER_PARALLEL_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1_000_000)
}

fn fingerprint_mix(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(0x100000001b3);
}

fn fingerprint_str(hash: &mut u64, value: &str) {
    for &b in value.as_bytes() {
        fingerprint_mix(hash, b as u64);
    }
    fingerprint_mix(hash, 0xff);
}

fn fingerprint_hop_key(hash: &mut u64, hop: &HopKey) {
    fingerprint_str(hash, &hop.vertex_label);
    fingerprint_str(hash, &hop.edge_label);
    fingerprint_mix(hash, hop.outgoing as u64);
}

fn remap_tables_fingerprint(remap_tables: &HashMap<RemapKey, Vec<u32>>) -> u64 {
    let mut rows: Vec<_> = remap_tables.iter().collect();
    rows.sort_by(|(a, _), (b, _)| cmp_hop_pair(a, b));

    let mut hash = 0xcbf29ce484222325u64;
    for ((current_hop, suffix_hop), remap) in rows {
        fingerprint_hop_key(&mut hash, current_hop);
        fingerprint_hop_key(&mut hash, suffix_hop);
        fingerprint_mix(&mut hash, remap.len() as u64);
        for &idx in remap {
            fingerprint_mix(&mut hash, idx as u64);
        }
    }
    hash
}

#[derive(Clone)]
struct RemapWorkItem {
    current_hop: HopKey,
    suffix_hop: HopKey,
}

fn build_one_remap(
    current_hop: &HopKey,
    suffix_hop: &HopKey,
    vec_caches: &HashMap<HopKey, VecNeighborData>,
) -> Result<(Vec<u32>, RemapProfile), anyhow::Error> {
    let key_start = std::time::Instant::now();
    let current_data = vec_caches
        .get(current_hop)
        .ok_or_else(|| anyhow::anyhow!("current hop not found: {:?}", current_hop))?;
    let suffix_data = vec_caches
        .get(suffix_hop)
        .ok_or_else(|| anyhow::anyhow!("suffix hop not found: {:?}", suffix_hop))?;
    let suffix_src_vid_to_idx = &suffix_data.src_vid_to_idx;
    let entry_count = current_data.dst_verts.len();
    let use_inner_parallel = remap_inner_parallel_enabled()
        && entry_count >= remap_inner_parallel_threshold()
        && rayon::current_num_threads() > 1;

    let (remap, matched_count) = if use_inner_parallel {
        let chunk_size =
            (entry_count / rayon::current_num_threads().max(1) / 4).clamp(16 * 1024, 256 * 1024);
        let chunks: Vec<(Vec<u32>, usize)> = current_data
            .dst_verts
            .par_chunks(chunk_size)
            .map(|chunk| {
                let mut out = Vec::with_capacity(chunk.len());
                let mut matched = 0usize;
                for v in chunk {
                    match suffix_src_vid_to_idx.get(v).copied() {
                        Some(idx) => {
                            matched += 1;
                            out.push(idx);
                        }
                        None => out.push(u32::MAX),
                    }
                }
                (out, matched)
            })
            .collect();

        let mut remap = Vec::with_capacity(entry_count);
        let mut matched_count = 0usize;
        for (chunk, matched) in chunks {
            matched_count += matched;
            remap.extend(chunk);
        }
        (remap, matched_count)
    } else {
        let mut matched_count = 0usize;
        let remap: Vec<u32> = current_data
            .dst_verts
            .iter()
            .map(|v| match suffix_src_vid_to_idx.get(v).copied() {
                Some(idx) => {
                    matched_count += 1;
                    idx
                }
                None => u32::MAX,
            })
            .collect();
        (remap, matched_count)
    };
    let profile = RemapProfile {
        current_hop: current_hop.clone(),
        suffix_hop: suffix_hop.clone(),
        entry_count,
        matched_count,
        build_time_s: key_start.elapsed().as_secs_f64(),
    };
    Ok((remap, profile))
}

fn build_remap_tables(
    all_deps: &[PatternDeps],
    vec_caches: &HashMap<HopKey, VecNeighborData>,
    edges: &HashMap<String, EdgeEndpoints>,
    scan_pool: &rayon::ThreadPool,
) -> Result<HashMap<RemapKey, Vec<u32>>, anyhow::Error> {
    let remap_pairs = collect_remap_pairs(all_deps, edges);
    let remap_parallel = remap_parallel_enabled();
    let work_items: Vec<RemapWorkItem> = remap_pairs
        .iter()
        .map(|(current_hop, suffix_hop)| RemapWorkItem {
            current_hop: current_hop.clone(),
            suffix_hop: suffix_hop.clone(),
        })
        .collect();

    let remap_start = std::time::Instant::now();
    let built: Vec<Result<(RemapKey, Vec<u32>, RemapProfile), anyhow::Error>> = if remap_parallel {
        scan_pool.install(|| {
            work_items
                .par_iter()
                .map(|item| {
                    let (remap, profile) =
                        build_one_remap(&item.current_hop, &item.suffix_hop, vec_caches)?;
                    Ok((
                        (item.current_hop.clone(), item.suffix_hop.clone()),
                        remap,
                        profile,
                    ))
                })
                .collect()
        })
    } else {
        work_items
            .iter()
            .map(|item| {
                let (remap, profile) =
                    build_one_remap(&item.current_hop, &item.suffix_hop, vec_caches)?;
                Ok((
                    (item.current_hop.clone(), item.suffix_hop.clone()),
                    remap,
                    profile,
                ))
            })
            .collect()
    };

    let mut remap_tables: HashMap<RemapKey, Vec<u32>> = HashMap::with_capacity(remap_pairs.len());
    let mut remap_profiles: Vec<RemapProfile> = Vec::with_capacity(remap_pairs.len());
    for row in built {
        let (key, remap, profile) = row?;
        remap_profiles.push(profile);
        remap_tables.insert(key, remap);
    }

    print_remap_profiles(remap_pairs.len(), &remap_profiles);
    let remap_entries: usize = remap_tables.values().map(Vec::len).sum();
    let remap_fingerprint = remap_tables_fingerprint(&remap_tables);
    eprintln!(
        "GCard_scan_remap_summary: logical_pairs={} unique_remaps={} remap_entries={} remap_fingerprint={:016x} remap_time={:.6}s parallel_enabled={} inner_parallel_enabled={}",
        remap_pairs.len(),
        remap_tables.len(),
        remap_entries,
        remap_fingerprint,
        remap_start.elapsed().as_secs_f64(),
        remap_parallel,
        remap_inner_parallel_enabled(),
    );

    Ok(remap_tables)
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

    all_deps.sort_by(cmp_pattern_deps);

    let mut unique_hops: HashSet<HopKey> = HashSet::new();
    for deps in &all_deps {
        unique_hops.insert(deps.left_hop.clone());
        unique_hops.insert(deps.right_hop.clone());
    }
    let mut unique_hops: Vec<_> = unique_hops.into_iter().collect();
    unique_hops.sort_by(cmp_hop_key);

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

    let remap_tables = build_remap_tables(&all_deps, &vec_caches, edges, scan_pool)?;

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
        let mut level_deps: Vec<&PatternDeps> = all_deps
            .iter()
            .filter(|d| d.len() == len && !computed_patterns.contains(&d.pattern))
            .collect();
        level_deps.sort_by(|a, b| cmp_pattern_deps(a, b));

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
        let mut endpoint_rows: Vec<_> = endpoints.into_iter().collect();
        endpoint_rows.sort_by(|(a, _), (b, _)| cmp_hop_key(a, b));

        let mut out_endpoints: HashMap<String, DegreeSeq> =
            HashMap::with_capacity(endpoint_rows.len());
        for (hop_key, degrees) in endpoint_rows {
            let vec_data = vec_caches.get(&hop_key).unwrap();
            let vids = Arc::clone(&vec_data.src_verts);
            // Use the hop's vertex_label as the endpoint name for downstream consumers.
            // Some cyclic paths have the same label at both endpoints; the public
            // statistic key cannot represent both, so keep a deterministic endpoint.
            out_endpoints
                .entry(hop_key.vertex_label.clone())
                .or_insert((vids, degrees));
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

#[derive(Clone)]
struct HopScanProfile {
    hop: HopKey,
    src_count: usize,
    edge_count: usize,
    unique_dst_count: usize,
    build_time_s: f64,
}

fn print_hop_scan_profiles(profiles: &[HopScanProfile]) {
    let mut rows = profiles.to_vec();
    rows.sort_by(|a, b| {
        b.build_time_s
            .partial_cmp(&a.build_time_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_build_time_s: f64 = rows.iter().map(|p| p.build_time_s).sum();
    let total_edges: usize = rows.iter().map(|p| p.edge_count).sum();
    let max_build_time_s = rows.first().map(|p| p.build_time_s).unwrap_or(0.0);
    eprintln!(
        "GCard_scan_hop_summary: hops={} total_edges={} total_hop_build_time={:.6}s max_hop_build_time={:.6}s",
        rows.len(),
        total_edges,
        total_build_time_s,
        max_build_time_s,
    );

    for profile in rows {
        eprintln!(
            "GCard_scan_hop: vertex_label={} edge_label={} outgoing={} src_count={} edge_count={} unique_dst_count={} build_time={:.6}s",
            profile.hop.vertex_label,
            profile.hop.edge_label,
            profile.hop.outgoing,
            profile.src_count,
            profile.edge_count,
            profile.unique_dst_count,
            profile.build_time_s,
        );
    }
}

#[derive(Clone)]
struct RemapProfile {
    current_hop: HopKey,
    suffix_hop: HopKey,
    entry_count: usize,
    matched_count: usize,
    build_time_s: f64,
}

fn print_remap_profiles(logical_pair_count: usize, profiles: &[RemapProfile]) {
    let mut rows = profiles.to_vec();
    rows.sort_by(|a, b| {
        b.build_time_s
            .partial_cmp(&a.build_time_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_entries: usize = rows.iter().map(|p| p.entry_count).sum();
    let total_matched: usize = rows.iter().map(|p| p.matched_count).sum();
    let total_build_time_s: f64 = rows.iter().map(|p| p.build_time_s).sum();
    let max_build_time_s = rows.first().map(|p| p.build_time_s).unwrap_or(0.0);
    eprintln!(
        "GCard_scan_remap_key_summary: logical_pairs={} unique_remaps={} total_entries={} total_matched={} total_key_build_time={:.6}s max_key_build_time={:.6}s",
        logical_pair_count,
        rows.len(),
        total_entries,
        total_matched,
        total_build_time_s,
        max_build_time_s,
    );

    for profile in rows {
        eprintln!(
            "GCard_scan_remap_key: current=({},{},{}) suffix=({},{},{}) entries={} matched={} build_time={:.6}s",
            profile.current_hop.vertex_label,
            profile.current_hop.edge_label,
            profile.current_hop.outgoing,
            profile.suffix_hop.vertex_label,
            profile.suffix_hop.edge_label,
            profile.suffix_hop.outgoing,
            profile.entry_count,
            profile.matched_count,
            profile.build_time_s,
        );
    }
}

fn hop_inner_parallel_enabled() -> bool {
    std::env::var("GCARD_SCAN_HOP_INNER_PARALLEL")
        .ok()
        .and_then(|s| match s.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Some(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

fn hop_inner_parallel_threshold() -> usize {
    std::env::var("GCARD_SCAN_HOP_INNER_PARALLEL_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1_000_000)
}

fn compact_neighbor_vids_sequential(neighbor_vids: Vec<VertexId>) -> (Vec<VertexId>, Vec<u32>) {
    let mut dst_verts: Vec<VertexId> = Vec::new();
    let mut dst_to_local: HashMap<VertexId, u32> = HashMap::new();
    let mut flat_neighbors: Vec<u32> = Vec::with_capacity(neighbor_vids.len());

    for vid in neighbor_vids {
        let next_local_id = dst_verts.len() as u32;
        let local_id = *dst_to_local.entry(vid).or_insert_with(|| {
            dst_verts.push(vid);
            next_local_id
        });
        flat_neighbors.push(local_id);
    }

    (dst_verts, flat_neighbors)
}

fn compact_neighbor_vids_parallel(neighbor_vids: Vec<VertexId>) -> (Vec<VertexId>, Vec<u32>) {
    let mut dst_verts = neighbor_vids.clone();
    dst_verts.par_sort_unstable();
    dst_verts.dedup();

    let dst_to_local: HashMap<VertexId, u32> = dst_verts
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect();

    let flat_neighbors: Vec<u32> = neighbor_vids
        .par_iter()
        .map(|vid| dst_to_local[vid])
        .collect();

    (dst_verts, flat_neighbors)
}

fn compact_neighbor_vids(
    neighbor_vids: Vec<VertexId>,
    allow_parallel: bool,
) -> (Vec<VertexId>, Vec<u32>) {
    if allow_parallel
        && hop_inner_parallel_enabled()
        && neighbor_vids.len() >= hop_inner_parallel_threshold()
        && rayon::current_num_threads() > 1
    {
        compact_neighbor_vids_parallel(neighbor_vids)
    } else {
        compact_neighbor_vids_sequential(neighbor_vids)
    }
}

fn build_src_vid_to_idx(verts: &[VertexId]) -> HashMap<VertexId, u32> {
    verts
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i as u32))
        .collect()
}

fn build_compact_vec_neighbor_data_from_raw(
    raw: RawHopData,
    allow_parallel: bool,
) -> VecNeighborData {
    let (dst_verts, flat_neighbors) = compact_neighbor_vids(raw.neighbors_flat, allow_parallel);
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

fn collect_scan_deps(
    edges: &HashMap<String, EdgeEndpoints>,
    schema_path: &PathsByLen,
    max_k: usize,
) -> (Vec<PatternDeps>, Vec<HopKey>) {
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

    all_deps.sort_by(cmp_pattern_deps);

    let mut unique_hops: HashSet<HopKey> = HashSet::new();
    for deps in &all_deps {
        unique_hops.insert(deps.left_hop.clone());
        unique_hops.insert(deps.right_hop.clone());
    }
    let mut unique_hops: Vec<_> = unique_hops.into_iter().collect();
    unique_hops.sort_by(cmp_hop_key);

    (all_deps, unique_hops)
}

fn finish_scanned_hops(
    all_deps: Vec<PatternDeps>,
    vec_caches: HashMap<HopKey, VecNeighborData>,
    edges: &HashMap<String, EdgeEndpoints>,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
    let remap_tables = build_remap_tables(&all_deps, &vec_caches, edges, scan_pool)?;

    Ok(ScannedHops {
        vec_caches,
        all_deps,
        remap_tables,
    })
}

fn scan_hop_parallelism(_scan_pool: &rayon::ThreadPool, hop_count: usize) -> usize {
    let default_parallelism = hop_count.max(1);
    let requested = std::env::var("GCARD_SCAN_HOP_PARALLELISM")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default_parallelism);
    requested.min(hop_count.max(1))
}

fn scan_flat_direct() -> bool {
    std::env::var("GCARD_SCAN_FLAT_DIRECT")
        .ok()
        .and_then(|s| match s.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Some(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

/// Build `ScannedHops` from externally-provided raw adjacency data.
///
/// `get_hop(vertex_label, edge_label, outgoing)` must return a `RawHopData`
/// for the requested hop. This allows building the scan cache from CSV files
/// without loading data into `MemoryGraph`.
pub fn scan_all_hops_from_raw(
    get_hop: &(dyn Fn(&str, &str, bool) -> RawHopData + Sync),
    edges: &HashMap<String, EdgeEndpoints>,
    schema_path: &PathsByLen,
    max_k: usize,
    scan_pool: &rayon::ThreadPool,
) -> Result<ScannedHops, anyhow::Error> {
    let (all_deps, unique_hops) = collect_scan_deps(edges, schema_path, max_k);

    let hop_parallelism = scan_hop_parallelism(scan_pool, unique_hops.len());
    eprintln!(
        "GCard_scan: unique_hops={}, hop_parallelism={}",
        unique_hops.len(),
        hop_parallelism,
    );

    let mut vec_caches: HashMap<HopKey, VecNeighborData> =
        HashMap::with_capacity(unique_hops.len());
    let mut profiles: Vec<HopScanProfile> = Vec::with_capacity(unique_hops.len());
    if hop_parallelism == 1 {
        for hop in &unique_hops {
            let start = std::time::Instant::now();
            let raw = get_hop(&hop.vertex_label, &hop.edge_label, hop.outgoing);
            let src_count = raw.src_vids.len();
            let edge_count = raw.neighbors_flat.len();
            let vec_data = build_compact_vec_neighbor_data_from_raw(raw, false);
            let profile = HopScanProfile {
                hop: hop.clone(),
                src_count,
                edge_count,
                unique_dst_count: vec_data.dst_verts.len(),
                build_time_s: start.elapsed().as_secs_f64(),
            };
            profiles.push(profile);
            vec_caches.insert(hop.clone(), vec_data);
        }
    } else {
        let rows: Vec<(HopKey, VecNeighborData, HopScanProfile)> = scan_pool.install(|| {
            unique_hops
                .par_iter()
                .map(|hop| {
                    let start = std::time::Instant::now();
                    let raw = get_hop(&hop.vertex_label, &hop.edge_label, hop.outgoing);
                    let src_count = raw.src_vids.len();
                    let edge_count = raw.neighbors_flat.len();
                    let vec_data = build_compact_vec_neighbor_data_from_raw(raw, true);
                    let profile = HopScanProfile {
                        hop: hop.clone(),
                        src_count,
                        edge_count,
                        unique_dst_count: vec_data.dst_verts.len(),
                        build_time_s: start.elapsed().as_secs_f64(),
                    };
                    (hop.clone(), vec_data, profile)
                })
                .collect()
        });
        for (hop, vec_data, profile) in rows {
            profiles.push(profile);
            vec_caches.insert(hop, vec_data);
        }
    }
    print_hop_scan_profiles(&profiles);

    finish_scanned_hops(all_deps, vec_caches, edges, scan_pool)
}

// ----- FlatGraph-based hop building -----

fn raw_hop_data_from_flat_csr(
    verts: &[VertexId],
    csr: Option<&crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid>,
) -> RawHopData {
    match csr {
        Some(csr) => {
            let csr_verts = csr.verts();
            let csr_offsets = csr.offsets();
            let neighbor_entries = csr.neighbor_entries();
            let neighbor_count = neighbor_entries.len();
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
                        neighbor_count
                    };
                    offsets.push(cur);
                }
            }
            offsets.push(neighbor_count);
            RawHopData {
                src_vids: verts.to_vec(),
                neighbors_flat: neighbor_entries.iter().map(|&(vid, _)| vid).collect(),
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

fn build_vec_neighbor_data_from_flat_csr(
    verts: &[VertexId],
    csr: Option<&crate::procedures::gcard_query::flat_graph::csr::CsrAdjWithEid>,
    allow_parallel: bool,
) -> VecNeighborData {
    match csr {
        Some(csr) => {
            let csr_verts = csr.verts();
            let csr_offsets = csr.offsets();
            let neighbor_entries = csr.neighbor_entries();
            let neighbor_count = neighbor_entries.len();

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
                        neighbor_count
                    };
                    offsets.push(cur);
                }
            }
            offsets.push(neighbor_count);

            let (dst_verts, flat_neighbors) = if allow_parallel
                && hop_inner_parallel_enabled()
                && neighbor_count >= hop_inner_parallel_threshold()
                && rayon::current_num_threads() > 1
            {
                let neighbor_vids: Vec<VertexId> =
                    neighbor_entries.par_iter().map(|&(vid, _)| vid).collect();
                compact_neighbor_vids_parallel(neighbor_vids)
            } else {
                let mut dst_verts: Vec<VertexId> = Vec::new();
                let mut dst_to_local: HashMap<VertexId, u32> = HashMap::new();
                let mut flat_neighbors: Vec<u32> = Vec::with_capacity(neighbor_count);
                for &(vid, _) in neighbor_entries {
                    let next_local_id = dst_verts.len() as u32;
                    let local_id = *dst_to_local.entry(vid).or_insert_with(|| {
                        dst_verts.push(vid);
                        next_local_id
                    });
                    flat_neighbors.push(local_id);
                }
                (dst_verts, flat_neighbors)
            };

            let src_vid_to_idx = build_src_vid_to_idx(verts);

            VecNeighborData {
                src_verts: Arc::new(verts.to_vec()),
                src_vid_to_idx,
                dst_verts,
                flat_neighbors,
                offsets,
            }
        }
        None => VecNeighborData {
            src_vid_to_idx: build_src_vid_to_idx(verts),
            src_verts: Arc::new(verts.to_vec()),
            dst_verts: Vec::new(),
            flat_neighbors: Vec::new(),
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
    if !scan_flat_direct() {
        return scan_all_hops_from_raw(
            &|vertex_label, edge_label, outgoing| {
                let verts = flat_graph.all_vertex_ids_by_label(vertex_label);
                let key = (vertex_label.to_string(), edge_label.to_string(), outgoing);
                raw_hop_data_from_flat_csr(verts, flat_graph.hop_csrs().get(&key))
            },
            edges,
            schema_path,
            max_k,
            scan_pool,
        );
    }

    let (all_deps, unique_hops) = collect_scan_deps(edges, schema_path, max_k);
    let hop_parallelism = scan_hop_parallelism(scan_pool, unique_hops.len());
    eprintln!(
        "GCard_scan: unique_hops={}, hop_parallelism={}, flat_direct=true",
        unique_hops.len(),
        hop_parallelism,
    );

    let mut vec_caches: HashMap<HopKey, VecNeighborData> =
        HashMap::with_capacity(unique_hops.len());
    let mut profiles: Vec<HopScanProfile> = Vec::with_capacity(unique_hops.len());
    if hop_parallelism == 1 {
        for hop in &unique_hops {
            let start = std::time::Instant::now();
            let verts = flat_graph.all_vertex_ids_by_label(&hop.vertex_label);
            let key = (
                hop.vertex_label.clone(),
                hop.edge_label.clone(),
                hop.outgoing,
            );
            let vec_data = build_vec_neighbor_data_from_flat_csr(
                verts,
                flat_graph.hop_csrs().get(&key),
                false,
            );
            let profile = HopScanProfile {
                hop: hop.clone(),
                src_count: vec_data.src_verts.len(),
                edge_count: vec_data.flat_neighbors.len(),
                unique_dst_count: vec_data.dst_verts.len(),
                build_time_s: start.elapsed().as_secs_f64(),
            };
            profiles.push(profile);
            vec_caches.insert(hop.clone(), vec_data);
        }
    } else {
        let rows: Vec<(HopKey, VecNeighborData, HopScanProfile)> = scan_pool.install(|| {
            unique_hops
                .par_iter()
                .map(|hop| {
                    let start = std::time::Instant::now();
                    let verts = flat_graph.all_vertex_ids_by_label(&hop.vertex_label);
                    let key = (
                        hop.vertex_label.clone(),
                        hop.edge_label.clone(),
                        hop.outgoing,
                    );
                    let vec_data = build_vec_neighbor_data_from_flat_csr(
                        verts,
                        flat_graph.hop_csrs().get(&key),
                        true,
                    );
                    let profile = HopScanProfile {
                        hop: hop.clone(),
                        src_count: vec_data.src_verts.len(),
                        edge_count: vec_data.flat_neighbors.len(),
                        unique_dst_count: vec_data.dst_verts.len(),
                        build_time_s: start.elapsed().as_secs_f64(),
                    };
                    (hop.clone(), vec_data, profile)
                })
                .collect()
        });
        for (hop, vec_data, profile) in rows {
            profiles.push(profile);
            vec_caches.insert(hop, vec_data);
        }
    }
    print_hop_scan_profiles(&profiles);

    finish_scanned_hops(all_deps, vec_caches, edges, scan_pool)
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

    #[test]
    fn parallel_neighbor_compaction_preserves_edge_targets() {
        let neighbors = vec![42, 7, 42, 9, 7, 11, 9, 42];

        let (seq_dst, seq_flat) = compact_neighbor_vids_sequential(neighbors.clone());
        let (par_dst, par_flat) = compact_neighbor_vids_parallel(neighbors);

        let seq_targets: Vec<VertexId> = seq_flat
            .iter()
            .map(|&local| seq_dst[local as usize])
            .collect();
        let par_targets: Vec<VertexId> = par_flat
            .iter()
            .map(|&local| par_dst[local as usize])
            .collect();

        assert_eq!(seq_targets, par_targets);
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
