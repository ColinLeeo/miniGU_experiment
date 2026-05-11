use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use pathce::counter::{PathCounter, StarCounter};
use pathce::graph::LabeledGraph;
use pathce::pattern::{GeneralPattern, PathPattern};
use rayon::ThreadPoolBuilder;

#[derive(Debug, Args)]
pub struct CountArgs {
    /// Specify the graph path.
    #[arg(short, long, value_name = "GRAPH_FILE")]
    graph: PathBuf,
    /// Specify the pattern path.
    #[arg(short, long)]
    pattern: PathBuf,
    /// Specify the number of threads.
    #[arg(short, long, default_value = "4")]
    threads: usize,
    /// Specify the pattern type.
    #[arg(short, long, default_value = "path")]
    shape: String,
}

pub fn count(args: CountArgs) {
    let graph = Arc::new(LabeledGraph::import_bincode(args.graph).unwrap());
    let pool = Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build()
            .unwrap(),
    );
    let count = match args.shape.as_str() {
        "path" => {
            let counter = PathCounter::new(graph, pool);
            let path: PathPattern = {
                let file = File::open(args.pattern).unwrap();
                let reader = BufReader::new(file);
                serde_json::from_reader(reader).unwrap()
            };
            counter.count(&path)
        }
        "star" => {
            let counter = StarCounter::new(graph, pool);
            let pattern: GeneralPattern = {
                let file = File::open(args.pattern).unwrap();
                let reader = BufReader::new(file);
                serde_json::from_reader(reader).unwrap()
            };
            counter.count(&pattern)
        }
        _ => panic!("Invalid pattern type"),
    };
    println!("{count}");
}
