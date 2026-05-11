use std::collections::HashMap;
use std::convert::TryInto;
use std::fs::create_dir_all;
use std::path::PathBuf;

use ir_core::catalogue::catalog::{Catalogue, PatMatPlanSpace};
use ir_core::catalogue::pattern::{Pattern, PatternWithCount};
use log::info;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "p", long = "pattern")]
    pattern: PathBuf,
    #[structopt(short = "o", long = "outdir")]
    outdir: PathBuf,
    #[structopt(short = "s", long = "plan_space", default_value = "hybrid")]
    plan_space: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = Config::from_args();
    let pattern: Pattern = PatternWithCount::import(config.pattern)?.try_into()?;
    let plan_space = match config.plan_space.as_str() {
        "extend" => PatMatPlanSpace::ExtendWithIntersection,
        "hybrid" => PatMatPlanSpace::Hybrid,
        "join" => PatMatPlanSpace::BinaryJoin,
        _ => panic!("unsupported plan space: {}", config.plan_space),
    };
    let catalog = Catalogue::build_from_pattern(&pattern, plan_space);
    let subpatterns: HashMap<_, _> = catalog
        .pattern_indices_iter()
        .map(|index| {
            (
                index,
                catalog
                    .get_pattern_weight(index)
                    .unwrap()
                    .get_pattern(),
            )
        })
        .collect();
    info!("generate {} subpatterns", subpatterns.len());
    create_dir_all(&config.outdir)?;
    for (idx, pattern) in subpatterns.into_iter() {
        let path = config
            .outdir
            .join(format!("{:0>5}.json", idx.index()));
        let pattern: PatternWithCount = pattern.clone().into();
        pattern.export(path)?;
    }
    Ok(())
}
