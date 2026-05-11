use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use pathce::graph::{LabeledGraph, LabeledVertex};
use pathce::schema::Schema;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Specify the serialized graph path.
    #[arg(short, long, value_name = "GRAPH_FILE")]
    graph: PathBuf,
    /// Specify the schema path.
    #[arg(short, long, value_name = "SCHEMA_JSON")]
    schema: PathBuf,
    /// Specify the maximum path length.
    #[arg(long, default_value = "4")]
    max_length: usize,
}

pub fn graph(args: GraphArgs) {
    println!("{:?}", args);
    let graph = Arc::new(LabeledGraph::import_bincode(args.graph).unwrap());
    let schema = Arc::new(Schema::import_json(args.schema).unwrap());
    let mut vlabel_to_count = BTreeMap::new();
    let mut elabel_to_count = BTreeMap::new();
    let mut total_v_count = 0;
    let mut total_e_count = 0;
    for v in schema.vertices() {
        let count = graph.vertices(v.label).unwrap().len();
        let name = schema.get_vertex_label_name(v.label).unwrap();
        vlabel_to_count.insert(name.clone(), count);
        total_v_count += count;
    }
    for e in schema.edges() {
        let from = e.from;
        let name = schema.get_edge_label_name(e.label).unwrap();
        let count: usize = graph
            .vertices(from)
            .unwrap()
            .par_iter()
            .map(|v| {
                graph
                    .outgoing_degree(LabeledVertex::new(*v, from), e.label)
                    .unwrap()
            })
            .sum();
        elabel_to_count.insert(name.clone(), count);
        total_e_count += count;
    }
    println!("vlabels: {}", schema.vertices().len());
    for (v, count) in vlabel_to_count.into_iter() {
        println!("{v}: {count}")
    }
    println!("elabels: {}", schema.edges().len());
    for (e, count) in elabel_to_count.into_iter() {
        println!("{e}: {count}")
    }
    println!("total_v: {total_v_count}, total_e: {total_e_count}");

    for i in 1..=args.max_length {
        let paths = schema.generate_paths(i);
        println!("{i}-path: {}", paths.len());
    }
}
