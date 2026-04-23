//! `random_insert` procedure — randomly insert a fraction of edges and vertices,
//! update GCard statistics, and export a CSV snapshot for ground-truth evaluation.
//!
//! Usage:
//!   `call random_insert(<ratio>, '<output_dir>', <seed>)`
//!
//! - `ratio` (Float64): fraction to insert relative to current size, e.g. 0.01 means add 1% more
//!   edges + 1% more vertices
//! - `output_dir` (String): directory to write CSV snapshot (created if not exists)
//! - `seed` (Int64): random seed for reproducibility
//!
//! Returns `(inserted_edges: Int64, inserted_vertices: Int64)`.

use std::io;
use std::sync::Arc;

use arrow::array::Int64Array;
use minigu_common::data_chunk::DataChunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use super::flat_graph::FlatGraph;
use super::random_update::export_csv_snapshot;
use super::update_log::GCardUpdateLog;

// ── Core insert logic ──────────────────────────────────────────────────────

fn do_random_insert(
    container: &GraphContainer,
    ratio: f64,
    output_dir: &str,
    seed: u64,
) -> anyhow::Result<(i64, i64)> {
    let mut rng = StdRng::seed_from_u64(seed);

    // ── Get FlatGraph (take ownership to avoid clone) ──────────────────
    let fg_arc = container
        .take_gcard_flat_graph()
        .and_then(|arc| Arc::downcast::<FlatGraph>(arc).ok())
        .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded (run load_ldbc first)"))?;

    let mut fg = Arc::try_unwrap(fg_arc).unwrap_or_else(|arc| {
        eprintln!("[random_insert] WARN: FlatGraph clone fallback");
        (*arc).clone()
    });

    // ── Get UpdateLog ────────────────────────────────────────────────────
    let log_arc = container
        .gcard_update_log()
        .ok_or_else(|| anyhow::anyhow!("gcard_update_log not set (run load_ldbc first)"))?;
    let log_arc = log_arc
        .downcast::<std::sync::Mutex<GCardUpdateLog>>()
        .map_err(|_| anyhow::anyhow!("gcard_update_log type mismatch"))?;

    // ── Compute next available IDs ───────────────────────────────────────
    let mut next_vid: VertexId = fg
        .all_labels()
        .flat_map(|l| fg.all_vertex_ids_by_label(l).iter().copied())
        .max()
        .unwrap_or(0)
        + 1;

    let mut next_eid: EdgeId = fg.all_edge_ids().max().unwrap_or(0) + 1;

    let t_total = std::time::Instant::now();

    // ── Phase 1: randomly insert vertices ────────────────────────────────
    let t_phase = std::time::Instant::now();
    // For each label, insert ratio * count new vertices.
    let all_labels: Vec<String> = fg.all_labels().map(String::from).collect();
    let mut inserted_vertex_count: i64 = 0;

    for label in &all_labels {
        let current_count = fg.all_vertex_ids_by_label(label).len();
        let n_insert = (current_count as f64 * ratio).round() as usize;

        let prop_names = fg
            .vertex_prop_schema()
            .get(label.as_str())
            .cloned()
            .unwrap_or_default();
        let empty_props: Vec<ScalarValue> = prop_names.iter().map(|_| ScalarValue::Null).collect();

        for _ in 0..n_insert {
            let vid = next_vid;
            next_vid += 1;
            fg.record_insert_vertex(vid, label, empty_props.clone());
            // New vertex with no edges — no UpdateLog entry needed (zero degree).
            inserted_vertex_count += 1;
        }
    }

    eprintln!(
        "[random_insert] phase1 insert_vertices: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 2: randomly insert edges ───────────────────────────────────
    let t_phase = std::time::Instant::now();
    // For each edge type, insert ratio * count new edges between random
    // endpoints of the matching vertex labels.
    let edge_schema = fg.edge_type_schema().clone();
    let mut inserted_edge_count: i64 = 0;

    // Snapshot current vertex IDs per label (including just-inserted vertices
    // that are in pending — we need to collect from both base and pending).
    let mut vids_by_label: std::collections::HashMap<String, Vec<VertexId>> =
        std::collections::HashMap::new();
    for label in &all_labels {
        let mut vids: Vec<VertexId> = fg.all_vertex_ids_by_label(label).to_vec();
        // Also include pending inserted vertices for this label.
        for (vid, vlabel, _) in &fg.pending_ref().inserted_vertices {
            if vlabel == label {
                vids.push(*vid);
            }
        }
        vids_by_label.insert(label.clone(), vids);
    }

    // Count existing edges per label.
    let mut edge_count_by_label: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for eid in fg.all_edge_ids() {
        if let Some((_, _, _, _, elabel)) = fg.edge_endpoints(eid) {
            *edge_count_by_label.entry(elabel.to_string()).or_insert(0) += 1;
        }
    }

    for (edge_label, (src_label, dst_label)) in &edge_schema {
        let current_count = edge_count_by_label.get(edge_label).copied().unwrap_or(0);
        let n_insert = (current_count as f64 * ratio).round() as usize;

        let src_vids = match vids_by_label.get(src_label) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };
        let dst_vids = match vids_by_label.get(dst_label) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let prop_names = fg
            .edge_prop_schema()
            .get(edge_label.as_str())
            .cloned()
            .unwrap_or_default();
        let empty_props: Vec<ScalarValue> = prop_names.iter().map(|_| ScalarValue::Null).collect();

        for _ in 0..n_insert {
            let src = src_vids[rng.gen_range(0..src_vids.len())];
            let dst = dst_vids[rng.gen_range(0..dst_vids.len())];
            let eid = next_eid;
            next_eid += 1;

            fg.record_insert_edge(
                eid,
                src,
                src_label,
                dst,
                dst_label,
                edge_label,
                empty_props.clone(),
            );
            inserted_edge_count += 1;
        }
    }

    eprintln!(
        "[random_insert] phase2 insert_edges: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 3: apply pending (rebuild CSR) ───────────────────────────
    let t_phase = std::time::Instant::now();
    fg.apply_pending();
    eprintln!(
        "[random_insert] phase3 apply_pending: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 4: full recompute statistic from FlatGraph CSR ─────────────
    let t_phase = std::time::Instant::now();
    let graph_type = container.graph_type();
    let max_k = log_arc
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
        .max_k;
    let statistic =
        super::load_ldbc::rebuild_statistic_from_flat_graph(&fg, graph_type.as_ref(), max_k)?;
    let new_dsgc = statistic
        .to_degree_seq_graph_compressed()
        .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {e}"))?;
    container.set_statistic(Arc::new(statistic));
    container.set_degree_seq_graph_compressed(Arc::new(new_dsgc));
    container.set_gcard_flat_graph(Arc::new(fg));
    eprintln!(
        "[random_insert] phase4 rebuild_statistic: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    eprintln!(
        "[random_insert] total: {:.2}s",
        t_total.elapsed().as_secs_f64()
    );
    eprintln!(
        "[random_insert] inserted {} edges, {} vertices",
        inserted_edge_count, inserted_vertex_count
    );

    Ok((inserted_edge_count, inserted_vertex_count))
}

// ── Procedure registration ──────────────────────────────────────────────────

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::Float64, // ratio
        LogicalType::String,  // output_dir
        LogicalType::Int64,   // seed
    ];

    let schema = Arc::new(DataSchema::new(vec![
        DataField::new("inserted_edges".into(), LogicalType::Int64, false),
        DataField::new("inserted_vertices".into(), LogicalType::Int64, false),
    ]));

    Procedure::new(parameters, Some(schema), move |context, args| {
        let graph_ref = context.current_graph.clone().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "no current graph selected",
            )))
        })?;
        let container = graph_ref
            .object()
            .downcast_ref::<GraphContainer>()
            .ok_or_else(|| {
                ExecutionError::Custom(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "current graph is not a GraphContainer",
                )))
            })?;

        let ratio = args[0]
            .to_f64()
            .map_err(|_| anyhow::anyhow!("ratio must be a float"))?;
        let output_dir = args[1]
            .try_as_string()
            .expect("second arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("output_dir cannot be null"))?
            .to_string();
        let seed = args[2]
            .to_i64()
            .map_err(|_| anyhow::anyhow!("seed must be an integer"))? as u64;

        let (ins_edges, ins_verts) = do_random_insert(container, ratio, &output_dir, seed)?;

        let chunk = DataChunk::new(vec![
            Arc::new(Int64Array::from_iter_values([ins_edges])),
            Arc::new(Int64Array::from_iter_values([ins_verts])),
        ]);
        Ok(vec![chunk])
    })
}
