mod abs_graph;
mod block_statistic;
mod catalog;
pub mod compact_update_log;
pub mod compression;
pub mod create_catalog;
mod degree_compute;
mod degree_compute_dense;
mod degreepiecewise;
pub mod error;
pub mod export_edge_csv;
pub mod export_flatgraph_snapshot;
pub mod flat_graph;
pub mod stat_quality;
mod statistic;
pub mod update_log;

pub use block_statistic::BlockStatistic;
pub use catalog::make_alt_key;
pub use statistic::{Statistic, load_statistic};
mod graph;
pub mod load_catalog;
pub mod load_flatgraph;
pub mod load_ldbc;
mod query_graph;
pub mod random_insert;
pub mod random_update;
pub mod types;
mod union_find;
pub mod utils;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::{fmt, fs, io};

use minigu_common::data_chunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;

use crate::procedures::gcard_query::abs_graph::AbstractGraph;
use crate::procedures::gcard_query::catalog::DegreeSeqGraphCompressed;
use crate::procedures::gcard_query::query_graph::QueryGraph;
use crate::procedures::gcard_query::types::{DecompositionDef, Query};

static GCARD_VERBOSE: AtomicBool = AtomicBool::new(false);
pub(crate) static SAMPLING_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
// Fine-grained profiling counters for sampling breakdown
pub(crate) static SAMPLING_CALLS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static SAMPLING_TOTAL_WALKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static SAMPLING_VERTICES_PROCESSED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static SAMPLING_NBR_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static SAMPLING_PROP_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
// Build phase breakdown timers
pub(crate) static BUILD_CYCLE_CHECK_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static BUILD_SCORE_TREE_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static BUILD_PIVOT_PATH_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static BUILD_ABSTRACT_EDGE_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static BUILD_PCF_LOOKUP_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
use std::convert::TryFrom;

use serde::{Deserialize, Serialize};

impl TryFrom<u8> for PredicateApplyType {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PredicateApplyType::INNER),
            2 => Ok(PredicateApplyType::IGNORE),
            _ => Err("invalid PredicateApplyType value"),
        }
    }
}

impl fmt::Display for PredicateApplyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PredicateApplyType::INNER => write!(f, "INNER"),
            PredicateApplyType::IGNORE => write!(f, "IGNORE"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum PredicateApplyType {
    INNER,
    IGNORE,
}

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String,  // query_json_path
        LogicalType::Int8,    // max_path_length (K)
        LogicalType::Int32,   // sample_size
        LogicalType::UInt8,   // predicate apply type
        LogicalType::Boolean, // verbose
        LogicalType::Int32,   // max_subgraphs (optional, default 50)
        LogicalType::String,  // decomposition_json_path (optional, null = auto decompose)
    ];

    let schema = Arc::new(DataSchema::new(vec![DataField::new(
        "cardinality".into(),
        LogicalType::Int64,
        false,
    )]));

    Procedure::new(parameters, Some(schema), move |context, args| {
        let graph_ref = context.current_graph.clone().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "current graph is not selected",
            )))
        })?;
        let provider = graph_ref.object().clone();
        let container = provider.downcast_ref::<GraphContainer>().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "only in-memory graphs support vector scans",
            )))
        })?;
        let metadata: DegreeSeqGraphCompressed = container
            .degree_seq_graph_compressed()
            .as_ref()
            .and_then(|arc| arc.downcast_ref::<DegreeSeqGraphCompressed>().cloned())
            .ok_or_else(|| {
                anyhow::anyhow!("degree_seq_graph_compressed not set (run GCard_build first)")
            })?;

        let query_json_path = args[0]
            .try_as_string()
            .expect("first arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("query_json_path cannot be null"))?
            .to_string();
        let simple_size = args[2]
            .to_i32()
            .map_err(|e| anyhow::anyhow!("Failed to convert sample size to i32: {:?}", e))?
            as usize;

        #[allow(unused_variables)]
        let verbose = args
            .get(4)
            .map(|a| a.to_bool().unwrap_or(false))
            .unwrap_or(false);
        GCARD_VERBOSE.store(verbose, Ordering::Relaxed);

        let query_json = fs::read_to_string(&query_json_path).map_err(|e| {
            anyhow::anyhow!("Failed to read query JSON file {}: {}", query_json_path, e)
        })?;

        let query: Query = serde_json::from_str(&query_json)
            .map_err(|e| anyhow::anyhow!("Failed to parse query JSON: {}", e))?;

        let query_graph: QueryGraph = query
            .build_graph()
            .map_err(|e| anyhow::anyhow!("Failed to build query graph: {}", e))?;

        let max_path_length = args[1]
            .to_i32()
            .map_err(|e| anyhow::anyhow!("Failed to convert max_path_length to i32: {:?}", e))?
            as usize;

        let predicate_apply_type: PredicateApplyType = args[3]
            .to_u8()
            .map_err(|e| anyhow::anyhow!("Failed to convert predicate apply type to u8: {:?}", e))?
            .try_into()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to convert predicate apply type to PredicateApplyType: {}",
                    e
                )
            })?;

        let max_subgraphs = args
            .get(5)
            .and_then(|a| a.to_i32().ok())
            .map(|n| if n <= 0 { 10 } else { n as usize })
            .unwrap_or(10);

        let decomposition_path: Option<String> = args
            .get(6)
            .and_then(|a| a.try_as_string())
            .and_then(|opt| opt.clone());
        let decomposition: Option<DecompositionDef> = decomposition_path
            .map(|path| {
                let json = fs::read_to_string(&path).map_err(|e| {
                    anyhow::anyhow!("Failed to read decomposition JSON {}: {}", path, e)
                })?;
                let def: DecompositionDef = serde_json::from_str(&json)
                    .map_err(|e| anyhow::anyhow!("Failed to parse decomposition JSON: {}", e))?;
                Ok::<_, anyhow::Error>(def)
            })
            .transpose()?;

        #[cfg(feature = "profiling")]
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(1000)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()
            .expect("failed to start pprof profiler");

        SAMPLING_NANOS.store(0, Ordering::Relaxed);
        SAMPLING_CALLS.store(0, Ordering::Relaxed);
        SAMPLING_TOTAL_WALKS.store(0, Ordering::Relaxed);
        SAMPLING_VERTICES_PROCESSED.store(0, Ordering::Relaxed);
        SAMPLING_NBR_NANOS.store(0, Ordering::Relaxed);
        SAMPLING_PROP_NANOS.store(0, Ordering::Relaxed);
        BUILD_CYCLE_CHECK_NANOS.store(0, Ordering::Relaxed);
        BUILD_SCORE_TREE_NANOS.store(0, Ordering::Relaxed);
        BUILD_PIVOT_PATH_NANOS.store(0, Ordering::Relaxed);
        BUILD_ABSTRACT_EDGE_NANOS.store(0, Ordering::Relaxed);
        BUILD_PCF_LOOKUP_NANOS.store(0, Ordering::Relaxed);

        // Prefer FlatGraph path when available (avoids MemoryGraph / MVCC overhead).
        type FlatGraphType = crate::procedures::gcard_query::flat_graph::FlatGraph;
        let flat_graph_arc: Option<std::sync::Arc<FlatGraphType>> = container
            .gcard_flat_graph()
            .and_then(|arc| std::sync::Arc::downcast::<FlatGraphType>(arc).ok());

        let build_start = Instant::now();
        let flat_graph_ref = flat_graph_arc.as_deref().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "FlatGraph not loaded (run load_ldbc first)",
            )))
        })?;
        let build_result = if let Some(ref decomp) = decomposition {
            query_graph.build_abstract_graph_flat_from_decomposition(
                decomp,
                &metadata,
                Some(flat_graph_ref),
                simple_size,
                &predicate_apply_type,
            )
        } else {
            query_graph.build_abstract_graph_flat(
                max_path_length,
                max_subgraphs,
                &metadata,
                Some(flat_graph_ref),
                simple_size,
                &predicate_apply_type,
            )
        };
        let cardinality = match build_result {
            Ok(abstract_graphs_with_scores) => {
                let build_elapsed = build_start.elapsed();
                let sampling_secs = SAMPLING_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let estimate_start = Instant::now();
                let total_count = abstract_graphs_with_scores.len();
                let mut min_nonzero_es = f64::INFINITY;
                let mut score_of_min_es: Option<u64> = None;
                let mut index_of_min_es: Option<usize> = None;
                let mut max_score: u64 = 0;
                #[allow(dead_code)]
                let mut min_es_abstract_graph: Option<AbstractGraph> = None;
                for (idx, (mut abs, score)) in abstract_graphs_with_scores.into_iter().enumerate() {
                    if score > max_score {
                        max_score = score;
                    }
                    let abs_for_debug = abs.clone();

                    if GCARD_VERBOSE.load(Ordering::Relaxed) {
                        println!("use predicate type:{}", predicate_apply_type.to_string());
                        println!("idx:{}", idx);
                        for (edge_id, edge) in &abs_for_debug.edges {
                            println!("edge: {}, selectivity: {}", edge.path_str, edge.selectivity);
                            println!("src : {}", edge.src_pcf);
                            println!("dst : {}", edge.dst_pcf);
                        }
                    }
                    let mut es = abs.get_es().map_err(|e| {
                        ExecutionError::Custom(Box::new(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("GCard get_es: {}", e),
                        )))
                    })?;

                    if GCARD_VERBOSE.load(Ordering::Relaxed) {
                        println!("es: {}", es);
                    }
                    if es > 1.0 {
                        if es < min_nonzero_es {
                            min_nonzero_es = es;
                            score_of_min_es = Some(score);
                            index_of_min_es = Some(idx + 1);
                            min_es_abstract_graph = Some(abs_for_debug);
                        }
                    }
                }
                let cardinality_value = if min_nonzero_es.is_finite() {
                    min_nonzero_es
                } else {
                    0.0
                };
                let estimate_elapsed = estimate_start.elapsed();
                let is_highest = score_of_min_es.map(|s| s == max_score).unwrap_or(false);
                let display_index = if is_highest { Some(1) } else { index_of_min_es };
                let prof_calls = SAMPLING_CALLS.load(Ordering::Relaxed);
                let prof_walks = SAMPLING_TOTAL_WALKS.load(Ordering::Relaxed);
                let prof_verts = SAMPLING_VERTICES_PROCESSED.load(Ordering::Relaxed);
                let prof_nbr_s = SAMPLING_NBR_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_prop_s = SAMPLING_PROP_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_cycle_s = BUILD_CYCLE_CHECK_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_tree_s = BUILD_SCORE_TREE_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_pivot_s = BUILD_PIVOT_PATH_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_ae_s = BUILD_ABSTRACT_EDGE_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                let prof_pcf_s = BUILD_PCF_LOOKUP_NANOS.load(Ordering::Relaxed) as f64 / 1e9;
                print!(
                    "total: {}, min_es index: {:?}, is highest: {}, sample_time: {:.6}, estimate_time: {:.6}, build_time: {:.6}, ",
                    total_count,
                    display_index,
                    is_highest,
                    sampling_secs,
                    estimate_elapsed.as_secs_f64(),
                    build_elapsed.as_secs_f64(),
                );
                eprintln!(
                    "[build-prof] cycle_check: {:.6}s, score+tree_enum: {:.6}s, pivot+path: {:.6}s, abstract_edge: {:.6}s, pcf_lookup: {:.6}s",
                    prof_cycle_s, prof_tree_s, prof_pivot_s, prof_ae_s, prof_pcf_s,
                );
                if prof_calls > 0 {
                    eprintln!(
                        "[sampling-prof] calls={}, vertices={}, walks={}, nbr_time={:.4}s, prop_time={:.4}s, walks/vert={:.1}",
                        prof_calls,
                        prof_verts,
                        prof_walks,
                        prof_nbr_s,
                        prof_prop_s,
                        if prof_verts > 0 {
                            prof_walks as f64 / prof_verts as f64
                        } else {
                            0.0
                        },
                    );
                }
                cardinality_value
            }
            Err(e) => {
                eprintln!(
                    "GCard: unsupported predicate or error, returning 0.0: {}",
                    e
                );
                0.0
            }
        };

        #[cfg(feature = "profiling")]
        {
            if let Ok(report) = guard.report().build() {
                let svg_path = format!(
                    "gcard_flamegraph_{}.svg",
                    Path::new(query_json_path.as_str())
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                );
                if let Ok(file) = std::fs::File::create(&svg_path) {
                    let _ = report.flamegraph(file);
                    println!("[profiling] flamegraph saved to: {}", svg_path);
                }
            }
        }
        let stem = Path::new(query_json_path.as_str())
            .file_stem()
            .and_then(|n| n.to_str());
        println!("{}, cardinality: {}", stem.unwrap(), cardinality.ceil());

        Ok(vec![data_chunk!((
            Int64,
            [Some(cardinality.ceil() as i64)]
        ))])
    })
}
