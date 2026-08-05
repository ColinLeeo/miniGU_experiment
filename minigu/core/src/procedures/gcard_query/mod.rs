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
pub mod gcard_snapshot;
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
mod predicate;
mod query_graph;
pub mod random_insert;
pub mod random_update;
pub mod types;
mod union_find;
pub mod utils;
pub mod wander_join;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
use std::{fmt, fs, io};

use minigu_common::data_chunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;

use crate::procedures::gcard_query::abs_graph::AbstractGraph;
use crate::procedures::gcard_query::catalog::DegreeSeqGraphCompressed;
use crate::procedures::gcard_query::degreepiecewise::{
    AlphaBetaCallCounts, get_alpha_beta_call_counts, reset_alpha_beta_call_counts,
};
use crate::procedures::gcard_query::query_graph::QueryGraph;
use crate::procedures::gcard_query::types::{DecompositionDef, Query};

static GCARD_VERBOSE: AtomicBool = AtomicBool::new(false);
static GCARD_FUNCTIONAL_REFINE: AtomicBool = AtomicBool::new(true);
static GCARD_MAX_BUCKET: AtomicBool = AtomicBool::new(true);
pub(crate) const GCARD_STAR_CONFIG_UNSET: usize = usize::MAX;
pub(crate) static GCARD_MAX_STAR_LENGTH_OVERRIDE: AtomicUsize =
    AtomicUsize::new(GCARD_STAR_CONFIG_UNSET);
pub(crate) static GCARD_MAX_STAR_DEGREE_OVERRIDE: AtomicUsize =
    AtomicUsize::new(GCARD_STAR_CONFIG_UNSET);
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

// Filter-pool cache + stage-2 / failure counters
pub(crate) static FILTER_POOL_CACHE_HITS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static FILTER_POOL_CACHE_MISSES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static FILTER_POOL_EMPTY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static STAGE2_TRIGGERED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static STAGE2_RESULT_ZERO: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static SELECTIVITY_ZERO: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Times a sampling miss (zero structurally-valid samples, but the predicate
/// pool is *not* empty) fell back to the static stats-based predicate
/// selectivity instead of collapsing the estimate to zero.
pub(crate) static SELECTIVITY_FALLBACK: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Times the pre-walk guard skipped sampling entirely because even the best
/// anchor's expected structural hit count was below one, and returned the
/// static fallback directly.
pub(crate) static WALK_GUARD_SKIPPED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

use std::convert::TryFrom;

use serde::{Deserialize, Serialize};

impl TryFrom<u8> for PredicateApplyType {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PredicateApplyType::INNER),
            1 => Ok(PredicateApplyType::SCALE),
            2 => Ok(PredicateApplyType::IGNORE),
            _ => Err("invalid PredicateApplyType value"),
        }
    }
}

impl fmt::Display for PredicateApplyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PredicateApplyType::INNER => write!(f, "INNER"),
            PredicateApplyType::SCALE => write!(f, "SCALE"),
            PredicateApplyType::IGNORE => write!(f, "IGNORE"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum PredicateApplyType {
    INNER,
    SCALE,
    IGNORE,
}

pub(crate) fn functional_refine_enabled() -> bool {
    GCARD_FUNCTIONAL_REFINE.load(Ordering::Relaxed)
}

pub(crate) fn max_bucket_enabled() -> bool {
    GCARD_MAX_BUCKET.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosureFactorMode {
    Off,
    PositiveRank,
}

fn closure_factor_mode() -> ClosureFactorMode {
    std::env::var("GCARD_CLOSURE_FACTOR")
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "positive_rank" => ClosureFactorMode::PositiveRank,
            _ => ClosureFactorMode::Off,
        })
        .unwrap_or(ClosureFactorMode::Off)
}

pub fn set_session_guc(name: &str, value: bool) -> std::io::Result<()> {
    if name.eq_ignore_ascii_case("functional_refine") {
        GCARD_FUNCTIONAL_REFINE.store(value, Ordering::Relaxed);
        return Ok(());
    }
    if name.eq_ignore_ascii_case("max_bucket") {
        GCARD_MAX_BUCKET.store(value, Ordering::Relaxed);
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("unknown session GUC: {}", name),
    ))
}

pub fn unset_session_guc(name: &str) -> std::io::Result<()> {
    if name.eq_ignore_ascii_case("functional_refine") {
        GCARD_FUNCTIONAL_REFINE.store(true, Ordering::Relaxed);
        return Ok(());
    }
    if name.eq_ignore_ascii_case("max_bucket") {
        GCARD_MAX_BUCKET.store(true, Ordering::Relaxed);
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("unknown session GUC: {}", name),
    ))
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
        LogicalType::Boolean, // unit_selectivity_walks (optional, default false)
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
        let unit_selectivity_walks = args.get(7).and_then(|a| a.to_bool().ok()).unwrap_or(false);
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
        let guard = {
            let freq = std::env::var("GCARD_PROFILE_FREQ")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .filter(|&v| v > 0)
                .unwrap_or(1000);
            eprintln!("[profiling] pprof started at {} Hz", freq);
            pprof::ProfilerGuardBuilder::default()
                .frequency(freq)
                .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                .build()
                .expect("failed to start pprof profiler")
        };

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
        // NOTE: SELECTIVITY_FALLBACK / WALK_GUARD_SKIPPED are intentionally NOT
        // reset here — like the other [cache-prof] counters (filter_pool_*,
        // stage2_*, selectivity_zero) they accumulate across all gcard_query
        // calls in the process, so the final [cache-prof] line reports true
        // cumulative totals rather than only the last query's values.

        // Prefer FlatGraph path when available (avoids MemoryGraph / MVCC overhead).
        type FlatGraphType = crate::procedures::gcard_query::flat_graph::FlatGraph;
        let flat_graph_arc: Option<std::sync::Arc<FlatGraphType>> = container
            .gcard_flat_graph()
            .and_then(|arc| std::sync::Arc::downcast::<FlatGraphType>(arc).ok());

        let inference_nanos: u128;
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
                unit_selectivity_walks,
            )
        } else {
            query_graph.build_abstract_graph_flat(
                max_path_length,
                max_subgraphs,
                &metadata,
                Some(flat_graph_ref),
                simple_size,
                &predicate_apply_type,
                unit_selectivity_walks,
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
                let mut closure_factor_of_min_es: Option<f64> = None;
                let mut max_score: u64 = 0;
                #[allow(dead_code)]
                let mut min_es_abstract_graph: Option<AbstractGraph> = None;
                let plan_trace_log_path = std::env::var_os("GCARD_PLAN_TRACE_LOG");
                let decomp_trace_log_path = std::env::var_os("GCARD_DECOMP_TRACE_LOG");
                let print_plan = std::env::var_os("GCARD_PRINT_PLAN").is_some()
                    || plan_trace_log_path.is_some()
                    || decomp_trace_log_path.is_some();
                let print_alpha_beta =
                    print_plan || std::env::var_os("GCARD_PRINT_ALPHA_BETA").is_some();
                let suppress_candidate_logs =
                    std::env::var_os("GCARD_SUPPRESS_CANDIDATE_LOGS").is_some();
                let mut min_es_alpha_beta_counts: Option<AlphaBetaCallCounts> = None;
                let cand_query_stem = Path::new(query_json_path.as_str())
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
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
                    if print_alpha_beta {
                        reset_alpha_beta_call_counts();
                    }
                    let mut es = abs.get_es().map_err(|e| {
                        ExecutionError::Custom(Box::new(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("GCard get_es: {}", e),
                        )))
                    })?;
                    let alpha_beta_counts = if print_alpha_beta {
                        Some(get_alpha_beta_call_counts())
                    } else {
                        None
                    };
                    let raw_es = es;
                    let closure_factor = match closure_factor_mode() {
                        ClosureFactorMode::Off => 1.0,
                        ClosureFactorMode::PositiveRank => query_graph
                            .estimate_positive_rank_closure_factor_for_abstract_graph_flat(
                                &abs_for_debug,
                                &metadata,
                                Some(flat_graph_ref),
                            ),
                    };
                    es *= closure_factor;

                    if GCARD_VERBOSE.load(Ordering::Relaxed) {
                        println!("es: {}", es);
                    }
                    if !suppress_candidate_logs {
                        // Per-candidate abstract graph dump so diagnostics can inspect every
                        // spanning tree. Formal inference timing suppresses this output.
                        let total_ae = abs.edges.len();
                        let n1_ae = abs
                            .edges
                            .values()
                            .filter(|e| {
                                e.functional
                                != crate::procedures::gcard_query::types::FunctionalDirection::None
                            })
                            .count();
                        println!(
                            "[gcard-cand] query={} idx={}/{} score={} card={} raw_card={} closure_factor={:.9} n1_edges={}/{}",
                            cand_query_stem,
                            idx + 1,
                            total_count,
                            score,
                            es.ceil(),
                            raw_es.ceil(),
                            closure_factor,
                            n1_ae,
                            total_ae
                        );
                        let mut ae_ids: Vec<_> = abs.edges.keys().copied().collect();
                        ae_ids.sort_unstable();
                        for ae_id in ae_ids {
                            let edge = &abs.edges[&ae_id];
                            let src_label = abs
                                .vertices
                                .get(&edge.src)
                                .map(|v| v.label.as_str())
                                .unwrap_or("?");
                            let dst_label = abs
                                .vertices
                                .get(&edge.dst)
                                .map(|v| v.label.as_str())
                                .unwrap_or("?");
                            let functional_tag = match edge.functional {
                            crate::procedures::gcard_query::types::FunctionalDirection::None => {
                                "none"
                            }
                            crate::procedures::gcard_query::types::FunctionalDirection::SrcToDst => {
                                "src->dst"
                            }
                            crate::procedures::gcard_query::types::FunctionalDirection::DstToSrc => {
                                "dst->src"
                            }
                            crate::procedures::gcard_query::types::FunctionalDirection::Both => {
                                "both"
                            }
                        };
                            let n1 = edge.functional
                                != crate::procedures::gcard_query::types::FunctionalDirection::None;
                            // Interleave path_vertices and original_edge_ids so intermediate
                            // hops are visible, resolving each id to its query-graph label.
                            let mut chain = String::new();
                            for (i, vid) in edge.path_vertices.iter().enumerate() {
                                if i > 0 {
                                    let edge_label = edge
                                        .original_edge_ids
                                        .get(i - 1)
                                        .and_then(|eid| query_graph.inner.edges.get(eid))
                                        .map(|e| e.label.as_str())
                                        .unwrap_or("?");
                                    chain.push_str(&format!(" -[{}]-> ", edge_label));
                                }
                                let vlabel = query_graph
                                    .inner
                                    .vertices
                                    .get(vid)
                                    .map(|v| v.label.as_str())
                                    .unwrap_or("?");
                                chain.push_str(vlabel);
                            }
                            println!(
                                "  ae{}: {}({}) -> {}({}) chain={} sel={:.4} src_rows={:.0} dst_rows={:.0} n1={} functional={}",
                                ae_id,
                                edge.src,
                                src_label,
                                edge.dst,
                                dst_label,
                                chain,
                                edge.selectivity,
                                edge.src_pcf.get_num_rows(),
                                edge.dst_pcf.get_num_rows(),
                                n1,
                                functional_tag,
                            );
                        }
                    }
                    // Cardinality one is a valid result for singleton and
                    // highly selective single-edge queries. Only discard the
                    // zero-PCF placeholder and other non-positive estimates.
                    if es >= 1.0 {
                        if es < min_nonzero_es {
                            min_nonzero_es = es;
                            score_of_min_es = Some(score);
                            index_of_min_es = Some(idx + 1);
                            min_es_abstract_graph = Some(abs_for_debug);
                            min_es_alpha_beta_counts = alpha_beta_counts;
                            closure_factor_of_min_es = Some(closure_factor);
                        }
                    }
                }
                let cardinality_value = if min_nonzero_es.is_finite() {
                    min_nonzero_es
                } else {
                    0.0
                };
                let (sel_n1, sel_total) = match &min_es_abstract_graph {
                    Some(abs) => {
                        let total = abs.edges.len();
                        let n1 = abs
                            .edges
                            .values()
                            .filter(|e| {
                                e.functional
                                    != crate::procedures::gcard_query::types::FunctionalDirection::None
                            })
                            .count();
                        (n1, total)
                    }
                    None => (0, 0),
                };
                if !suppress_candidate_logs {
                    println!(
                        "[gcard-cand-min] query={} selected={} score={} card={} total_candidates={} \
                         selected_n1_edges={}/{} n1_fast_path={} closure_factor={:.9}",
                        cand_query_stem,
                        index_of_min_es
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| "none".to_string()),
                        score_of_min_es
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                        cardinality_value.ceil(),
                        total_count,
                        sel_n1,
                        sel_total,
                        sel_n1 > 0,
                        closure_factor_of_min_es.unwrap_or(1.0),
                    );
                }
                if let (Some(abs), Some(log_path)) =
                    (&min_es_abstract_graph, decomp_trace_log_path.clone())
                {
                    match std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                    {
                        Ok(mut file) => {
                            use std::io::Write;
                            let query_name = Path::new(query_json_path.as_str())
                                .file_stem()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown");
                            let _ = writeln!(
                                file,
                                "==== selected best decomposition query={} selected_index={:?} selected_score={:?} estimate={} ====",
                                query_name, index_of_min_es, score_of_min_es, cardinality_value
                            );
                            let _ = writeln!(file, "{}", abs.describe_plan());
                            let _ = writeln!(file, "{}", abs.describe_reduction_trace());
                            if let Some(counts) = min_es_alpha_beta_counts {
                                let _ = writeln!(
                                    file,
                                    "counts raw_alpha_total={} raw_beta_total={} effective_alpha_total={} effective_beta_total={}",
                                    counts.total_alpha(),
                                    counts.total_beta(),
                                    counts.total_effective_alpha(),
                                    counts.total_effective_beta()
                                );
                            }
                            let _ = writeln!(file, "==== GCARD decomposition trace end ====\n");
                        }
                        Err(err) => {
                            eprintln!(
                                "[gcard-decomp-trace] failed to open {}: {}",
                                Path::new(&log_path).display(),
                                err
                            );
                        }
                    }
                }
                if print_plan {
                    let query_name = Path::new(query_json_path.as_str())
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    eprintln!(
                        "[gcard-plan] query={}, selected_index={:?}, selected_score={:?}, estimate={}",
                        query_name, index_of_min_es, score_of_min_es, cardinality_value
                    );
                    if let Some(abs) = &min_es_abstract_graph {
                        eprint!("{}", abs.describe_plan());
                    }
                    if let Some(counts) = min_es_alpha_beta_counts {
                        eprintln!(
                            "[gcard-alpha-beta] best_plan raw_alpha={}, raw_alpha_refs={}, raw_alpha_total={}, raw_beta_left={}, raw_beta_right={}, raw_beta={}, raw_beta_total={}, effective_alpha_total={}, effective_beta_total={}",
                            counts.alpha,
                            counts.alpha_refs,
                            counts.total_alpha(),
                            counts.beta_left,
                            counts.beta_right,
                            counts.beta,
                            counts.total_beta(),
                            counts.total_effective_alpha(),
                            counts.total_effective_beta()
                        );
                    }
                    if let (Some(abs), Some(log_path)) =
                        (&min_es_abstract_graph, plan_trace_log_path.clone())
                    {
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&log_path)
                        {
                            Ok(mut file) => {
                                use std::io::Write;
                                let _ = writeln!(
                                    file,
                                    "==== query={} selected_index={:?} selected_score={:?} estimate={} ====",
                                    query_name, index_of_min_es, score_of_min_es, cardinality_value
                                );
                                let _ = writeln!(file, "{}", abs.describe_reduction_trace());
                                if let Some(counts) = min_es_alpha_beta_counts {
                                    let _ = writeln!(
                                        file,
                                        "counts raw_alpha_total={} raw_beta_total={} effective_alpha_total={} effective_beta_total={}\n",
                                        counts.total_alpha(),
                                        counts.total_beta(),
                                        counts.total_effective_alpha(),
                                        counts.total_effective_beta()
                                    );
                                }
                            }
                            Err(err) => {
                                eprintln!(
                                    "[gcard-plan-trace] failed to open {}: {}",
                                    Path::new(&log_path).display(),
                                    err
                                );
                            }
                        }
                    }
                }
                let estimate_elapsed = estimate_start.elapsed();
                // File loading and query parsing happen before this interval.
                inference_nanos = build_start.elapsed().as_nanos();
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
                let cache_hits = FILTER_POOL_CACHE_HITS.load(Ordering::Relaxed);
                let cache_misses = FILTER_POOL_CACHE_MISSES.load(Ordering::Relaxed);
                let pool_empty = FILTER_POOL_EMPTY.load(Ordering::Relaxed);
                let stage2_count = STAGE2_TRIGGERED.load(Ordering::Relaxed);
                let stage2_zero = STAGE2_RESULT_ZERO.load(Ordering::Relaxed);
                let sel_zero = SELECTIVITY_ZERO.load(Ordering::Relaxed);
                let sel_fallback = SELECTIVITY_FALLBACK.load(Ordering::Relaxed);
                let guard_skipped = WALK_GUARD_SKIPPED.load(Ordering::Relaxed);
                eprintln!(
                    "[cache-prof] filter_pool_hits={}, filter_pool_misses={}, filter_pool_empty={}, stage2_triggered={}, stage2_result_zero={}, selectivity_zero={}, selectivity_fallback={}, walk_guard_skipped={}",
                    cache_hits,
                    cache_misses,
                    pool_empty,
                    stage2_count,
                    stage2_zero,
                    sel_zero,
                    sel_fallback,
                    guard_skipped,
                );
                cardinality_value
            }
            Err(e) => {
                inference_nanos = build_start.elapsed().as_nanos();
                eprintln!(
                    "GCard: unsupported predicate or error, returning 0.0: {}",
                    e
                );
                0.0
            }
        };
        let query_name = Path::new(query_json_path.as_str())
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        eprintln!(
            "[gcard-inference] query={}, nanos={}",
            query_name, inference_nanos
        );

        #[cfg(feature = "profiling")]
        {
            match guard.report().build() {
                Ok(report) => {
                    let dir =
                        std::env::var("GCARD_FLAMEGRAPH_DIR").unwrap_or_else(|_| ".".to_string());
                    if let Err(err) = std::fs::create_dir_all(&dir) {
                        eprintln!("[profiling] failed to create {}: {}", dir, err);
                    }
                    let stem = Path::new(query_json_path.as_str())
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let svg_path = format!(
                        "{}/gcard_flamegraph_{}_{}_{}.svg",
                        dir.trim_end_matches('/'),
                        stem,
                        ts,
                        std::process::id()
                    );
                    match std::fs::File::create(&svg_path) {
                        Ok(file) => match report.flamegraph(file) {
                            Ok(_) => {
                                eprintln!("[profiling] flamegraph saved: {}", svg_path)
                            }
                            Err(err) => {
                                eprintln!("[profiling] flamegraph write failed: {}", err)
                            }
                        },
                        Err(err) => {
                            eprintln!("[profiling] cannot create {}: {}", svg_path, err)
                        }
                    }
                }
                Err(err) => eprintln!("[profiling] report build failed: {}", err),
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

pub fn build_set_star_config_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::Int32, // max_star_length
        LogicalType::Int32, // max_star_degree; 0 disables star use at query time
    ];

    Procedure::new(parameters, None, move |_context, args| {
        let max_star_length = args
            .first()
            .and_then(|a| a.to_i32().ok())
            .map(|n| n.max(0) as usize)
            .ok_or_else(|| anyhow::anyhow!("max_star_length must be an int"))?;
        let max_star_degree = args
            .get(1)
            .and_then(|a| a.to_i32().ok())
            .map(|n| n.max(0) as usize)
            .ok_or_else(|| anyhow::anyhow!("max_star_degree must be an int"))?;

        GCARD_MAX_STAR_LENGTH_OVERRIDE.store(max_star_length, Ordering::Relaxed);
        GCARD_MAX_STAR_DEGREE_OVERRIDE.store(max_star_degree, Ordering::Relaxed);
        eprintln!(
            "[gcard] query star config set: max_star_length={}, max_star_degree={}",
            max_star_length, max_star_degree
        );
        Ok(vec![])
    })
}
