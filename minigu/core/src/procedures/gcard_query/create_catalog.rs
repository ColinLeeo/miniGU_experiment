use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use minigu_catalog::provider::GraphTypeProvider;
use minigu_common::data_type::LogicalType;
use minigu_common::types::{LabelId, VertexId};
use minigu_context::graph::GraphStorage;
use minigu_context::procedure::Procedure;
use minigu_transaction::IsolationLevel::Serializable;
use minigu_transaction::{GraphTxnManager, Transaction};
use rayon::ThreadPoolBuilder;

use super::Statistic;
use super::degree_compute_dense::{PathsByLen, PatternDegCache, ScannedHops};
use crate::procedures::gcard_query::statistic::save_statistic;
use crate::procedures::gcard_query::utils::{
    EdgeEndpoints, PathPattern, build_undirected_adj, get_edges_from_catalog,
};

// ----- Schema path enumeration -----

pub(super) fn enumerate_all_paths_walks_in_schema(
    edges: &HashMap<String, EdgeEndpoints>,
    max_len: usize,
) -> PathsByLen {
    let vertex_types: HashSet<String> = edges
        .values()
        .flat_map(|e| [e.src_label.as_str(), e.dst_label.as_str()])
        .map(String::from)
        .collect();
    let adj = build_undirected_adj(edges);
    let mut out: PathsByLen = HashMap::new();

    fn dfs(
        adj: &HashMap<String, Vec<(String, String)>>,
        max_len: usize,
        node_seq: &mut Vec<String>,
        edge_seq: &mut Vec<String>,
        out: &mut PathsByLen,
    ) {
        let cur_len = edge_seq.len();
        if cur_len > 0 {
            out.entry(cur_len)
                .or_default()
                .insert(PathPattern::new(node_seq.clone(), edge_seq.clone()));
        }
        if cur_len == max_len {
            return;
        }
        let cur_node = node_seq.last().unwrap().clone();
        if let Some(nbrs) = adj.get(&cur_node) {
            for (edge_name, next_node) in nbrs.iter() {
                edge_seq.push(edge_name.clone());
                node_seq.push(next_node.clone());
                dfs(adj, max_len, node_seq, edge_seq, out);
                node_seq.pop();
                edge_seq.pop();
            }
        }
    }

    for start in vertex_types.iter() {
        let mut node_seq = vec![start.clone()];
        let mut edge_seq: Vec<String> = Vec::new();
        dfs(&adj, max_len, &mut node_seq, &mut edge_seq, &mut out);
    }
    out
}

// ----- Helper functions -----

fn build_label_name_to_id(graph_type: &dyn GraphTypeProvider) -> HashMap<String, LabelId> {
    let mut map = HashMap::new();
    for name in graph_type.label_names() {
        if let Ok(Some(id)) = graph_type.get_label_id(&name) {
            map.insert(name.clone(), id);
        }
    }
    map
}

fn create_thread_pools(
    num_threads: usize,
    scan_threads: usize,
) -> Result<(rayon::ThreadPool, rayon::ThreadPool), anyhow::Error> {
    let pool = ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to create thread pool: {}", e))?;
    let scan_pool = ThreadPoolBuilder::new()
        .num_threads(scan_threads)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to create scan thread pool: {}", e))?;
    Ok((pool, scan_pool))
}

pub(super) fn build_statistic_from_pattern_cache(
    cache: &PatternDegCache,
) -> Result<Statistic, anyhow::Error> {
    let mut statistic = Statistic::default();
    for (path_pattern, endpoint_degrees) in cache {
        let alt_key = path_pattern.to_alt_key();
        for (node_name, (vertex_ids, frequencies)) in endpoint_degrees {
            statistic
                .insert_or_update(node_name, vertex_ids, alt_key.clone(), frequencies)
                .map_err(|e| anyhow::anyhow!("Statistic::insert_or_update: {}", e))?;
        }
    }
    println!("=== Per-label path count in Statistic ===");
    for (label, ls) in &statistic.label_path_statistic {
        println!(
            "  {:20} paths: {:5}  vertices: {:10}",
            label,
            ls.path_statistic.len(),
            ls.vertex_ids.len()
        );
    }
    Ok(statistic)
}

fn build_and_persist_statistic(
    cache: &PatternDegCache,
    graph_container: &minigu_context::graph::GraphContainer,
    db_path: &Option<std::path::PathBuf>,
    graph_name: &str,
    max_k: usize,
) -> Result<(), anyhow::Error> {
    let statistic = build_statistic_from_pattern_cache(cache)?;
    let size = statistic.serialized_size();
    println!(
        "Statistic estimated bincode size: {} bytes - {:.2} MB",
        size,
        size as f64 / 1024.0 / 1024.0
    );
    statistic.report_compressed_sizes();

    let degree_seq_graph_compressed = statistic
        .to_degree_seq_graph_compressed()
        .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {}", e))?;

    if let Some(db_path) = db_path {
        save_statistic(db_path, graph_name, &statistic)?;
    }

    graph_container.set_degree_seq_graph_compressed(Arc::new(degree_seq_graph_compressed));
    graph_container.set_statistic(Arc::new(statistic));

    let update_log = crate::procedures::gcard_query::update_log::new_log_arc(max_k);
    graph_container.set_gcard_update_log(update_log);

    Ok(())
}

// ----- Procedure entry point -----

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String, // graph_name
        LogicalType::Int8,   // max_k
        LogicalType::UInt8,  // compute_threads
        LogicalType::UInt8,  // scan_threads
        LogicalType::String, // optional: lightgraph bincode path (from load_ldbc)
    ];
    Procedure::new(parameters, None, move |context, args| {
        // 参数既控制路径最大长度，也控制扫描/计算的并行度。
        let graph_name = args[0]
            .try_as_string()
            .expect("expecting string value for graph_name")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expecting string value for graph name"))?
            .to_string();
        let max_k = args[1]
            .try_as_int8()
            .expect("max length of path must be int8")
            .ok_or_else(|| anyhow::anyhow!("expecting int8 for path length"))?
            as usize;
        let db_path = context.database().config().db_path.clone();

        let default_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        let parse_threads = |idx: usize| {
            args.get(idx)
                .and_then(|a| a.to_u8().ok())
                .map(|n| if n == 0 { default_threads } else { n as usize })
                .unwrap_or(default_threads)
        };
        let num_threads = parse_threads(2);
        let scan_threads = parse_threads(3);

        // Optional 5th arg: path to LightGraph bincode (from load_ldbc)
        let lightgraph_path: Option<String> = args
            .get(4)
            .and_then(|a| a.try_as_string())
            .and_then(|opt: &Option<String>| opt.clone());

        println!(
            "compute_threads={}, scan_threads={}, lightgraph={} (available parallelism: {})",
            num_threads,
            scan_threads,
            lightgraph_path.as_deref().unwrap_or("(none)"),
            default_threads,
        );

        // ── Setup graph & thread pools ──
        let (pool, scan_pool) = create_thread_pools(num_threads, scan_threads)?;

        let graph_container = context.get_graph_container(graph_name.as_str())?;
        let graph_type_ref = graph_container.graph_type();

        graph_container.clear_gcard_data();

        // ── Build degree cache ──
        let edges = get_edges_from_catalog(graph_type_ref.as_ref())?;
        let schema_path = enumerate_all_paths_walks_in_schema(&edges, max_k);
        {
            let total: usize = schema_path.values().map(|s| s.len()).sum();
            println!("=== Schema paths (total: {}) ===", total);
            for len in 1..=max_k {
                if let Some(paths) = schema_path.get(&len) {
                    println!("--- length {} ({} paths) ---", len, paths.len());
                    for p in paths {
                        println!("  {}", p);
                    }
                }
            }
        }

        // ── Phase 1: Scan neighbor data ──
        let scan_start = std::time::Instant::now();
        let scanned: Arc<ScannedHops> = if let Some(cached) = graph_container.gcard_scanned_hops() {
            // ── Fastest path: reuse in-memory cached ScannedHops ──
            if let Ok(s) = cached.downcast::<ScannedHops>() {
                println!(
                    "Reusing cached scanned hops ({:.2} MB)",
                    s.mem_usage_bytes() as f64 / 1024.0 / 1024.0
                );
                s
            } else {
                return Err(anyhow::anyhow!("cached scanned hops downcast failed").into());
            }
        } else if let Some(bin_path) = lightgraph_path {
            let p = std::path::Path::new(&bin_path);
            let s = if bin_path.ends_with(".scanned_hops.bin") {
                // ── Fast path: deserialize ScannedHops from bincode ──
                Arc::new(ScannedHops::load_bincode(p)?)
            } else {
                // ── Load LightGraph, rebuild ScannedHops ──
                let light_graph = super::load_ldbc::load_light_graph(p)
                    .map_err(|e| anyhow::anyhow!("load lightgraph: {}", e))?;
                let scanned = super::load_ldbc::scan_from_light_graph(
                    &light_graph,
                    &edges,
                    &schema_path,
                    max_k,
                    &scan_pool,
                )?;
                drop(light_graph);
                Arc::new(scanned)
            };
            // Cache it for subsequent calls in the same process
            graph_container
                .set_gcard_scanned_hops(Arc::clone(&s) as Arc<dyn std::any::Any + Send + Sync>);
            s
        } else {
            let graph = match graph_container.graph_storage() {
                GraphStorage::Memory(g) => Arc::clone(g),
            };
            let label_name_to_id = build_label_name_to_id(graph_type_ref.as_ref());
            let txn = graph
                .txn_manager()
                .begin_transaction(Serializable)
                .map_err(|e| anyhow::anyhow!("begin_transaction: {}", e))?;

            let s = pool.install(|| {
                super::degree_compute_dense::scan_all_hops(
                    &txn,
                    &edges,
                    &label_name_to_id,
                    &schema_path,
                    max_k,
                    &scan_pool,
                )
            })?;

            txn.commit()
                .map_err(|e| anyhow::anyhow!("commit transaction: {}", e))?;
            drop(txn);
            graph.clear_in_memory_data();
            drop(graph);
            println!("Graph released from memory");

            let s = Arc::new(s);
            graph_container
                .set_gcard_scanned_hops(Arc::clone(&s) as Arc<dyn std::any::Any + Send + Sync>);
            s
        };
        let scan_elapsed = scan_start.elapsed();
        println!(
            "Scan time: {:.3}s (neighbor cache: {:.2} MB)",
            scan_elapsed.as_secs_f64(),
            scanned.mem_usage_bytes() as f64 / 1024.0 / 1024.0,
        );

        let compute_start = std::time::Instant::now();
        let cache: PatternDegCache =
            super::degree_compute_dense::compute_from_scanned_hops(&scanned, &edges, max_k, &pool)?;
        let compute_elapsed = compute_start.elapsed();
        println!("Compute time: {:.3}s", compute_elapsed.as_secs_f64());
        println!(
            "Catalog build time: {:.3}s (scan: {:.3}s + compute: {:.3}s)",
            (scan_elapsed + compute_elapsed).as_secs_f64(),
            scan_elapsed.as_secs_f64(),
            compute_elapsed.as_secs_f64(),
        );
        drop(scanned);

        // ── Build statistic & persist ──
        build_and_persist_statistic(&cache, &graph_container, &db_path, &graph_name, max_k)?;

        Ok(vec![])
    })
}
