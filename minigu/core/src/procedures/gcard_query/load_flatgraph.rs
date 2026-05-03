//! `load_flatgraph` procedure: build FlatGraph from LDBC CSVs.
//!
//! Usage:
//!   `call load_flatgraph('<dataset_dir>', '<graph_name>')`
//!
//! This builds a FlatGraph from CSV files (with full property data) and registers
//! it in the schema.  Statistic / DegreeSeqGraphCompressed are NOT loaded here —
//! they are automatically restored at database startup from persisted bin files.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use minigu_catalog::provider::SchemaProvider;
use minigu_common::data_type::LogicalType;
use minigu_context::graph::{GraphContainer, GraphStorage};
use minigu_context::procedure::Procedure;
use minigu_storage::tp::MemoryGraph;

use super::load_ldbc::build_flat_graph;
use crate::procedures::common::Manifest;
use crate::procedures::import_graph::get_graph_type_from_manifest;

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String, // dataset_dir (path to LDBC CSV directory with manifest.json)
        LogicalType::String, // graph_name
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

        let dataset_path = Path::new(&dataset_dir);
        let manifest_path = dataset_path.join("manifest.json");

        // ── Parse manifest ──
        let manifest_data = std::fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", manifest_path.display(), e))?;
        let manifest = Manifest::from_str(&manifest_data)
            .map_err(|e| anyhow::anyhow!("cannot parse manifest: {}", e))?;
        let graph_type = get_graph_type_from_manifest(&manifest)
            .map_err(|e| anyhow::anyhow!("get_graph_type_from_manifest: {}", e))?;

        // ── Build FlatGraph from CSVs ──
        eprintln!("[load_flatgraph] building FlatGraph from CSV...");
        let t0 = std::time::Instant::now();
        let flat_graph = build_flat_graph(&manifest, dataset_path);
        eprintln!(
            "[load_flatgraph] FlatGraph built in {:.2}s ({} vertices, {} edges)",
            t0.elapsed().as_secs_f64(),
            flat_graph.total_vertex_count(),
            flat_graph.total_edge_count(),
        );

        // ── Persist FlatGraph to db directory ──
        if let Some(ref db_path) = context.database().config().db_path {
            let fg_path = crate::catalog_persistence::flatgraph_path(db_path, &graph_name);
            if let Err(e) = flat_graph.export_bincode(&fg_path) {
                eprintln!("[load_flatgraph] warning: could not save FlatGraph: {}", e);
            } else {
                eprintln!("[load_flatgraph] FlatGraph saved to {}", fg_path.display());
            }
        }

        // ── Build GraphContainer (empty MemoryGraph, no MVCC data) ──
        let empty_graph = MemoryGraph::in_memory();
        let container = GraphContainer::new(graph_type, GraphStorage::Memory(empty_graph));

        container
            .set_gcard_flat_graph(Arc::new(flat_graph) as Arc<dyn std::any::Any + Send + Sync>);

        // ── Register in schema ──
        let schema = context
            .current_schema
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("current schema not set"))?;

        if !schema.add_graph(graph_name.clone(), Arc::new(container)) {
            return Err(anyhow::anyhow!("graph '{}' already exists in schema", graph_name).into());
        }

        eprintln!(
            "[load_flatgraph] graph '{}' registered (FlatGraph only)",
            graph_name,
        );

        Ok(vec![])
    })
}
