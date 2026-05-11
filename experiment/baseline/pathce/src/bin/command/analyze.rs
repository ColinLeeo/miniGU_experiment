use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Args;
use pathce::catalog_builder::CatalogBuilder;
use pathce::graph::LabeledGraph;
use pathce::pattern::{PatternVertex, RawPattern};
use pathce::schema::{PathTreeNodeRef, Schema};
use rayon::ThreadPoolBuilder;

#[derive(Debug, Args, Clone)]
pub struct AnalyzeArgs {
    /// Specify the number of buckets.
    #[arg(short, long, default_value = "200")]
    buckets: usize,
    /// Specify the schema path.
    #[arg(short, long, value_name = "SCHEMA_JSON")]
    schema: PathBuf,
    /// Specify the serialized graph path.
    #[arg(short, long, value_name = "GRAPH_FILE")]
    graph: PathBuf,
    /// Specify the maximum path length.
    #[arg(long, default_value = "3")]
    max_path_length: usize,
    /// Specify the maximum star length.
    #[arg(long, default_value = "1")]
    max_star_length: usize,
    /// Specify the maximum degree of star (for star statistics)
    #[arg(long, default_value = "5")]
    max_star_degree: usize,
    /// Specify whether to use greedy binning
    #[arg(long)]
    greedy: bool,
    /// Specify whether to skip path statistics
    #[arg(long)]
    skip_path: bool,
    /// Specify whether to save bucket maps (for debugging)
    #[arg(long)]
    save_bucket_map: bool,
    /// Specify the number of worker threads.
    #[arg(short, long, value_name = "THREADS", default_value = "8")]
    threads: usize,
    /// Specify the output directory.
    #[arg(short, long, value_name = "OUTPUT_DIR")]
    output: Option<PathBuf>,
}

fn traverse_path_tree<F>(root: PathTreeNodeRef, path: &mut Vec<PatternVertex>, callback: &mut F)
where
    F: FnMut(&[PatternVertex]),
{
    let end = root.path().end();
    path.push(end);
    for child in root.children() {
        traverse_path_tree(child, path, callback);
    }
    callback(path);
    path.pop();
}

fn estimate_memory_footprint(
    schema: Arc<Schema>,
    graph: Arc<LabeledGraph>,
    max_path_length: usize,
    buckets: usize,
) -> usize {
    let mut max_memory = 0;
    for v in schema.vertices() {
        let path = RawPattern::new()
            .push_back_vertex((0, v.label))
            .to_path()
            .unwrap();
        let tree = schema.generate_path_tree_from_path_end(&path, max_path_length);
        let mut path = vec![];
        traverse_path_tree(tree.root(), &mut path, &mut |path| {
            let mut memory = 0;
            for v in path {
                let vertex_map = graph.get_internal_vertex_map(v.label_id()).unwrap();
                memory += vertex_map.len() * buckets * 8;
            }
            max_memory = max_memory.max(memory);
        });
    }
    max_memory
}

pub fn analyze(args: AnalyzeArgs) {
    println!("{:#?}", args);
    let schema = Arc::new(Schema::import_json(args.schema).unwrap());
    let graph = Arc::new(LabeledGraph::import_bincode(args.graph).unwrap());
    let pool = Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build()
            .unwrap(),
    );

    let memory_footprint = estimate_memory_footprint(
        schema.clone(),
        graph.clone(),
        args.max_path_length,
        args.buckets,
    );
    println!("estimate memory footprint: {memory_footprint} bytes");

    let builder = CatalogBuilder::new(schema, graph, pool)
        .max_path_length(args.max_path_length)
        .max_star_length(args.max_star_length)
        .max_star_degree(args.max_star_degree)
        .buckets(args.buckets)
        .enable_greedy_bucket(args.greedy)
        .save_bucket_map(args.save_bucket_map)
        .skip_path(args.skip_path);

    let start = Instant::now();
    let catalog = builder.build().unwrap();
    println!("total building time: {} s", start.elapsed().as_secs_f64());

    if let Some(output) = args.output {
        let start = Instant::now();
        catalog.export(output).unwrap();
        println!("export time: {} s", start.elapsed().as_secs_f64());
    }
}
