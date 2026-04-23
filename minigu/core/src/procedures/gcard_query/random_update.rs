//! `random_update` procedure — randomly delete a fraction of edges and vertices,
//! update GCard statistics, and export a CSV snapshot for ground-truth evaluation.
//!
//! Usage:
//!   `call random_update(<ratio>, '<output_dir>', <seed>)`
//!
//! - `ratio` (Float64): fraction to delete, e.g. 0.05 means 5% of edges + 5% of vertices
//! - `output_dir` (String): directory to write CSV snapshot (created if not exists)
//! - `seed` (Int64): random seed for reproducibility
//!
//! Returns `(deleted_edges: Int64, deleted_vertices: Int64)`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use arrow::array::Int64Array;
use minigu_common::data_chunk::DataChunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_common::types::{EdgeId, VertexId};
use minigu_common::value::ScalarValue;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rayon::prelude::*;

use super::flat_graph::FlatGraph;
use super::update_log::GCardUpdateLog;

// ── CSV export helpers ──────────────────────────────────────────────────────

fn scalar_to_csv(v: &ScalarValue) -> String {
    match v {
        ScalarValue::Null => String::new(),
        ScalarValue::Boolean(Some(b)) => b.to_string(),
        ScalarValue::Int8(Some(n)) => n.to_string(),
        ScalarValue::Int16(Some(n)) => n.to_string(),
        ScalarValue::Int32(Some(n)) => n.to_string(),
        ScalarValue::Int64(Some(n)) => n.to_string(),
        ScalarValue::UInt8(Some(n)) => n.to_string(),
        ScalarValue::UInt16(Some(n)) => n.to_string(),
        ScalarValue::UInt32(Some(n)) => n.to_string(),
        ScalarValue::UInt64(Some(n)) => n.to_string(),
        ScalarValue::Float32(Some(f)) => f.to_string(),
        ScalarValue::Float64(Some(f)) => f.to_string(),
        ScalarValue::String(Some(s)) => {
            // Escape CSV: if contains comma/quote/newline, quote it
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.clone()
            }
        }
        _ => String::new(),
    }
}

pub(crate) fn export_csv_snapshot(fg: &FlatGraph, output_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(output_dir)?;

    // Collect all vertex-label tasks.
    let vertex_labels: Vec<String> = fg
        .all_labels()
        .filter(|l| !fg.all_vertex_ids_by_label(l).is_empty())
        .map(String::from)
        .collect();

    // Group edges by label (single pass).
    let mut edges_by_label: std::collections::HashMap<String, Vec<EdgeId>> =
        std::collections::HashMap::new();
    for eid in fg.all_edge_ids() {
        if let Some((_, _, _, _, edge_label)) = fg.edge_endpoints(eid) {
            edges_by_label
                .entry(edge_label.to_string())
                .or_default()
                .push(eid);
        }
    }

    // Build a unified task list: each task writes one CSV file.
    enum ExportTask {
        Vertex(String),
        Edge(String, Vec<EdgeId>),
    }

    let mut tasks: Vec<ExportTask> = vertex_labels.into_iter().map(ExportTask::Vertex).collect();
    for (edge_label, mut eids) in edges_by_label {
        eids.sort_unstable();
        tasks.push(ExportTask::Edge(edge_label, eids));
    }

    // Execute all CSV writes in parallel.
    let errors: Vec<anyhow::Error> = tasks
        .into_par_iter()
        .filter_map(|task| {
            let result = match task {
                ExportTask::Vertex(label) => write_vertex_csv(fg, output_dir, &label),
                ExportTask::Edge(edge_label, eids) => {
                    write_edge_csv(fg, output_dir, &edge_label, &eids)
                }
            };
            result.err()
        })
        .collect();

    if let Some(first_err) = errors.into_iter().next() {
        return Err(first_err);
    }

    Ok(())
}

fn write_vertex_csv(fg: &FlatGraph, output_dir: &Path, label: &str) -> anyhow::Result<()> {
    let vids = fg.all_vertex_ids_by_label(label);
    let prop_names = fg
        .vertex_prop_schema()
        .get(label)
        .cloned()
        .unwrap_or_default();

    let csv_path = output_dir.join(format!("{}.csv", label));
    let mut file = io::BufWriter::new(fs::File::create(&csv_path)?);

    // Header
    let mut header = "id".to_string();
    for name in &prop_names {
        header.push(',');
        header.push_str(name);
    }
    writeln!(file, "{}", header)?;

    // Rows
    for &vid in vids {
        let mut line = vid.to_string();
        if let Some(props) = fg.vertex_props(vid) {
            for (i, _) in prop_names.iter().enumerate() {
                line.push(',');
                if i < props.len() {
                    line.push_str(&scalar_to_csv(&props[i]));
                }
            }
        } else {
            for _ in &prop_names {
                line.push(',');
            }
        }
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

fn write_edge_csv(
    fg: &FlatGraph,
    output_dir: &Path,
    edge_label: &str,
    eids: &[EdgeId],
) -> anyhow::Result<()> {
    let prop_names = fg
        .edge_prop_schema()
        .get(edge_label)
        .cloned()
        .unwrap_or_default();

    let csv_path = output_dir.join(format!("{}.csv", edge_label));
    let mut file = io::BufWriter::new(fs::File::create(&csv_path)?);

    // Header
    let mut header = "src,dst".to_string();
    for name in &prop_names {
        header.push(',');
        header.push_str(name);
    }
    writeln!(file, "{}", header)?;

    // Rows
    for eid in eids {
        if let Some((src, _, dst, _, _)) = fg.edge_endpoints(*eid) {
            let mut line = format!("{},{}", src, dst);
            if let Some(props) = fg.edge_props(*eid) {
                for (i, _) in prop_names.iter().enumerate() {
                    line.push(',');
                    if i < props.len() {
                        line.push_str(&scalar_to_csv(&props[i]));
                    }
                }
            } else {
                for _ in &prop_names {
                    line.push(',');
                }
            }
            writeln!(file, "{}", line)?;
        }
    }
    Ok(())
}

// ── Core update logic ───────────────────────────────────────────────────────

fn do_random_update(
    container: &GraphContainer,
    ratio: f64,
    output_dir: &str,
    seed: u64,
) -> anyhow::Result<(i64, i64)> {
    let mut rng = StdRng::seed_from_u64(seed);

    // ── Get FlatGraph (take ownership to avoid 30GB clone) ─────────────
    let fg_arc = container
        .take_gcard_flat_graph()
        .and_then(|arc| Arc::downcast::<FlatGraph>(arc).ok())
        .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded (run load_ldbc first)"))?;

    let mut fg = Arc::try_unwrap(fg_arc).unwrap_or_else(|arc| {
        eprintln!("[random_update] WARN: FlatGraph clone fallback");
        (*arc).clone()
    });

    // ── Get UpdateLog ────────────────────────────────────────────────────
    let log_arc = container
        .gcard_update_log()
        .ok_or_else(|| anyhow::anyhow!("gcard_update_log not set (run GCard_build first)"))?;
    let log_arc = log_arc
        .downcast::<std::sync::Mutex<GCardUpdateLog>>()
        .map_err(|_| anyhow::anyhow!("gcard_update_log type mismatch"))?;

    let t_total = std::time::Instant::now();

    // ── Phase 1: randomly delete edges ───────────────────────────────────
    let t_phase = std::time::Instant::now();
    let all_eids: Vec<EdgeId> = fg.all_edge_ids().collect();
    let n_del_edges = ((all_eids.len() as f64 * ratio).round() as usize).min(all_eids.len());

    let mut eids_shuffled = all_eids;
    eids_shuffled.shuffle(&mut rng);
    let edges_to_delete: Vec<EdgeId> = eids_shuffled.into_iter().take(n_del_edges).collect();

    // Collect deleted edge info BEFORE marking (need endpoints for delta computation).
    let mut deleted_edge_infos: Vec<(VertexId, String, VertexId, String, String)> = Vec::new();
    let mut deleted_edge_count: i64 = 0;
    for &eid in &edges_to_delete {
        if let Some((src, src_label, dst, dst_label, edge_label)) = fg.edge_endpoints(eid) {
            deleted_edge_infos.push((
                src,
                src_label.to_string(),
                dst,
                dst_label.to_string(),
                edge_label.to_string(),
            ));
            fg.record_delete_edge(eid);
            deleted_edge_count += 1;
        }
    }

    eprintln!(
        "[random_update] phase1 delete_edges: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 2: randomly delete vertices (+ cascade edges) ──────────────
    let t_phase = std::time::Instant::now();
    let all_labels: Vec<String> = fg.all_labels().map(String::from).collect();
    let mut all_vids: Vec<VertexId> = Vec::new();
    for label in &all_labels {
        all_vids.extend_from_slice(fg.all_vertex_ids_by_label(label));
    }
    let n_del_verts = ((all_vids.len() as f64 * ratio).round() as usize).min(all_vids.len());
    all_vids.shuffle(&mut rng);
    let verts_to_delete: Vec<VertexId> = all_vids.into_iter().take(n_del_verts).collect();
    let verts_to_delete_set: HashSet<VertexId> = verts_to_delete.iter().copied().collect();

    let mut deleted_vertex_count: i64 = 0;
    for &vid in &verts_to_delete {
        if fg.vertex_label(vid).is_none() {
            continue;
        }
        let incidents = fg.incident_edges(vid);
        for (eid, nbr, nbr_label, edge_label) in &incidents {
            if let Some(v_label) = fg.vertex_label(vid) {
                deleted_edge_infos.push((
                    vid,
                    v_label.to_string(),
                    *nbr,
                    nbr_label.clone(),
                    edge_label.clone(),
                ));
            }
            fg.record_delete_edge(*eid);
            deleted_edge_count += 1;
        }
        fg.record_delete_vertex(vid);
        deleted_vertex_count += 1;
    }

    eprintln!(
        "[random_update] phase2 delete_vertices: {:.2}s ({} edge infos collected)",
        t_phase.elapsed().as_secs_f64(),
        deleted_edge_infos.len()
    );

    // ── Phase 3: apply pending (rebuild CSR) ───────────────────────────
    let t_phase = std::time::Instant::now();
    fg.apply_pending();
    eprintln!(
        "[random_update] phase3 apply_pending: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    // ── Phase 4: direct delta computation ────────────────────────────────
    // No HashMap, no UpdateLog. For each deleted edge, directly compute
    // which path patterns are affected and apply deltas to Statistic.
    let t_phase = std::time::Instant::now();

    let stat_arc = container
        .take_statistic()
        .ok_or_else(|| anyhow::anyhow!("statistic not set"))?;
    let stat_arc = Arc::downcast::<super::Statistic>(stat_arc)
        .map_err(|_| anyhow::anyhow!("statistic type mismatch"))?;
    let mut statistic = Arc::try_unwrap(stat_arc).unwrap_or_else(|arc| (*arc).clone());

    let max_k = log_arc
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
        .max_k;

    // Build extension table: label → [(edge_label, nbr_label, outgoing)]
    let label_to_exts: std::collections::HashMap<String, Vec<(String, String, bool)>> = fg
        .all_labels()
        .filter_map(|label| {
            let exts = fg.edge_extensions_for_label(label);
            if exts.is_empty() {
                None
            } else {
                Some((label.to_string(), exts))
            }
        })
        .collect();

    // Pre-build AltKey templates grouped by edge type.
    // For each (src_label, edge_label, dst_label) edge type and each extension,
    // build the AltKey once and reuse for all deleted edges of that type.
    use super::catalog::AltKey;

    struct DeltaTemplate {
        altkey: AltKey,
        target_label: String,
        csr_key: (String, String, bool),
        side: bool, // true = src side, false = dst side
    }

    let mut templates_by_edge_type: std::collections::HashMap<
        (String, String, String), // (src_label, edge_label, dst_label)
        Vec<DeltaTemplate>,
    > = std::collections::HashMap::new();

    // Collect unique edge types from deleted edges.
    let mut edge_types: HashSet<(String, String, String)> = HashSet::new();
    for (src, src_label, dst, dst_label, edge_label) in &deleted_edge_infos {
        edge_types.insert((src_label.clone(), edge_label.clone(), dst_label.clone()));
    }

    for (src_label, edge_label, dst_label) in &edge_types {
        let mut templates = Vec::new();

        // Templates from src side: expand src's neighbors via extensions
        if let Some(exts) = label_to_exts.get(src_label.as_str()) {
            for (ext_edge, ext_nbr, outgoing) in exts {
                templates.push(DeltaTemplate {
                    altkey: AltKey::new(vec![
                        ext_nbr.clone(),
                        ext_edge.clone(),
                        src_label.clone(),
                        edge_label.clone(),
                        dst_label.clone(),
                    ]),
                    target_label: ext_nbr.clone(),
                    csr_key: (src_label.clone(), ext_edge.clone(), *outgoing),
                    side: true,
                });
            }
        }

        // Templates from dst side: expand dst's neighbors via extensions
        if let Some(exts) = label_to_exts.get(dst_label.as_str()) {
            for (ext_edge, ext_nbr, outgoing) in exts {
                templates.push(DeltaTemplate {
                    altkey: AltKey::new(vec![
                        ext_nbr.clone(),
                        ext_edge.clone(),
                        dst_label.clone(),
                        edge_label.clone(),
                        src_label.clone(),
                    ]),
                    target_label: ext_nbr.clone(),
                    csr_key: (dst_label.clone(), ext_edge.clone(), *outgoing),
                    side: false,
                });
            }
        }

        templates_by_edge_type.insert(
            (src_label.clone(), edge_label.clone(), dst_label.clone()),
            templates,
        );
    }

    let hop_csrs = fg.hop_csrs();
    let mut delta_count: u64 = 0;

    for (src, src_label, dst, dst_label, edge_label) in &deleted_edge_infos {
        let templates = match templates_by_edge_type.get(&(
            src_label.clone(),
            edge_label.clone(),
            dst_label.clone(),
        )) {
            Some(t) => t,
            None => continue,
        };

        for tmpl in templates {
            let vid = if tmpl.side { *src } else { *dst };
            if let Some(csr) = hop_csrs.get(&tmpl.csr_key) {
                for &(nbr, _) in csr.neighbors_slice(vid) {
                    if !verts_to_delete_set.contains(&nbr) {
                        statistic.apply_delta(&tmpl.target_label, &tmpl.altkey, nbr, -1);
                        delta_count += 1;
                    }
                }
            }
        }
    }

    // Delete vertices from statistic.
    for &vid in &verts_to_delete {
        statistic.delete_vertex(vid);
    }

    eprintln!(
        "[random_update] phase4 direct_delta: {:.2}s ({} deltas applied)",
        t_phase.elapsed().as_secs_f64(),
        delta_count
    );

    // ── Phase 5: incremental rebuild DegreeSeqGraphCompressed ────────────
    let t_phase = std::time::Instant::now();
    // Collect dirty keys from the edge types and their templates.
    let mut dirty_keys: HashSet<(AltKey, String)> = HashSet::new();
    for templates in templates_by_edge_type.values() {
        for tmpl in templates {
            dirty_keys.insert((tmpl.altkey.clone(), tmpl.target_label.clone()));
        }
    }
    let dsgc_arc = container
        .take_degree_seq_graph_compressed()
        .ok_or_else(|| anyhow::anyhow!("degree_seq_graph_compressed not set"))?;
    let dsgc_arc = Arc::downcast::<super::catalog::DegreeSeqGraphCompressed>(dsgc_arc)
        .map_err(|_| anyhow::anyhow!("dsgc type mismatch"))?;
    let mut new_dsgc = Arc::try_unwrap(dsgc_arc).unwrap_or_else(|arc| (*arc).clone());
    new_dsgc.update_dirty(&statistic, &dirty_keys);
    container.set_statistic(Arc::new(statistic));
    container.set_degree_seq_graph_compressed(Arc::new(new_dsgc));
    container.set_gcard_flat_graph(Arc::new(fg));
    eprintln!(
        "[random_update] phase5 rebuild_dsgc: {:.2}s ({} dirty keys)",
        t_phase.elapsed().as_secs_f64(),
        dirty_keys.len()
    );
    eprintln!(
        "[random_update] phase4 rebuild_statistic: {:.2}s",
        t_phase.elapsed().as_secs_f64()
    );

    eprintln!(
        "[random_update] total: {:.2}s",
        t_total.elapsed().as_secs_f64()
    );
    eprintln!(
        "[random_update] deleted {} edges, {} vertices",
        deleted_edge_count, deleted_vertex_count
    );

    Ok((deleted_edge_count, deleted_vertex_count))
}

// ── Procedure registration ──────────────────────────────────────────────────

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::Float64, // ratio
        LogicalType::String,  // output_dir
        LogicalType::Int64,   // seed
    ];

    let schema = Arc::new(DataSchema::new(vec![
        DataField::new("deleted_edges".into(), LogicalType::Int64, false),
        DataField::new("deleted_vertices".into(), LogicalType::Int64, false),
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

        let (del_edges, del_verts) = do_random_update(container, ratio, &output_dir, seed)?;

        let chunk = DataChunk::new(vec![
            Arc::new(Int64Array::from_iter_values([del_edges])),
            Arc::new(Int64Array::from_iter_values([del_verts])),
        ]);
        Ok(vec![chunk])
    })
}
