use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use minigu_common::data_type::LogicalType;
use minigu_context::procedure::Procedure;
use rayon::ThreadPoolBuilder;

use super::Statistic;
use super::degree_compute_dense::{PathsByLen, PatternDegCache, StarDegCache};
use super::flat_graph::FlatGraph;
use crate::procedures::gcard_query::catalog::{
    DegreeSeqGraphCompressed, EdgeCardinality, PathAlias, make_alt_key,
};
use crate::procedures::gcard_query::statistic::save_statistic;
use crate::procedures::gcard_query::utils::{
    EdgeEndpoints, PathPattern, edge_cardinalities_from_schema, get_edges_from_catalog,
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
    let adj = super::utils::build_undirected_adj(edges);
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

pub(super) fn parse_functional_extension_config(max_k: usize) -> usize {
    std::env::var("GCARD_FUNCTIONAL_EXTENSION")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(max_k)
}

pub(super) fn enumerate_paths_with_functional_extensions(
    edges: &HashMap<String, EdgeEndpoints>,
    max_k: usize,
) -> (PathsByLen, usize) {
    let mut out = enumerate_all_paths_walks_in_schema(edges, max_k);
    let extension = parse_functional_extension_config(max_k);
    if extension == 0 {
        return (out, max_k);
    }

    let max_len = max_k + extension;
    let vertex_types: HashSet<String> = edges
        .values()
        .flat_map(|e| [e.src_label.as_str(), e.dst_label.as_str()])
        .map(String::from)
        .collect();
    let adj = super::utils::build_undirected_adj(edges);

    fn is_functional(edge_label: &str, edges: &HashMap<String, EdgeEndpoints>) -> bool {
        edges
            .get(edge_label)
            .map(|edge| {
                matches!(
                    edge.cardinality,
                    EdgeCardinality::ManyToOne | EdgeCardinality::OneToMany
                )
            })
            .unwrap_or(false)
    }

    fn is_contractible_path(edge_seq: &[String], edges: &HashMap<String, EdgeEndpoints>) -> bool {
        if edge_seq.len() <= 1 {
            return false;
        }

        edge_seq.windows(2).all(|pair| {
            is_functional(&pair[0], edges)
                || is_functional(&pair[1], edges)
                || !matches!(
                    (
                        edges.get(&pair[0]).map(|e| e.cardinality),
                        edges.get(&pair[1]).map(|e| e.cardinality)
                    ),
                    (
                        Some(EdgeCardinality::ManyToMany),
                        Some(EdgeCardinality::ManyToMany)
                    )
                )
        })
    }

    fn dfs(
        adj: &HashMap<String, Vec<(String, String)>>,
        edges: &HashMap<String, EdgeEndpoints>,
        base_max_len: usize,
        max_len: usize,
        node_seq: &mut Vec<String>,
        edge_seq: &mut Vec<String>,
        out: &mut PathsByLen,
    ) {
        let cur_len = edge_seq.len();
        if cur_len > base_max_len && is_contractible_path(edge_seq, edges) {
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
                dfs(adj, edges, base_max_len, max_len, node_seq, edge_seq, out);
                node_seq.pop();
                edge_seq.pop();
            }
        }
    }

    for start in vertex_types {
        let mut node_seq = vec![start];
        let mut edge_seq = Vec::new();
        dfs(
            &adj,
            edges,
            max_k,
            max_len,
            &mut node_seq,
            &mut edge_seq,
            &mut out,
        );
    }

    (out, max_len)
}

pub(crate) fn add_functional_path_aliases(
    graph: &mut DegreeSeqGraphCompressed,
    edges: &HashMap<String, EdgeEndpoints>,
    max_k: usize,
) -> usize {
    let extension = parse_functional_extension_config(max_k);
    if extension == 0 {
        graph.path_aliases.clear();
        return 0;
    }

    let base_paths = enumerate_all_paths_walks_in_schema(edges, max_k);
    let mut aliases = HashMap::new();
    for path in base_paths.values().flat_map(|paths| paths.iter()) {
        let source = path.to_alt_key();
        if !graph.edge_set_to_endpoints.contains_key(&source) {
            continue;
        }
        add_aliases_for_core_path(graph, edges, path, &source, extension, &mut aliases);
    }

    let count = aliases.len();
    graph.path_aliases = aliases;
    count
}

pub(crate) fn add_functional_path_aliases_for_existing_catalog(
    graph: &mut DegreeSeqGraphCompressed,
    edges: &HashMap<String, EdgeEndpoints>,
) -> usize {
    let max_k = graph
        .edge_set_to_endpoints
        .keys()
        .map(|key| key.raw.len().saturating_sub(1) / 2)
        .max()
        .unwrap_or(0);
    if max_k == 0 {
        graph.path_aliases.clear();
        return 0;
    }
    add_functional_path_aliases(graph, edges, max_k)
}

fn add_aliases_for_core_path(
    graph: &DegreeSeqGraphCompressed,
    edges: &HashMap<String, EdgeEndpoints>,
    core: &PathPattern,
    source: &crate::procedures::gcard_query::catalog::AltKey,
    extension: usize,
    aliases: &mut HashMap<crate::procedures::gcard_query::catalog::AltKey, PathAlias>,
) {
    fn rec(
        graph: &DegreeSeqGraphCompressed,
        edges: &HashMap<String, EdgeEndpoints>,
        core: &PathPattern,
        source: &crate::procedures::gcard_query::catalog::AltKey,
        vs: Vec<String>,
        es: Vec<String>,
        remaining: usize,
        aliases: &mut HashMap<crate::procedures::gcard_query::catalog::AltKey, PathAlias>,
    ) {
        if vs.len() > core.vs.len() {
            let alias_key = make_alt_key(&vs, &es);
            if !graph.edge_set_to_endpoints.contains_key(&alias_key) {
                let mut endpoint_map = HashMap::new();
                endpoint_map.insert(vs[0].clone(), core.vs[0].clone());
                endpoint_map.insert(
                    vs.last().expect("path has vertices").clone(),
                    core.vs.last().expect("core path has vertices").clone(),
                );
                aliases.entry(alias_key).or_insert_with(|| PathAlias {
                    source: source.clone(),
                    endpoint_map,
                });
            }
        }
        if remaining == 0 {
            return;
        }

        let start = vs.first().expect("path has vertices").clone();
        for (edge_label, new_start) in functional_predecessors_to(edges, &start) {
            let mut next_vs = Vec::with_capacity(vs.len() + 1);
            next_vs.push(new_start);
            next_vs.extend(vs.iter().cloned());
            let mut next_es = Vec::with_capacity(es.len() + 1);
            next_es.push(edge_label);
            next_es.extend(es.iter().cloned());
            rec(
                graph,
                edges,
                core,
                source,
                next_vs,
                next_es,
                remaining - 1,
                aliases,
            );
        }

        let end = vs.last().expect("path has vertices").clone();
        for (edge_label, new_end) in functional_successors_from(edges, &end) {
            let mut next_vs = vs.clone();
            next_vs.push(new_end);
            let mut next_es = es.clone();
            next_es.push(edge_label);
            rec(
                graph,
                edges,
                core,
                source,
                next_vs,
                next_es,
                remaining - 1,
                aliases,
            );
        }
    }

    rec(
        graph,
        edges,
        core,
        source,
        core.vs.clone(),
        core.es.clone(),
        extension,
        aliases,
    );
}

fn functional_successors_from(
    edges: &HashMap<String, EdgeEndpoints>,
    from: &str,
) -> Vec<(String, String)> {
    edges
        .iter()
        .filter_map(|(edge_label, edge)| {
            if edge.src_label.eq_ignore_ascii_case(from)
                && matches!(edge.cardinality, EdgeCardinality::ManyToOne)
            {
                Some((edge_label.clone(), edge.dst_label.clone()))
            } else if edge.dst_label.eq_ignore_ascii_case(from)
                && matches!(edge.cardinality, EdgeCardinality::OneToMany)
            {
                Some((edge_label.clone(), edge.src_label.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn functional_predecessors_to(
    edges: &HashMap<String, EdgeEndpoints>,
    to: &str,
) -> Vec<(String, String)> {
    edges
        .iter()
        .filter_map(|(edge_label, edge)| {
            if edge.dst_label.eq_ignore_ascii_case(to)
                && matches!(edge.cardinality, EdgeCardinality::OneToMany)
            {
                Some((edge_label.clone(), edge.src_label.clone()))
            } else if edge.src_label.eq_ignore_ascii_case(to)
                && matches!(edge.cardinality, EdgeCardinality::ManyToOne)
            {
                Some((edge_label.clone(), edge.dst_label.clone()))
            } else {
                None
            }
        })
        .collect()
}

// ----- Helper functions -----

fn create_thread_pool(num_threads: usize) -> Result<rayon::ThreadPool, anyhow::Error> {
    ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to create thread pool: {}", e))
}

pub(super) fn parse_star_collection_config(max_k: usize) -> (usize, usize) {
    let max_star_length = std::env::var("GCARD_MAX_STAR_LENGTH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(max_k);
    let max_star_degree = std::env::var("GCARD_MAX_STAR_DEGREE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    (max_star_length.min(max_k), max_star_degree)
}

pub(super) fn build_statistic_from_pattern_cache(
    cache: &PatternDegCache,
) -> Result<Statistic, anyhow::Error> {
    build_statistic_from_pattern_and_star_cache(cache, &StarDegCache::new())
}

pub(super) fn build_statistic_from_pattern_and_star_cache(
    cache: &PatternDegCache,
    star_cache: &StarDegCache,
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
    for (star_key, (vertex_ids, frequencies)) in star_cache {
        statistic
            .insert_or_update_star(
                &star_key.center_label,
                vertex_ids,
                star_key.clone(),
                frequencies,
            )
            .map_err(|e| anyhow::anyhow!("Statistic::insert_or_update_star: {}", e))?;
    }
    println!("=== Per-label path/star count in Statistic ===");
    for (label, ls) in &statistic.label_path_statistic {
        println!(
            "  {:20} paths: {:5}  stars: {:5}  vertices: {:10}",
            label,
            ls.path_statistic.len(),
            ls.star_statistic.len(),
            ls.vertex_ids.len()
        );
    }
    Ok(statistic)
}

fn build_and_persist_statistic(
    cache: &PatternDegCache,
    star_cache: &StarDegCache,
    edges: &HashMap<String, EdgeEndpoints>,
    edge_cardinalities: std::collections::HashMap<
        String,
        crate::procedures::gcard_query::catalog::EdgeCardinality,
    >,
    graph_container: &minigu_context::graph::GraphContainer,
    db_path: &Option<std::path::PathBuf>,
    graph_name: &str,
    max_k: usize,
) -> Result<(), anyhow::Error> {
    let statistic = build_statistic_from_pattern_and_star_cache(cache, star_cache)?;
    let size = statistic.serialized_size();
    println!(
        "Statistic estimated bincode size: {} bytes - {:.2} MB",
        size,
        size as f64 / 1024.0 / 1024.0
    );
    statistic.report_compressed_sizes();

    let mut degree_seq_graph_compressed = statistic
        .to_degree_seq_graph_compressed()
        .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {}", e))?;
    degree_seq_graph_compressed.edge_cardinalities = edge_cardinalities;
    let alias_count = add_functional_path_aliases(&mut degree_seq_graph_compressed, edges, max_k);
    println!(
        "Functional path aliases generated: {} (not scanned)",
        alias_count
    );

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
    ];
    Procedure::new(parameters, None, move |context, args| {
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
        let num_threads = args
            .get(2)
            .and_then(|a| a.to_u8().ok())
            .map(|n| if n == 0 { default_threads } else { n as usize })
            .unwrap_or(default_threads);

        println!(
            "compute_threads={} (available parallelism: {})",
            num_threads, default_threads,
        );
        let (max_star_length, max_star_degree) = parse_star_collection_config(max_k);
        println!(
            "star collection: max_star_length={}, max_star_degree={}{}",
            max_star_length,
            max_star_degree,
            if max_star_degree == 0 {
                " (disabled)"
            } else {
                ""
            },
        );

        let pool = create_thread_pool(num_threads)?;

        let graph_container = context.get_graph_container(graph_name.as_str())?;
        let graph_type_ref = graph_container.graph_type();

        graph_container.clear_gcard_data();

        // ── Get FlatGraph ──
        type FlatGraphType = FlatGraph;
        let flat_graph_arc: Arc<FlatGraphType> = graph_container
            .gcard_flat_graph()
            .and_then(|arc| Arc::downcast::<FlatGraphType>(arc).ok())
            .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded (run load_ldbc first)"))?;

        // ── Build degree cache from FlatGraph ──
        let edges = get_edges_from_catalog(graph_type_ref.as_ref())?;
        let schema_path = enumerate_all_paths_walks_in_schema(&edges, max_k);
        {
            let total: usize = schema_path.values().map(|s| s.len()).sum();
            println!("=== Schema paths (total: {}) ===", total);
            println!(
                "path collection: base_max_k={}, functional_extension_aliases={}, scanned_max_len={}",
                max_k,
                parse_functional_extension_config(max_k),
                max_k
            );
            for len in 1..=max_k {
                if let Some(paths) = schema_path.get(&len) {
                    println!("--- length {} ({} paths) ---", len, paths.len());
                    for p in paths {
                        println!("  {}", p);
                    }
                }
            }
        }

        let scan_start = std::time::Instant::now();
        eprintln!("GCard_build: scanning hops from FlatGraph...");
        let scanned = super::degree_compute_dense::scan_all_hops_from_flat_graph(
            &flat_graph_arc,
            &edges,
            &schema_path,
            max_k,
            &pool,
        )?;
        let scan_elapsed = scan_start.elapsed();
        println!(
            "Scan time: {:.3}s ({:.2} MB)",
            scan_elapsed.as_secs_f64(),
            scanned.mem_usage_bytes() as f64 / 1024.0 / 1024.0,
        );

        let scanned = Arc::new(scanned);
        let compute_start = std::time::Instant::now();
        let (cache, star_cache): (PatternDegCache, StarDegCache) =
            super::degree_compute_dense::compute_from_scanned_hops_with_star(
                &scanned,
                &edges,
                max_k,
                max_star_length,
                max_star_degree,
                &pool,
            )?;
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
        build_and_persist_statistic(
            &cache,
            &star_cache,
            &edges,
            edge_cardinalities_from_schema(&edges),
            &graph_container,
            &db_path,
            &graph_name,
            max_k,
        )?;

        Ok(vec![])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedures::gcard_query::catalog::CompressedDegreeSeq;
    use crate::procedures::gcard_query::degreepiecewise::PiecewiseConstantFunction;

    fn edge(src: &str, dst: &str, cardinality: EdgeCardinality) -> EdgeEndpoints {
        EdgeEndpoints {
            src_label: src.to_string(),
            dst_label: dst.to_string(),
            cardinality,
        }
    }

    #[test]
    fn functional_extension_does_not_alias_many_side_predecessor() {
        let mut edges = HashMap::new();
        edges.insert(
            "a_to_b".to_string(),
            edge("A", "B", EdgeCardinality::ManyToOne),
        );
        edges.insert(
            "b_to_c".to_string(),
            edge("B", "C", EdgeCardinality::ManyToMany),
        );
        edges.insert(
            "c_to_d".to_string(),
            edge("C", "D", EdgeCardinality::ManyToMany),
        );

        let core = make_alt_key(
            &["B".to_string(), "C".to_string(), "D".to_string()],
            &["b_to_c".to_string(), "c_to_d".to_string()],
        );
        let long = make_alt_key(
            &[
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
            ],
            &[
                "a_to_b".to_string(),
                "b_to_c".to_string(),
                "c_to_d".to_string(),
            ],
        );

        let mut graph = DegreeSeqGraphCompressed::new();
        graph
            .edge_set_to_endpoints
            .entry(core.clone())
            .or_default()
            .insert(
                "B".to_string(),
                CompressedDegreeSeq::SafeBound {
                    function: PiecewiseConstantFunction {
                        constants: vec![3.0],
                        right_interval_edges: vec![2.0],
                        cumulative_rows: vec![6.0],
                    },
                },
            );
        graph
            .edge_set_to_endpoints
            .entry(core.clone())
            .or_default()
            .insert(
                "D".to_string(),
                CompressedDegreeSeq::SafeBound {
                    function: PiecewiseConstantFunction {
                        constants: vec![5.0],
                        right_interval_edges: vec![2.0],
                        cumulative_rows: vec![10.0],
                    },
                },
            );

        let aliases = add_functional_path_aliases(&mut graph, &edges, 2);

        assert!(!graph.edge_set_to_endpoints.contains_key(&long));
        assert!(!graph.path_aliases.contains_key(&long));
        assert!(!graph.path_has_endpoint_pair(&long, "A", "D"));
        assert_eq!(aliases, graph.path_aliases.len());
    }

    #[test]
    fn functional_extension_aliases_one_side_predecessor() {
        let mut edges = HashMap::new();
        edges.insert(
            "a_to_b".to_string(),
            edge("A", "B", EdgeCardinality::OneToMany),
        );
        edges.insert(
            "b_to_c".to_string(),
            edge("B", "C", EdgeCardinality::ManyToMany),
        );
        edges.insert(
            "c_to_d".to_string(),
            edge("C", "D", EdgeCardinality::ManyToMany),
        );

        let core = make_alt_key(
            &["B".to_string(), "C".to_string(), "D".to_string()],
            &["b_to_c".to_string(), "c_to_d".to_string()],
        );
        let long = make_alt_key(
            &[
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
            ],
            &[
                "a_to_b".to_string(),
                "b_to_c".to_string(),
                "c_to_d".to_string(),
            ],
        );

        let mut graph = DegreeSeqGraphCompressed::new();
        graph
            .edge_set_to_endpoints
            .entry(core.clone())
            .or_default()
            .insert(
                "B".to_string(),
                CompressedDegreeSeq::SafeBound {
                    function: PiecewiseConstantFunction {
                        constants: vec![3.0],
                        right_interval_edges: vec![2.0],
                        cumulative_rows: vec![6.0],
                    },
                },
            );
        graph.edge_set_to_endpoints.entry(core).or_default().insert(
            "D".to_string(),
            CompressedDegreeSeq::SafeBound {
                function: PiecewiseConstantFunction {
                    constants: vec![5.0],
                    right_interval_edges: vec![2.0],
                    cumulative_rows: vec![10.0],
                },
            },
        );

        let aliases = add_functional_path_aliases(&mut graph, &edges, 2);

        assert!(aliases > 0);
        assert!(!graph.edge_set_to_endpoints.contains_key(&long));
        assert!(graph.path_aliases.contains_key(&long));
        assert!(graph.path_has_endpoint_pair(&long, "A", "D"));
        assert_eq!(graph.get_piece_func_by_path(&long, "A").get_num_rows(), 6.0);
        assert_eq!(
            graph.get_piece_func_by_path(&long, "D").get_num_rows(),
            10.0
        );
    }
}
