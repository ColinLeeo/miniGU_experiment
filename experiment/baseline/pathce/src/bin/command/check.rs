use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use clap::Args;
use pathce::pattern::{GeneralPattern, GraphPattern};

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Specify the pattern path.
    #[arg(short, long, value_name = "PATTERN_FILE")]
    pattern: PathBuf,
}

pub fn check(args: CheckArgs) {
    let pattern: Result<GeneralPattern, _> = {
        let file = File::open(args.pattern).unwrap();
        let reader = BufReader::new(file);
        serde_json::from_reader(reader)
    };
    if pattern.is_err() {
        println!("invalid");
        return;
    }
    let pattern = pattern.unwrap();
    if pattern.vertices().len() == 1 && pattern.edges().is_empty() {
        println!("vertex");
        return;
    }
    if pattern.vertices().len() == 2 && pattern.edges().len() == 1 {
        println!("edge");
        return;
    }
    if pattern.is_cyclic() {
        println!("cyclic");
        return;
    }
    println!("acyclic")
}
