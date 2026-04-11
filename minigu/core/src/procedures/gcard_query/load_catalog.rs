use std::sync::Arc;

use minigu_catalog::provider::SchemaProvider;
use minigu_common::data_type::LogicalType;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;

use crate::procedures::gcard_query::statistic::load_statistic;

/// Procedure: `call load_catalog("<graph_name>")`
///
/// Loads a previously saved statistic from `<db_path>/<graph_name>.statistic.bin`,
/// rebuilds `DegreeSeqGraphCompressed` from it, and sets both on the `GraphContainer`.
pub fn build_load_procedure() -> Procedure {
    let parameters = vec![LogicalType::String];
    Procedure::new(parameters, None, move |context, args| {
        let graph_name = args[0]
            .try_as_string()
            .expect("expecting string value for graph_name")
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expecting string value for graph name"))?
            .to_string();

        let db_path =
            context.database().config().db_path.clone().ok_or_else(|| {
                anyhow::anyhow!("no db_path configured (in-memory only database)")
            })?;

        let schema = context
            .current_schema
            .ok_or_else(|| anyhow::anyhow!("current schema not set"))?;
        let graph_ref = schema
            .get_graph(&graph_name)?
            .ok_or_else(|| anyhow::anyhow!("graph named '{}' not found", graph_name))?;
        let graph_container = graph_ref
            .downcast_ref::<GraphContainer>()
            .ok_or_else(|| anyhow::anyhow!("graph '{}' container type mismatch", graph_name))?;

        let stat_path = crate::catalog_persistence::statistic_path(&db_path, &graph_name);
        let statistic = load_statistic(&stat_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no saved statistic found for graph '{}' at {}",
                graph_name,
                stat_path.display()
            )
        })?;

        let degree_seq_graph_compressed = statistic
            .to_degree_seq_graph_compressed()
            .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {}", e))?;

        graph_container.clear_gcard_data();
        graph_container.set_degree_seq_graph_compressed(Arc::new(degree_seq_graph_compressed));
        graph_container.set_statistic(Arc::new(statistic));

        println!("Catalog loaded for graph '{}'", graph_name);
        Ok(vec![])
    })
}
