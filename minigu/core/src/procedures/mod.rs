mod build_ldbc_mainfest;
mod common;
mod create_test_graph;
mod create_test_graph_data;
mod create_test_vector_graph_data;
mod echo;
mod export_graph;
mod gcard_query;
mod import_graph;
mod mutate_graph;
mod show_graph;
mod show_procedures;

pub(crate) use gcard_query::flat_graph::FlatGraph;
pub(crate) use gcard_query::load_statistic;
pub(crate) use gcard_query::update_log::new_log_arc as new_gcard_update_log;
pub(crate) use import_graph::import;
use minigu_context::procedure::Procedure;

pub fn build_predefined_procedures() -> Vec<(String, Procedure)> {
    vec![
        ("echo".to_string(), echo::build_procedure()),
        (
            "show_procedures".to_string(),
            show_procedures::build_procedure(),
        ),
        (
            "create_test_graph".to_string(),
            create_test_graph::build_procedure(),
        ),
        (
            "create_test_graph_data".to_string(),
            create_test_graph_data::build_procedure(),
        ),
        (
            "create_test_vector_graph_data".to_string(),
            create_test_vector_graph_data::build_procedure(),
        ),
        // Show graph in current schema.
        ("show_graph".to_string(), show_graph::build_procedure()),
        ("import_graph".to_string(), import_graph::build_procedure()),
        ("export_graph".to_string(), export_graph::build_procedure()),
        (
            "create_catalog".to_string(),
            gcard_query::create_catalog::build_procedure(),
        ),
        (
            "load_catalog".to_string(),
            gcard_query::load_catalog::build_load_procedure(),
        ),
        (
            "build_ldbc_manifest".to_string(),
            build_ldbc_mainfest::build_procedure(),
        ),
        (
            "load_ldbc".to_string(),
            gcard_query::load_ldbc::build_procedure(),
        ),
        (
            "load_flatgraph".to_string(),
            gcard_query::load_flatgraph::build_procedure(),
        ),
        ("gcard_query".to_string(), gcard_query::build_procedure()),
        ("mutate_graph".to_string(), mutate_graph::build_procedure()),
        (
            "compact_gcard_log".to_string(),
            gcard_query::compact_update_log::build_procedure(),
        ),
        (
            "GCard_stat_quality".to_string(),
            gcard_query::stat_quality::build_procedure(),
        ),
        (
            "random_update".to_string(),
            gcard_query::random_update::build_procedure(),
        ),
        (
            "export_flatgraph_snapshot".to_string(),
            gcard_query::export_flatgraph_snapshot::build_procedure(),
        ),
        (
            "random_insert".to_string(),
            gcard_query::random_insert::build_procedure(),
        ),
        (
            "export_edge_csv".to_string(),
            gcard_query::export_edge_csv::build_procedure(),
        ),
    ]
}
