use std::fs::File;
use std::path::PathBuf;

use clap::Args;
use pathce::estimate::decompose::heuristic::find_candidate_paths;
use pathce::pattern::{GraphPattern, RawPattern};

#[derive(Debug, Args)]
pub struct PatternStatisticsArgs {
    /// Specify the pattern path.
    #[arg(short, long, value_name = "PATTERN_FILE")]
    pattern: PathBuf,
}

pub fn pattern_statistics(args: PatternStatisticsArgs) {
    let pattern: RawPattern = serde_json::from_reader(File::open(args.pattern).unwrap()).unwrap();
    let pattern = pattern.to_general().unwrap();
    let longest_path = if pattern.vertices().len() == 1 && pattern.edges().is_empty() {
        0
    } else {
        let paths = find_candidate_paths(&pattern);
        paths
            .values()
            .flatten()
            .map(|p| p.len())
            .max()
            .unwrap_or_default()
    };
    println!("{longest_path}")
}
