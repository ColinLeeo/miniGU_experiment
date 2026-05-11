use std::fs::create_dir_all;
use std::path::PathBuf;

use ir_core::catalogue::catalog::{Catalogue, PatMatPlanSpace};
use ir_core::catalogue::pattern::PatternWithCount;
use runtime_integration::{read_pattern, read_pattern_meta, read_patterns};
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "m", long = "catalog_mode", default_value = "from_meta")]
    catalog_mode: String,
    #[structopt(short = "d", long = "catalog_depth", default_value = "3")]
    catalog_depth: usize,
    #[structopt(short = "s", long = "plan_space", default_value = "hybrid")]
    plan_space: String,
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
    let catalog = match config.catalog_mode.as_str() {
        "from_pattern" => {
            let pattern = read_pattern()?;
            Catalogue::build_from_pattern(&pattern, plan_space)
        }
        "from_patterns" => {
            let patterns = read_patterns()?;
            let mut catalog = Catalogue::default();
            catalog.set_plan_space(plan_space);
            for pattern in patterns {
                catalog.update_catalog_by_pattern(&pattern)
            }
            catalog
        }
        "from_meta" => {
            let pattern_meta = read_pattern_meta()?;
            Catalogue::build_from_meta(&pattern_meta, config.catalog_depth, config.catalog_depth)
        }
        "mix_mode" => {
            let pattern_meta = read_pattern_meta()?;
            let patterns = read_patterns()?;
            let mut catalog =
                Catalogue::build_from_meta(&pattern_meta, config.catalog_depth, config.catalog_depth);
            catalog.set_plan_space(plan_space);
            for pattern in patterns {
                catalog.update_catalog_by_pattern(&pattern)
            }
            catalog
        }
        _ => unreachable!(),
    };
    create_dir_all(&config.output)?;
    for index in catalog.pattern_indices_iter() {
        let pattern = catalog
            .get_pattern_weight(index)
            .unwrap()
            .get_pattern();
        let path = config
            .output
            .join(format!("{:0>5}.json", index.index()));
        PatternWithCount::from(pattern.clone()).export(path)?;
    }
    Ok(())
}
