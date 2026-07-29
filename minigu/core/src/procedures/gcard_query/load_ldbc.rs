//! `load_ldbc` procedure: build GCard catalog directly from LDBC CSV files.
//!
//! Reads only topology (vertex IDs + edge src/dst) from CSV, builds a lightweight
//! CSR adjacency in memory, computes degree sequences, and persists the resulting
//! `Statistic` as bincode.  This bypasses `MemoryGraph` entirely, reducing memory
//! from ~80 GB (sf10 via import_graph) to ~4 GB.
//!
//! Usage:
//!   `call load_ldbc('<dataset_dir>', '<graph_name>', <max_k>)`
//!
//! After this, use `load_catalog('<graph_name>')` or directly run `gcard_query`.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use csv::ReaderBuilder;
use minigu_catalog::provider::{GraphTypeProvider, SchemaProvider};
use minigu_common::data_type::LogicalType;
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use minigu_context::graph::{GraphContainer, GraphStorage};
use minigu_context::procedure::Procedure;
use minigu_storage::tp::MemoryGraph;
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;

use super::create_catalog::{add_functional_path_aliases, enumerate_all_paths_walks_in_schema};
use super::degree_compute_dense::{self, RawHopData};
use super::flat_graph::{FlatGraph, FlatGraphBuilder};
use crate::procedures::common::Manifest;
use crate::procedures::gcard_query::statistic::save_statistic;
use crate::procedures::gcard_query::utils::{
    EdgeEndpoints, edge_cardinalities_from_schema, get_edges_from_catalog,
};
use crate::procedures::import_graph::{get_graph_type_from_manifest, property_to_scalar_value};

// ---------------------------------------------------------------------------
// Lightweight CSR adjacency (topology only, no properties / MVCC)
// ---------------------------------------------------------------------------

/// Per-edge-type CSR: sorted source vertices with flat neighbor offsets.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct CsrAdj {
    /// 本 CSR bucket 覆盖的“起点顶点”集合。
    pub verts: Vec<VertexId>,
    /// 扁平邻居数组。
    pub neighbors: Vec<VertexId>,
    /// CSR 偏移数组。
    pub offsets: Vec<usize>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct LightGraph {
    /// 顶点标签 -> 该标签下所有顶点 id。
    pub vertices_by_label: HashMap<String, Vec<VertexId>>,
    /// `(起点标签, 边标签, 是否出边)` -> 对应 CSR bucket。
    pub hop_csrs: HashMap<(String, String, bool), CsrAdj>,
}

// ---------------------------------------------------------------------------
// CSV reading helpers
// ---------------------------------------------------------------------------

fn read_vertex_ids(path: &Path) -> Vec<VertexId> {
    // 这里仅读第一列 id，不加载属性，目的是尽量低内存地构造 topology。
    let mut rdr = match ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b',')
        .from_path(path)
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: cannot open vertex CSV {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let mut ids = Vec::new();
    for record in rdr.records() {
        let record = match record {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Some(id_str) = record.get(0) {
            if let Ok(id) = id_str.parse::<VertexId>() {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Read (src, dst) pairs from an edge CSV.  Only the first two columns are used.
fn read_edge_pairs(path: &Path) -> Vec<(VertexId, VertexId)> {
    // 同样只取结构信息，不取边属性。
    let mut rdr = match ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b',')
        .from_path(path)
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: cannot open edge CSV {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    let mut pairs = Vec::new();
    for record in rdr.records() {
        let record = match record {
            Ok(r) => r,
            Err(_) => continue,
        };
        let src: VertexId = match record.get(0).and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let dst: VertexId = match record.get(1).and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        pairs.push((src, dst));
    }
    pairs
}

// ---------------------------------------------------------------------------
// Build CSR from edge pairs
// ---------------------------------------------------------------------------

/// Build a CSR where `all_verts` are the "source" side and pairs provide
/// (source, neighbor) for the given direction.
fn build_csr(all_verts: &[VertexId], mut pairs: Vec<(VertexId, VertexId)>) -> CsrAdj {
    // 标准 CSR 构建：先按 src 排序，再把邻居串接进一条平面数组。
    pairs.sort_unstable_by_key(|&(src, _)| src);

    let n = all_verts.len();
    let mut offsets = Vec::with_capacity(n + 1);
    let mut neighbors = Vec::with_capacity(pairs.len());

    let mut pair_idx = 0;
    for &vid in all_verts {
        offsets.push(neighbors.len());
        while pair_idx < pairs.len() && pairs[pair_idx].0 < vid {
            // 理论上不常见，但允许 pairs 中出现不在 all_verts 的 src。
            pair_idx += 1;
        }
        while pair_idx < pairs.len() && pairs[pair_idx].0 == vid {
            neighbors.push(pairs[pair_idx].1);
            pair_idx += 1;
        }
    }
    offsets.push(neighbors.len());

    CsrAdj {
        verts: all_verts.to_vec(),
        neighbors,
        offsets,
    }
}

// ---------------------------------------------------------------------------
// Build LightGraph from manifest + CSV directory
// ---------------------------------------------------------------------------

fn build_light_graph(manifest: &Manifest, dataset_dir: &Path) -> LightGraph {
    // LightGraph 是一个“只含结构、没有属性/MVCC”的过渡表示。
    // 用它来做 catalog 构建，比把整图先导入 MemoryGraph 更省内存。

    // 1. 并行读取每种顶点类型的顶点 id。
    let vertex_results: Vec<(String, Vec<VertexId>)> = manifest
        .vertices
        .par_iter()
        .map(|vs| {
            let path = dataset_dir.join(&vs.file.path);
            let label = vs.label.to_lowercase();
            let ids = read_vertex_ids(&path);
            eprintln!("  vertex label '{}': {} vertices", label, ids.len());
            (label, ids)
        })
        .collect();
    let vertices_by_label: HashMap<String, Vec<VertexId>> = vertex_results.into_iter().collect();

    // 2. 并行读取边，再分别构建 outgoing / incoming 两种方向的 CSR。
    let edge_results: Vec<(String, String, String, Vec<(VertexId, VertexId)>)> = manifest
        .edges
        .par_iter()
        .map(|es| {
            let path = dataset_dir.join(&es.file.path);
            let label = es.label.to_lowercase();
            let src_label = es.src_label.to_lowercase();
            let dst_label = es.dst_label.to_lowercase();
            let pairs = read_edge_pairs(&path);
            eprintln!("  edge label '{}': {} edges", label, pairs.len());
            (label, src_label, dst_label, pairs)
        })
        .collect();

    let mut hop_csrs: HashMap<(String, String, bool), CsrAdj> = HashMap::new();
    for (edge_label, src_label, dst_label, pairs) in edge_results {
        // Outgoing: from src_label vertices, neighbor = dst
        let src_verts = vertices_by_label
            .get(&src_label)
            .cloned()
            .unwrap_or_default();
        let out_pairs: Vec<(VertexId, VertexId)> = pairs.iter().copied().collect();
        hop_csrs.insert(
            (src_label.clone(), edge_label.clone(), true),
            build_csr(&src_verts, out_pairs),
        );

        // Incoming: from dst_label vertices, neighbor = src
        let dst_verts = vertices_by_label
            .get(&dst_label)
            .cloned()
            .unwrap_or_default();
        let in_pairs: Vec<(VertexId, VertexId)> = pairs.into_iter().map(|(s, d)| (d, s)).collect();
        hop_csrs.insert(
            (dst_label.clone(), edge_label.clone(), false),
            build_csr(&dst_verts, in_pairs),
        );
    }

    LightGraph {
        vertices_by_label,
        hop_csrs,
    }
}

// ---------------------------------------------------------------------------
// LightGraph bincode persistence
// ---------------------------------------------------------------------------

pub(super) fn save_light_graph(
    path: &Path,
    graph: &LightGraph,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = bincode::serialize(graph)?;
    std::fs::write(path, &data)?;
    eprintln!(
        "load_ldbc: saved LightGraph bincode ({:.2} MB) to {}",
        data.len() as f64 / 1024.0 / 1024.0,
        path.display()
    );
    Ok(())
}

pub(super) fn load_light_graph(
    path: &Path,
) -> Result<LightGraph, Box<dyn std::error::Error + Send + Sync>> {
    let data = std::fs::read(path)?;
    let graph: LightGraph = bincode::deserialize(&data)?;
    eprintln!(
        "Loaded LightGraph bincode ({:.2} MB) from {}",
        data.len() as f64 / 1024.0 / 1024.0,
        path.display()
    );
    Ok(graph)
}

/// Convert a LightGraph into a closure-compatible RawHopData lookup.
pub(super) fn scan_from_light_graph(
    light_graph: &LightGraph,
    edges: &HashMap<String, EdgeEndpoints>,
    schema_paths: &degree_compute_dense::PathsByLen,
    max_k: usize,
    pool: &rayon::ThreadPool,
) -> Result<degree_compute_dense::ScannedHops, anyhow::Error> {
    fn dense_raw_hop(all_verts: &[VertexId], csr: Option<&CsrAdj>) -> RawHopData {
        match csr {
            Some(csr) => {
                let mut offsets = Vec::with_capacity(all_verts.len() + 1);
                let mut csr_pos = 0usize;
                for &vid in all_verts {
                    while csr_pos < csr.verts.len() && csr.verts[csr_pos] < vid {
                        csr_pos += 1;
                    }
                    if csr_pos < csr.verts.len() && csr.verts[csr_pos] == vid {
                        offsets.push(csr.offsets[csr_pos]);
                    } else {
                        let cur = offsets.last().copied().unwrap_or(0);
                        offsets.push(cur);
                    }
                }
                offsets.push(csr.neighbors.len());
                RawHopData {
                    src_vids: all_verts.to_vec(),
                    neighbors_flat: csr.neighbors.clone(),
                    offsets,
                }
            }
            None => RawHopData {
                offsets: vec![0; all_verts.len() + 1],
                src_vids: all_verts.to_vec(),
                neighbors_flat: Vec::new(),
            },
        }
    }

    degree_compute_dense::scan_all_hops_from_raw(
        &|vertex_label, edge_label, outgoing| {
            let key = (vertex_label.to_string(), edge_label.to_string(), outgoing);
            let verts = light_graph
                .vertices_by_label
                .get(vertex_label)
                .cloned()
                .unwrap_or_default();
            dense_raw_hop(&verts, light_graph.hop_csrs.get(&key))
        },
        edges,
        schema_paths,
        max_k,
        pool,
    )
}

/// Build a lightweight `LightGraph` from a `FlatGraph` (topology only, ~730 MB
/// for sf1 vs 30 GB FlatGraph).  Only vertex IDs and neighbor IDs are kept;
/// edge IDs and properties are dropped.
fn light_graph_from_flat_graph(fg: &super::flat_graph::FlatGraph) -> LightGraph {
    let vertices_by_label: HashMap<String, Vec<VertexId>> = fg
        .vertices_by_label_map()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut hop_csrs: HashMap<(String, String, bool), CsrAdj> = HashMap::new();
    for ((vlabel, elabel, outgoing), csr) in fg.hop_csrs() {
        hop_csrs.insert(
            (vlabel.clone(), elabel.clone(), *outgoing),
            CsrAdj {
                verts: csr.verts().to_vec(),
                neighbors: csr.neighbors_vids(), // only VertexId, drops EdgeId
                offsets: csr.offsets().to_vec(),
            },
        );
    }

    LightGraph {
        vertices_by_label,
        hop_csrs,
    }
}

/// Rebuild `Statistic` from a `FlatGraph` by converting to a lightweight
/// `LightGraph` (~730 MB) and re-scanning its CSR adjacency.
///
/// Peak memory: FlatGraph (30 GB) + LightGraph (730 MB) + ScannedHops (1.1 GB).
/// The LightGraph is dropped before building the Statistic.
pub(crate) fn rebuild_statistic_from_flat_graph(
    fg: &super::flat_graph::FlatGraph,
    graph_type: &dyn minigu_catalog::provider::GraphTypeProvider,
    max_k: usize,
) -> anyhow::Result<super::Statistic> {
    let edges = get_edges_from_catalog(graph_type)?;
    let schema_paths = enumerate_all_paths_walks_in_schema(&edges, max_k);

    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let pool = ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .map_err(|e| anyhow::anyhow!("thread pool: {}", e))?;

    // Build lightweight CSR (only VertexId neighbors, no EdgeId/properties).
    eprintln!("[rebuild_statistic] building LightGraph from FlatGraph...");
    let t0 = std::time::Instant::now();
    let light_graph = light_graph_from_flat_graph(fg);
    eprintln!(
        "[rebuild_statistic] LightGraph built in {:.2}s",
        t0.elapsed().as_secs_f64()
    );

    let t1 = std::time::Instant::now();
    let scanned = scan_from_light_graph(&light_graph, &edges, &schema_paths, max_k, &pool)?;
    eprintln!(
        "[rebuild_statistic] hop scan done in {:.2}s",
        t1.elapsed().as_secs_f64()
    );

    // Drop LightGraph before computing to reduce peak memory.
    drop(light_graph);

    let scanned = Arc::new(scanned);
    let (max_star_length, max_star_degree) =
        super::create_catalog::parse_star_collection_config(max_k);
    eprintln!(
        "[rebuild_statistic] star collection: max_star_length={}, max_star_degree={}{}",
        max_star_length,
        max_star_degree,
        if max_star_degree == 0 {
            " (disabled)"
        } else {
            ""
        },
    );

    let t2 = std::time::Instant::now();
    let (cache, star_cache) = degree_compute_dense::compute_from_scanned_hops_with_star(
        &scanned,
        &edges,
        max_k,
        max_star_length,
        max_star_degree,
        &pool,
    )?;
    eprintln!(
        "[rebuild_statistic] degree compute done in {:.2}s",
        t2.elapsed().as_secs_f64()
    );

    let statistic =
        super::create_catalog::build_statistic_from_pattern_and_star_cache(&cache, &star_cache)?;
    Ok(statistic)
}

// ---------------------------------------------------------------------------
// FlatGraph construction from LDBC CSVs (with properties)
// ---------------------------------------------------------------------------

/// Build a [`FlatGraph`] by reading vertex and edge CSV files with full property data.
///
/// Vertex CSV layout: `<vid>,<prop1>,<prop2>,...`
/// Edge CSV layout:   `<src_vid>,<dst_vid>,<prop1>,<prop2>,...`
///
/// Returns `None` when property resolution fails for any spec, falling back gracefully.
pub(super) fn build_flat_graph(manifest: &Manifest, dataset_dir: &std::path::Path) -> FlatGraph {
    let mut builder = FlatGraphBuilder::new();

    // ── Vertex schemas and data ──────────────────────────────────────────────
    for vs in &manifest.vertices {
        let label = vs.label.to_lowercase();
        let prop_names: Vec<String> = vs.properties.iter().map(|p| p.name().to_string()).collect();
        builder.set_vertex_prop_schema(&label, prop_names);

        let path = dataset_dir.join(&vs.file.path);
        let mut rdr = match ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .delimiter(b',')
            .from_path(&path)
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "load_ldbc flat_graph: cannot open vertex CSV {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        for record in rdr.records() {
            let record = match record {
                Ok(r) => r,
                Err(_) => continue,
            };
            let vid: VertexId = match record.get(0).and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            // Build a schema-width positional row. `zip` would silently
            // truncate short CSV records, causing later property indexes and
            // column null counts to diverge from the label schema.
            let props: Vec<ScalarValue> = vs
                .properties
                .iter()
                .enumerate()
                .map(|(property_index, prop_def)| {
                    property_to_scalar_value(prop_def, record.get(property_index + 1).unwrap_or(""))
                        .ok()
                        .unwrap_or(ScalarValue::Null)
                })
                .collect();
            builder.add_vertex(vid, &label, props);
        }
    }

    // ── Edge schemas and data ────────────────────────────────────────────────
    let mut edge_id_counter: EdgeId = 1;
    for es in &manifest.edges {
        let edge_label = es.label.to_lowercase();
        let src_label = es.src_label.to_lowercase();
        let dst_label = es.dst_label.to_lowercase();

        let prop_names: Vec<String> = es.properties.iter().map(|p| p.name().to_string()).collect();
        builder.set_edge_prop_schema(&edge_label, prop_names);
        builder.set_edge_type_schema(&edge_label, &src_label, &dst_label);

        let path = dataset_dir.join(&es.file.path);
        let mut rdr = match ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .delimiter(b',')
            .from_path(&path)
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "load_ldbc flat_graph: cannot open edge CSV {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        for record in rdr.records() {
            let record = match record {
                Ok(r) => r,
                Err(_) => continue,
            };
            if record.len() < 2 {
                continue;
            }
            let src: VertexId = match record.get(0).and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let dst: VertexId = match record.get(1).and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let eid = edge_id_counter;
            edge_id_counter += 1;
            let props: Vec<ScalarValue> = es
                .properties
                .iter()
                .enumerate()
                .map(|(property_index, prop_def)| {
                    property_to_scalar_value(prop_def, record.get(property_index + 2).unwrap_or(""))
                        .ok()
                        .unwrap_or(ScalarValue::Null)
                })
                .collect();
            builder.add_edge(eid, src, &src_label, dst, &dst_label, &edge_label, props);
        }
    }

    builder.build()
}

// ---------------------------------------------------------------------------
// Procedure entry point
// ---------------------------------------------------------------------------

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String, // dataset_dir (path to LDBC CSV directory with manifest.json)
        LogicalType::String, // graph_name
        LogicalType::Int8,   // max_k
    ];
    Procedure::new(parameters, None, move |context, args| {
        let dataset_dir = args[0]
            .try_as_string()
            .expect("dataset_dir must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dataset_dir cannot be null"))?
            .to_string();
        let graph_name = args[1]
            .try_as_string()
            .expect("graph_name must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("graph_name cannot be null"))?
            .to_string();
        let max_k = args[2]
            .try_as_int8()
            .expect("max_k must be int8")
            .ok_or_else(|| anyhow::anyhow!("max_k cannot be null"))? as usize;

        let dataset_path = Path::new(&dataset_dir);
        let manifest_path = dataset_path.join("manifest.json");

        // SF tag derived from the last path component (e.g. "sf1" from ".../ldbc/sf1").
        let sf_tag = dataset_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_owned();
        // Parent of the SF directory (e.g. ".../datasets/ldbc/").  All cache files
        // and the statistic are placed here with an SF-tagged filename so that
        // different scale factors share the same directory and never collide.
        let cache_dir = dataset_path.parent().unwrap_or(dataset_path).to_path_buf();

        // ── Parse manifest & build graph type catalog ──
        let manifest_data = std::fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", manifest_path.display(), e))?;
        let manifest = Manifest::from_str(&manifest_data)
            .map_err(|e| anyhow::anyhow!("cannot parse manifest: {}", e))?;
        let graph_type = get_graph_type_from_manifest(&manifest)
            .map_err(|e| anyhow::anyhow!("get_graph_type_from_manifest: {}", e))?;

        // ── Build lightweight adjacency from CSVs ──
        eprintln!("load_ldbc: reading CSV topology...");
        let t0 = std::time::Instant::now();
        let light_graph = build_light_graph(&manifest, dataset_path);
        eprintln!(
            "load_ldbc: CSV reading done in {:.2}s",
            t0.elapsed().as_secs_f64()
        );

        // ── Save LightGraph bincode for create_catalog reuse ──
        let lg_path = cache_dir.join(format!("{}_{}.lightgraph.bin", graph_name, sf_tag));
        if !lg_path.exists() {
            if let Err(e) = save_light_graph(&lg_path, &light_graph) {
                eprintln!("load_ldbc: warning: could not save lightgraph cache: {}", e);
            }
        }

        // ── Compute degree sequences ──
        let edges = get_edges_from_catalog(graph_type.as_ref())?;
        let schema_paths = enumerate_all_paths_walks_in_schema(&edges, max_k);

        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        let pool = ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .map_err(|e| anyhow::anyhow!("thread pool: {}", e))?;

        eprintln!("load_ldbc: scanning hops from CSR...");
        let t1 = std::time::Instant::now();
        let scanned = scan_from_light_graph(&light_graph, &edges, &schema_paths, max_k, &pool)?;
        eprintln!(
            "load_ldbc: hop scan done in {:.2}s ({:.2} MB)",
            t1.elapsed().as_secs_f64(),
            scanned.mem_usage_bytes() as f64 / 1024.0 / 1024.0
        );

        // Drop the light graph to free memory before compute
        drop(light_graph);
        eprintln!("load_ldbc: light graph released");

        // ── Save ScannedHops bincode (for create_catalog to skip scan entirely) ──
        let scanned_path = cache_dir.join(format!("{}_{}.scanned_hops.bin", graph_name, sf_tag));
        if !scanned_path.exists() {
            if let Err(e) = scanned.save_bincode(&scanned_path) {
                eprintln!(
                    "load_ldbc: warning: could not save scanned_hops cache: {}",
                    e
                );
            }
        }

        let scanned = Arc::new(scanned);
        let (max_star_length, max_star_degree) =
            super::create_catalog::parse_star_collection_config(max_k);
        eprintln!(
            "load_ldbc: star collection max_star_length={}, max_star_degree={}{}",
            max_star_length,
            max_star_degree,
            if max_star_degree == 0 {
                " (disabled)"
            } else {
                ""
            },
        );
        let t2 = std::time::Instant::now();
        let (cache, star_cache) = degree_compute_dense::compute_from_scanned_hops_with_star(
            &scanned,
            &edges,
            max_k,
            max_star_length,
            max_star_degree,
            &pool,
        )?;
        eprintln!(
            "load_ldbc: degree compute done in {:.2}s",
            t2.elapsed().as_secs_f64()
        );

        // ── Build Statistic & DegreeSeqGraphCompressed ──
        let statistic = super::create_catalog::build_statistic_from_pattern_and_star_cache(
            &cache,
            &star_cache,
        )?;
        let size = statistic.serialized_size();
        eprintln!(
            "load_ldbc: statistic serialized size: {:.2} MB",
            size as f64 / 1024.0 / 1024.0
        );
        statistic.report_compressed_sizes();

        let mut degree_seq = statistic
            .to_degree_seq_graph_compressed()
            .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {}", e))?;
        degree_seq.edge_cardinalities = edge_cardinalities_from_schema(&edges);
        let alias_count = add_functional_path_aliases(&mut degree_seq, &edges, max_k);
        eprintln!(
            "load_ldbc: functional path aliases generated: {} (not scanned)",
            alias_count
        );

        // ── Persist statistic as bincode ──
        // Written to the shared cache directory (parent of the SF dir) with an
        // SF-tagged name so each scale factor gets its own file there.
        let tagged_graph_name = format!("{}_{}", graph_name, sf_tag);
        if let Err(e) = save_statistic(&cache_dir, &tagged_graph_name, &statistic) {
            eprintln!(
                "load_ldbc: warning: could not save statistic (disk full?): {}",
                e
            );
        }

        // ── Build FlatGraph with full property data ────────────────────────────
        eprintln!("load_ldbc: building FlatGraph with properties...");
        let t_flat = std::time::Instant::now();
        let flat_graph = build_flat_graph(&manifest, dataset_path);
        eprintln!(
            "load_ldbc: FlatGraph built in {:.2}s ({} vertices, {} edges)",
            t_flat.elapsed().as_secs_f64(),
            flat_graph.total_vertex_count(),
            flat_graph.total_edge_count(),
        );

        // ── Persist FlatGraph and Statistic to db directory ───────────────────
        let db_path = context.database().config().db_path.clone();
        if let Some(ref db_path) = db_path {
            let fg_path = crate::catalog_persistence::flatgraph_path(db_path, &graph_name);
            if let Err(e) = flat_graph.export_bincode(&fg_path) {
                eprintln!("load_ldbc: warning: could not save FlatGraph: {}", e);
            } else {
                eprintln!("load_ldbc: FlatGraph saved to {}", fg_path.display());
            }
            if let Err(e) = save_statistic(db_path, &graph_name, &statistic) {
                eprintln!("load_ldbc: warning: could not save statistic to db: {}", e);
            }
        }

        // ── Register a lightweight GraphContainer (no graph data, schema + catalog only) ──
        let schema = context
            .current_schema
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("current schema not set"))?;

        // Use an empty MemoryGraph — no vertex/edge data loaded
        let empty_graph = MemoryGraph::in_memory();
        let container =
            GraphContainer::new(Arc::clone(&graph_type), GraphStorage::Memory(empty_graph));
        container.set_degree_seq_graph_compressed(Arc::new(degree_seq));
        container.set_statistic(Arc::new(statistic));

        let update_log = crate::procedures::gcard_query::update_log::new_log_arc(max_k);
        container.set_gcard_update_log(update_log);
        container.set_gcard_scanned_hops(scanned as Arc<dyn std::any::Any + Send + Sync>);
        container
            .set_gcard_flat_graph(Arc::new(flat_graph) as Arc<dyn std::any::Any + Send + Sync>);

        if !schema.add_graph(graph_name.clone(), Arc::new(container)) {
            return Err(anyhow::anyhow!("graph '{}' already exists in schema", graph_name).into());
        }

        if let Some(ref db_path) = context.database().config().db_path {
            if let Err(e) = crate::catalog_persistence::save_catalog(db_path, schema) {
                eprintln!("load_ldbc: warning: could not save catalog: {}", e);
            }
        }

        eprintln!(
            "load_ldbc: graph '{}' registered with catalog (no graph data loaded)",
            graph_name
        );

        Ok(vec![])
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::str::FromStr;

    use minigu_common::value::ScalarValue;
    use tempfile::tempdir;

    use super::build_flat_graph;
    use crate::procedures::common::Manifest;

    #[test]
    fn flat_graph_loader_preserves_middle_and_trailing_null_positions() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("person.csv"),
            "id,name,age,city\n1,Alice,,Beijing\n2,Bob\n",
        )
        .unwrap();

        let manifest = Manifest::from_str(
            r#"{
                "vertices": [{
                    "label": "person",
                    "file": {"path": "person.csv", "format": "csv"},
                    "properties": [
                        {"name": "name", "logical_type": "String", "nullable": true},
                        {"name": "age", "logical_type": "Int64", "nullable": true},
                        {"name": "city", "logical_type": "String", "nullable": true}
                    ]
                }],
                "edges": []
            }"#,
        )
        .unwrap();

        let graph = build_flat_graph(&manifest, dir.path());
        assert_eq!(
            graph.vertex_props("person", 1),
            Some(
                [
                    ScalarValue::String(Some("Alice".to_string())),
                    ScalarValue::Int64(None),
                    ScalarValue::String(Some("Beijing".to_string())),
                ]
                .as_slice()
            )
        );
        assert_eq!(
            graph.vertex_props("person", 2),
            Some(
                [
                    ScalarValue::String(Some("Bob".to_string())),
                    ScalarValue::Int64(None),
                    ScalarValue::String(None),
                ]
                .as_slice()
            )
        );
    }
}
