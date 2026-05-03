//! Procedure: `call export_flatgraph_snapshot("<flatgraph_bincode>", "<output_dir>")`
//!
//! Loads a previously exported [`FlatGraph`] bincode and writes a CSV snapshot
//! compatible with `true_cardinality.py`.

use minigu_common::data_type::LogicalType;
use minigu_context::procedure::Procedure;

use crate::procedures::gcard_query::flat_graph::FlatGraph;
use crate::procedures::gcard_query::random_update::export_csv_snapshot;

pub fn build_procedure() -> Procedure {
    let parameters = vec![
        LogicalType::String, // flatgraph_bincode_path
        LogicalType::String, // output_dir
    ];

    Procedure::new(parameters, None, move |_context, args| {
        let bincode_path = args[0]
            .try_as_string()
            .expect("first arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("flatgraph_bincode_path cannot be null"))?
            .to_string();
        let output_dir = args[1]
            .try_as_string()
            .expect("second arg must be a string")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("output_dir cannot be null"))?
            .to_string();

        let graph = FlatGraph::import_bincode(&bincode_path)?;
        export_csv_snapshot(&graph, std::path::Path::new(&output_dir))?;
        println!(
            "export_flatgraph_snapshot: wrote CSV snapshot from {} to {}",
            bincode_path, output_dir
        );

        Ok(vec![])
    })
}
