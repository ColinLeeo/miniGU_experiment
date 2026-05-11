use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

use clap::Args;
use pathce::catalog::DuckCatalog;
use pathce::estimate::{CardinalityEstimatorManual, CatalogPattern};

#[derive(Debug, Args)]
pub struct EstimateManualArgs {
    /// Specify the catalog directory.
    #[arg(short, long, value_name = "CATALOG_DIR")]
    catalog: PathBuf,
    /// Specify the pattern path.
    #[arg(short, long, value_name = "PATTERN_FILE")]
    patterns: Vec<PathBuf>,
}

pub fn estimate_manual(args: EstimateManualArgs) {
    let catalog = DuckCatalog::import(args.catalog).unwrap();
    let estimator = CardinalityEstimatorManual::new(&catalog);
    for pattern in args.patterns {
        let pattern: CatalogPattern =
            serde_json::from_reader(File::open(pattern).unwrap()).unwrap();
        let start = Instant::now();
        let card = estimator.estimate(pattern).unwrap();
        let time = start.elapsed().as_secs_f64();
        println!("{},{}", card, time);
    }
}
