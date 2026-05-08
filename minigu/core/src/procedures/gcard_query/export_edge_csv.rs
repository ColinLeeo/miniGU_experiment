//! `export_edge_csv` procedure — export a single edge type's CSV from the
//! in-memory FlatGraph.
//!
//! Usage:
//!   `call export_edge_csv('<edge_label>', '<output_path>')`
//!
//! Writes a CSV file with columns `src,dst[,prop1,prop2,...]` for edges of the
//! specified label.  Much faster than a full `export_flatgraph_snapshot` when
//! only one edge type has been modified.

use std::io;
use std::sync::Arc;

use minigu_common::data_type::LogicalType;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;

use super::flat_graph::FlatGraph;
use super::random_update::export_single_edge_csv;

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String, // edge_label
        LogicalType::String, // output_path (file path, e.g. "/tmp/person_knows_person.csv")
    ];

    Procedure::new(parameters, None, move |context, args| {
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

        let edge_label = args[0]
            .try_as_string()
            .expect("first arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("edge_label cannot be null"))?
            .to_string();
        let output_path = args[1]
            .try_as_string()
            .expect("second arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("output_path cannot be null"))?
            .to_string();

        let fg_arc = container
            .gcard_flat_graph()
            .and_then(|arc| Arc::downcast::<FlatGraph>(arc).ok())
            .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded"))?;

        export_single_edge_csv(&fg_arc, &edge_label, std::path::Path::new(&output_path))?;

        eprintln!("[export_edge_csv] wrote {} to {}", edge_label, output_path);
        Ok(vec![])
    })
}
