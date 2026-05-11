//! `random_insert` procedure — randomly insert a fraction of edges and vertices,
//! update GCard statistics, and optionally export the updated FlatGraph bincode.
//!
//! Usage:
//!   `call random_insert(<ratio>, '<flatgraph_bincode_path>', <seed>)`
//!   `call random_insert(<ratio>, '<flatgraph_bincode_path>', <seed>, '<vertex_label>',
//! '<edge_label>')`   `call random_insert(<ratio>, '<flatgraph_bincode_path>', <seed>, '',
//! '<edge_label>')`
//!
//! - `ratio` (Float64): fraction to insert relative to current size, e.g. 0.01 means add 1% more
//!   edges + 1% more vertices
//! - `flatgraph_bincode_path` (String): optional path to export the updated FlatGraph bincode; pass
//!   `""` to skip export
//! - `seed` (Int64): random seed for reproducibility
//! - `vertex_label` (String, optional): if non-empty, only insert vertices of this label; pass `""`
//!   to skip vertex insertion when edge_label is specified
//! - `edge_label` (String, optional): if non-empty, only insert edges of this label
//!
//! Modes:
//! - Both empty or omitted: insert across all vertex and edge types (default)
//! - Only edge_label: insert edges of that type between existing vertices (no new vertices)
//! - Both vertex_label and edge_label: insert vertices of that type + edges of that type
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
use super::update_log::GCardUpdateLog;

// ── Core insert logic ──────────────────────────────────────────────────────

fn do_random_insert(
    container: &GraphContainer,
    ratio: f64,
    export_bincode_path: &str,
    seed: u64,
    target_vertex_label: Option<&str>,
    target_edge_label: Option<&str>,
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

    // ── Determine which vertex/edge labels to operate on ─────────────────
    let all_labels: Vec<String> = fg.all_labels().map(String::from).collect();
    let edge_schema = fg.edge_type_schema().clone();

    // Vertex labels to insert: if target_vertex_label is set, only that one;
    // if only edge_label is set (no vertex_label), skip vertex insertion;
    // if neither is set, insert across all labels.
    let vertex_labels_to_insert: Vec<String> = match (target_vertex_label, target_edge_label) {
        (Some(vl), _) => {
            if fg.all_vertex_ids_by_label(vl).is_empty() && !all_labels.contains(&vl.to_string()) {
                return Err(anyhow::anyhow!("vertex label '{}' not found in graph", vl));
            }
            vec![vl.to_string()]
        }
        (None, Some(_)) => vec![], // edge-only mode: no vertex insertion
        (None, None) => all_labels.clone(),
    };

    // Edge labels to insert: if target_edge_label is set, only that one;
    // otherwise all edge types.
    let edge_labels_to_insert: Vec<(String, String, String)> = match target_edge_label {
        Some(el) => {
            let (src_l, dst_l) = edge_schema
                .get(el)
                .ok_or_else(|| anyhow::anyhow!("edge label '{}' not found in graph", el))?;
            vec![(el.to_string(), src_l.clone(), dst_l.clone())]
        }
        None => edge_schema
            .iter()
            .map(|(el, (sl, dl))| (el.clone(), sl.clone(), dl.clone()))
            .collect(),
    };

    eprintln!(
        "[random_insert] mode: vertex_labels={}, edge_labels={}",
        if vertex_labels_to_insert.is_empty() {
            "(none)".to_string()
        } else {
            vertex_labels_to_insert.join(",")
        },
        edge_labels_to_insert
            .iter()
            .map(|(el, _, _)| el.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );

    // ── Phase 1: randomly insert vertices ────────────────────────────────
    let t_phase = std::time::Instant::now();
    let mut inserted_vertex_count: i64 = 0;

    for label in &vertex_labels_to_insert {
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
            fg.observe_vertex_stats(label, &empty_props);
            fg.record_insert_vertex(vid, label, empty_props.clone());
            inserted_vertex_count += 1;
        }
    }

    eprintln!(
        "[random_insert] phase1 insert_vertices: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 2: insert edges + append log ────────────────────────────────
    let t_phase = std::time::Instant::now();
    let mut inserted_edge_count: i64 = 0;

    // Snapshot current vertex IDs per label (including just-inserted vertices).
    let mut vids_by_label: std::collections::HashMap<String, Vec<VertexId>> =
        std::collections::HashMap::new();
    for label in &all_labels {
        let mut vids: Vec<VertexId> = fg.all_vertex_ids_by_label(label).to_vec();
        for (vid, vlabel, _) in &fg.pending_ref().inserted_vertices {
            if vlabel == label {
                vids.push(*vid);
            }
        }
        vids_by_label.insert(label.clone(), vids);
    }

    // Count existing edges per target label — O(1) per label via CSR metadata.
    let mut edge_count_by_label: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (edge_label, _, _) in &edge_labels_to_insert {
        edge_count_by_label.insert(edge_label.clone(), fg.edge_count_by_label(edge_label));
    }

    let mut log_guard = log_arc
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;

    for (edge_label, src_label, dst_label) in &edge_labels_to_insert {
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

            fg.observe_edge_stats(edge_label, &empty_props);
            fg.record_insert_edge(
                eid,
                src,
                src_label,
                dst,
                dst_label,
                edge_label,
                empty_props.clone(),
            );
            log_guard.record_insert_edge(src, src_label, dst, dst_label, edge_label);
            inserted_edge_count += 1;
        }
    }
    drop(log_guard);

    eprintln!(
        "[random_insert] phase2 insert_edges_and_append_log: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 3: compact update log into Statistic ───────────────────────
    let t_phase = std::time::Instant::now();
    let stat_arc = container
        .take_statistic()
        .ok_or_else(|| anyhow::anyhow!("statistic not set"))?;
    let stat_arc = Arc::downcast::<super::Statistic>(stat_arc)
        .map_err(|_| anyhow::anyhow!("statistic type mismatch"))?;
    let mut statistic = Arc::try_unwrap(stat_arc).unwrap_or_else(|arc| (*arc).clone());
    let dirty_keys = {
        let mut guard = log_arc
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        guard.compact_and_apply_flat(&fg, &mut statistic)?
    };
    eprintln!(
        "[random_insert] phase3 compact_log: {:.2}s ({} dirty keys)",
        t_phase.elapsed().as_secs_f64(),
        dirty_keys.len()
    );

    // ── Phase 4: apply pending + rebuild DegreeSeqGraphCompressed ────────
    let t_phase = std::time::Instant::now();
    fg.apply_pending();
    eprintln!(
        "[random_insert] phase4a apply_pending: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    let t_phase_b = std::time::Instant::now();
    let dsgc_arc = container
        .take_degree_seq_graph_compressed()
        .ok_or_else(|| anyhow::anyhow!("degree_seq_graph_compressed not set"))?;
    let dsgc_arc = Arc::downcast::<super::catalog::DegreeSeqGraphCompressed>(dsgc_arc)
        .map_err(|_| anyhow::anyhow!("dsgc type mismatch"))?;
    let mut new_dsgc = Arc::try_unwrap(dsgc_arc).unwrap_or_else(|arc| (*arc).clone());
    new_dsgc.update_dirty(&statistic, &dirty_keys);

    if !export_bincode_path.is_empty() && export_bincode_path != "unused" {
        fg.export_bincode(export_bincode_path)?;
        eprintln!(
            "[random_insert] exported FlatGraph bincode to {}",
            export_bincode_path
        );
    }

    container.set_statistic(Arc::new(statistic));
    container.set_degree_seq_graph_compressed(Arc::new(new_dsgc));
    container.set_gcard_flat_graph(Arc::new(fg));
    eprintln!(
        "[random_insert] phase4b rebuild_dsgc: {:.2}s ({} dirty keys)",
        t_phase_b.elapsed().as_secs_f64(),
        dirty_keys.len()
    );
    eprintln!(
        "[random_insert] phase4 total: {:.2}s ({} dirty keys)",
        t_phase.elapsed().as_secs_f64(),
        dirty_keys.len()
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
        LogicalType::String,  // flatgraph_bincode_path
        LogicalType::Int64,   // seed
        LogicalType::String,  // vertex_label (optional, "" to skip)
        LogicalType::String,  // edge_label (optional, "" to skip)
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
        let export_bincode_path = args[1]
            .try_as_string()
            .expect("second arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("flatgraph_bincode_path cannot be null"))?
            .to_string();
        let seed = args[2]
            .to_i64()
            .map_err(|_| anyhow::anyhow!("seed must be an integer"))? as u64;

        // Optional 4th arg: vertex_label (empty string = not specified)
        let target_vertex_label: Option<String> = args
            .get(3)
            .and_then(|a| a.try_as_string())
            .and_then(|opt| opt.clone())
            .filter(|s| !s.is_empty());

        // Optional 5th arg: edge_label (empty string = not specified)
        let target_edge_label: Option<String> = args
            .get(4)
            .and_then(|a| a.try_as_string())
            .and_then(|opt| opt.clone())
            .filter(|s| !s.is_empty());

        let (ins_edges, ins_verts) = do_random_insert(
            container,
            ratio,
            &export_bincode_path,
            seed,
            target_vertex_label.as_deref(),
            target_edge_label.as_deref(),
        )?;

        let chunk = DataChunk::new(vec![
            Arc::new(Int64Array::from_iter_values([ins_edges])),
            Arc::new(Int64Array::from_iter_values([ins_verts])),
        ]);
        Ok(vec![chunk])
    })
}
