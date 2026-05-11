use std::convert::TryInto;
use std::fs::read_dir;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use ir_core::catalogue::catalog::{Catalogue, PatMatPlanSpace};
use ir_core::catalogue::pattern::{Pattern, PatternWithCount};
use ir_core::catalogue::plan::{set_alpha, set_beta, set_w1, set_w2};
use ir_core::plan::logical::LogicalPlan;
use log::warn;
use runtime_integration::*;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

/// Execute a query with the plan generated from given subpatterns
#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "p", long = "pattern")]
    pattern: PathBuf,
    #[structopt(short = "s", long = "subpatterns")]
    subpatterns: PathBuf,
    #[structopt(long = "alpha", default_value = "0.15")]
    alpha: f64,
    #[structopt(long = "beta", default_value = "0.1")]
    beta: f64,
    #[structopt(long = "w1", default_value = "6.0")]
    w1: f64,
    #[structopt(long = "w2", default_value = "3.0")]
    w2: f64,
    #[structopt(long = "plan_space", default_value = "hybrid")]
    plan_space: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .target(env_logger::Target::Stdout)
        .filter_level(log::LevelFilter::Debug)
        .init();

    let config = Config::from_args();
    let pattern_meta = read_pattern_meta()?;
    let pattern: Pattern = PatternWithCount::import(config.pattern)?.try_into()?;
    let plan_space = match config.plan_space.as_str() {
        "extend" => PatMatPlanSpace::ExtendWithIntersection,
        "hybrid" => PatMatPlanSpace::Hybrid,
        "join" => PatMatPlanSpace::BinaryJoin,
        _ => panic!("unsupported plan space: {}", config.plan_space),
    };
    set_alpha(config.alpha);
    set_beta(config.beta);
    set_w1(config.w1);
    set_w2(config.w2);
    let mut catalog = Catalogue::build_from_pattern(&pattern, plan_space);

    let start = Instant::now();
    let mut num_updated = 0;
    for entry in read_dir(config.subpatterns)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || !entry
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(".json")
        {
            continue;
        }
        let path = entry.path();
        match PatternWithCount::import(&path) {
            Ok(pattern_with_count) => {
                let count = pattern_with_count.count().unwrap_or_default();
                if catalog.set_pattern_count(&pattern_with_count.try_into()?, count) {
                    num_updated += 1;
                }
            }
            Err(e) => {
                warn!("failed to load pattern from {:?}: {}", path, e);
                continue;
            }
        }
    }
    // Update extend weight
    let pattern_indices: Vec<_> = catalog.pattern_indices_iter().collect();
    for index in pattern_indices {
        catalog.set_extend_count_infos(index);
    }
    println!(
        "The counts of {} patterns are updated, time: {} ms.",
        num_updated,
        start.elapsed().as_millis()
    );

    let start = Instant::now();
    let pb_plan = pattern.generate_optimized_match_plan(&mut catalog, &pattern_meta, false)?;
    let plan: LogicalPlan = pb_plan.try_into().unwrap();
    println!("planning time: {} ms", start.elapsed().as_millis());
    println!("{}", plan);
    Ok(())
}
