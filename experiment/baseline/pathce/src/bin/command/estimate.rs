use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

use clap::Args;
use pathce::catalog::DuckCatalog;
use pathce::common::TagId;
use pathce::estimate::CardinalityEstimator;
use pathce::pattern::RawPattern;
use log::info;

#[derive(Debug, Args)]
pub struct EstimateArgs {
    /// Specify the catalog directory.
    #[arg(short, long, value_name = "CATALOG_DIR")]
    catalog: PathBuf,
    /// Specify the pattern path.
    #[arg(short, long, value_name = "PATTERN_FILE")]
    patterns: Vec<PathBuf>,
    /// Specify the maximum path length.
    #[arg(long, default_value = "3")]
    max_path_length: usize,
    /// Specify the maximum star length.
    #[arg(long, default_value = "1")]
    max_star_length: usize,
    /// Specify the maximum degree of star (for star statistics)
    #[arg(long, default_value = "5")]
    max_star_degree: usize,
    /// Specify the number of spanning trees when decomposing cyclic patterns.
    #[arg(short, long, default_value = "10")]
    limit: usize,
    /// Specify whether to disable the star statistics in query decomposition.
    #[arg(long)]
    disable_star: bool,
    /// Specify whether to disable query pruning.
    #[arg(long)]
    disable_prune: bool,
    /// Specify whether to estimate cyclic patterns using spanning trees only
    #[arg(long)]
    disable_cyclic: bool,
    /// Specify a predefined elimination order.
    #[arg(long)]
    order: Option<String>,
}

fn parse_order(order: String) -> Vec<TagId> {
    order
        .trim()
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect()
}

pub fn estimate(args: EstimateArgs) {
    let catalog = DuckCatalog::import(args.catalog).unwrap();
    let estimator = CardinalityEstimator::new(
        &catalog,
        args.max_path_length,
        args.max_star_length,
        args.max_star_degree,
        args.limit,
        args.disable_star,
        args.disable_prune,
        args.disable_cyclic,
    );
    if let Some(order) = args.order {
        assert_eq!(
            args.patterns.len(),
            1,
            "only one pattern can be estimated using predefined order"
        );
        let pattern = args.patterns.first().unwrap();
        let pattern: RawPattern = serde_json::from_reader(File::open(pattern).unwrap()).unwrap();
        let pattern = pattern.to_general().unwrap();
        let order = parse_order(order);
        let start = Instant::now();
        let card = estimator.estimate_with_order(&pattern, order).unwrap();
        let time = start.elapsed().as_secs_f64();
        println!("{},{}", card, time);
    } else {
        for pattern in args.patterns {
            info!("estimate {:?}", pattern);
            let pattern: RawPattern =
                serde_json::from_reader(File::open(pattern).unwrap()).unwrap();
            let pattern = pattern.to_general().unwrap();
            let start = Instant::now();
            let card = estimator.estimate(&pattern).unwrap();
            let time = start.elapsed().as_secs_f64();
            println!("{},{}", card, time);
        }
    }
}
