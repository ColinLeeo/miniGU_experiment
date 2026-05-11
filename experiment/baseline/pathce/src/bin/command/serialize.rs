use std::path::PathBuf;
use std::time::Instant;

use clap::Args;
use pathce::graph::LabeledGraph;
use pathce::schema::Schema;

#[derive(Debug, Args)]
pub struct SerializeArgs {
    /// Specify the input dataset directory.
    #[arg(short, long, value_name = "DATASET_DIR")]
    input: PathBuf,
    /// Specify the schema json.
    #[arg(short, long, value_name = "SCHEMA_JSON")]
    schema: PathBuf,
    /// Specify the output file.
    #[arg(short, long, value_name = "OUTPUT_FILE")]
    output: PathBuf,
    /// Specify the CSV delimiter.
    #[arg(long, value_name = "DELIMITER", default_value = ",")]
    delimiter: char,
    /// Specify the number of graph building threads.
    #[arg(short, long, value_name = "THREADS", default_value = "8")]
    threads: usize,
}

pub fn serialize(args: SerializeArgs) {
    println!("{:#?}", args);
    let schema = Schema::import_json(args.schema).unwrap();
    let start = Instant::now();
    let graph =
        LabeledGraph::from_csv(args.input, &schema, args.delimiter as u8, args.threads).unwrap();
    let time = start.elapsed().as_secs_f64();
    println!("graph building time: {time} s");

    let start = Instant::now();
    graph.export_bincode(args.output).unwrap();
    let time = start.elapsed().as_secs_f64();
    println!("serializing time: {time} s");
}
