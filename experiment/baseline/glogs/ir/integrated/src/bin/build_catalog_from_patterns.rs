use std::convert::TryInto;
use std::fs::read_dir;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use ir_core::catalogue::catalog::{Catalogue, PatMatPlanSpace};
use ir_core::catalogue::pattern::PatternWithCount;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "s", long = "plan_space", default_value = "hybrid")]
    plan_space: String,
    #[structopt(short = "i", long = "input")]
    input: PathBuf,
    #[structopt(short = "o", long = "output")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = Config::from_args();
    let plan_space: PatMatPlanSpace = match config.plan_space.as_str() {
        "extend" => PatMatPlanSpace::ExtendWithIntersection,
        "hybrid" => PatMatPlanSpace::Hybrid,
        _ => unreachable!(),
    };
    let start = Instant::now();
    let mut num_updated = 0;
    let mut catalog = Catalogue::default();
    catalog.set_plan_space(plan_space);
    for entry in read_dir(config.input)? {
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
        let pattern_with_count = PatternWithCount::import(&path)
            .with_context(|| format!("failed to load pattern from {:?}", path))?;
        let count = pattern_with_count.count().unwrap_or_default();
        let pattern = pattern_with_count.try_into()?;
        catalog.update_catalog_by_pattern(&pattern);
        if catalog.set_pattern_count(&pattern, count) {
            num_updated += 1;
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
    catalog.export(config.output)?;
    Ok(())
}
