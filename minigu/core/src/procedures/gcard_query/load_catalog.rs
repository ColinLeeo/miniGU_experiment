//! 从磁盘恢复已保存的 GCard catalog/statistic。
//!
//! 适合在 catalog 已经构建过的情况下快速恢复运行环境，
//! 避免重新执行一次完整的 `GCard_build`。

use std::sync::Arc;

use minigu_catalog::provider::SchemaProvider;
use minigu_common::data_type::LogicalType;
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;

use crate::procedures::gcard_query::create_catalog::add_functional_path_aliases_for_existing_catalog;
use crate::procedures::gcard_query::statistic::load_statistic;
use crate::procedures::gcard_query::utils::{
    edge_cardinalities_from_schema, get_edges_from_catalog,
};

/// Procedure: `call load_catalog("<graph_name>")`
///
/// Loads a previously saved statistic from `<db_path>/<graph_name>.statistic.bin`,
/// rebuilds `DegreeSeqGraphCompressed` from it, and sets both on the `GraphContainer`.
pub fn build_load_procedure() -> Procedure {
    let parameters = vec![LogicalType::String];
    Procedure::new(parameters, None, move |context, args| {
        // 这里只从持久化的 Statistic 恢复，
        // 查询用的 `DegreeSeqGraphCompressed` 会现场重新派生出来。
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

        let mut degree_seq_graph_compressed = statistic
            .to_degree_seq_graph_compressed()
            .map_err(|e| anyhow::anyhow!("to_degree_seq_graph_compressed: {}", e))?;
        let edges = get_edges_from_catalog(graph_container.graph_type().as_ref())?;
        degree_seq_graph_compressed.edge_cardinalities = edge_cardinalities_from_schema(&edges);
        let alias_count = add_functional_path_aliases_for_existing_catalog(
            &mut degree_seq_graph_compressed,
            &edges,
        );
        eprintln!(
            "[load_catalog] functional path aliases generated: {} (not scanned)",
            alias_count
        );

        graph_container.clear_gcard_data();
        graph_container.set_degree_seq_graph_compressed(Arc::new(degree_seq_graph_compressed));
        graph_container.set_statistic(Arc::new(statistic));

        println!("Catalog loaded for graph '{}'", graph_name);
        Ok(vec![])
    })
}
