mod command;

use std::thread;

use clap::Parser;
use mimalloc::MiMalloc;

use crate::command::*;

#[global_allocator]
static ALLOC: MiMalloc = MiMalloc;

/// An integrated framework for cardinality estimation of subgraph queries.
#[derive(Parser)]
#[command(version, about)]
#[command(propagate_version = true)]
enum Command {
    /// Load the CSV graph dataset and serialize it into a (bincode) graph file.
    Serialize(SerializeArgs),
    /// Analyze statistics from edges and paths (V2).
    Analyze(AnalyzeArgs),
    /// Build CEG catalogue
    BuildCegCatalog(BuildCegCatalogArgs),
    /// Count the given path pattern.
    Count(CountArgs),
    /// Check the type of the input pattern.
    Check(CheckArgs),
    /// Estimate the cardinality (V2).
    Estimate(EstimateArgs),
    /// Estimate the cardinality by manually specifying the tables.
    EstimateManual(EstimateManualArgs),
    /// Calculate the topological statistics of a pattern.
    PatternStatistics(PatternStatisticsArgs),
    /// Print the contents in the catalog.
    Show(ShowArgs),
    /// Print the statistics of a graph.
    Graph(GraphArgs),
    /// Generate patterns from the schema.
    GeneratePatterns(GeneratePatternsArgs),
}

const STACK_SIZE: usize = 128 * 1024 * 1024;

fn main() {
    env_logger::init();
    let handle = thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            let command = Command::parse();
            match command {
                Command::Serialize(args) => serialize(args),
                Command::Analyze(args) => analyze(args),
                Command::BuildCegCatalog(args) => build_ceg_catalog(args),
                Command::Estimate(args) => estimate(args),
                Command::EstimateManual(args) => estimate_manual(args),
                Command::Show(args) => show(args),
                Command::PatternStatistics(args) => pattern_statistics(args),
                Command::Graph(args) => graph(args),
                Command::GeneratePatterns(args) => generate_patterns(args),
                Command::Count(args) => count(args),
                Command::Check(args) => check(args),
            }
        })
        .unwrap();
    handle.join().unwrap()
}
