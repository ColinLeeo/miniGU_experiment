use std::convert::TryInto;
use std::path::PathBuf;

use dyn_type::Object;
use graph_proxy::create_exp_store;
use ir_core::catalogue::catalog::{Catalogue, PatMatPlanSpace};
use ir_core::catalogue::pattern::{Pattern, PatternWithCount};
use ir_core::catalogue::plan::{set_alpha, set_beta, set_w1, set_w2};
use ir_core::plan::logical::LogicalPlan;
use ir_core::plan::physical::AsPhysical;
use pegasus::{Configuration, JobConf};
use pegasus_client::builder::JobBuilder;
use runtime_integration::*;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

/// Execute a query with the plan generated from given subpatterns
#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "p", long = "pattern")]
    pattern: PathBuf,
    #[structopt(short = "c", long = "catalog")]
    catalog: PathBuf,
    #[structopt(short = "t", long = "threads", default_value = "1")]
    threads: u32,
    #[structopt(long = "alpha", default_value = "0.15")]
    alpha: f64,
    #[structopt(long = "beta", default_value = "0.1")]
    beta: f64,
    #[structopt(long = "w1", default_value = "6.0")]
    w1: f64,
    #[structopt(long = "w2", default_value = "3.0")]
    w2: f64,
    #[structopt(long = "batch_size", default_value = "1024")]
    batch_size: u32,
    #[structopt(long = "batch_capacity", default_value = "64")]
    batch_capacity: u32,
}

fn main() -> anyhow::Result<()> {
    pegasus_common::logs::init_log();
    let config = Config::from_args();
    let pattern_meta = read_pattern_meta()?;
    let pattern: Pattern = PatternWithCount::import(config.pattern)?.try_into()?;
    set_alpha(config.alpha);
    set_beta(config.beta);
    set_w1(config.w1);
    set_w2(config.w2);
    let mut plan_catalog = Catalogue::build_from_pattern(&pattern, PatMatPlanSpace::Hybrid);
    let catalog = Catalogue::import(config.catalog)?;

    let pattern_indices: Vec<_> = plan_catalog.pattern_indices_iter().collect();
    for i in &pattern_indices {
        let pattern = plan_catalog
            .get_pattern_weight(*i)
            .unwrap()
            .get_pattern()
            .clone();
        let count = catalog.estimate_pattern_count(&pattern);
        plan_catalog.set_pattern_count(&pattern, count);
    }
    // Update extend weight
    for index in pattern_indices {
        plan_catalog.set_extend_count_infos(index);
    }

    let pb_plan = pattern.generate_optimized_match_plan(&mut plan_catalog, &pattern_meta, false)?;
    let plan: LogicalPlan = pb_plan.try_into().unwrap();

    create_exp_store();
    let server_config = Configuration::singleton();
    let mut conf = JobConf::new("query");
    conf.set_workers(config.threads);
    conf.reset_servers(pegasus::ServerConf::All);
    conf.batch_size = config.batch_size;
    conf.batch_capacity = config.batch_capacity;
    pegasus::startup(server_config)?;
    pegasus::wait_servers_ready(conf.servers());

    let mut job_builder = JobBuilder::default();
    let mut plan_meta = plan.get_meta().clone();
    plan.add_job_builder(&mut job_builder, &mut plan_meta)
        .unwrap();
    let request = job_builder
        .build()
        .expect("Failed at job builder");
    let mut records =
        submit_query_and_fetch_results(request, conf.clone()).expect("Failed to submit query");
    assert_eq!(records.len(), 1);
    let record = records[0].take(Some(&0)).unwrap();
    let count = match record.as_object().unwrap() {
        Object::Primitive(count) => count,
        _ => unreachable!(),
    }
    .as_i64()
    .unwrap();
    println!("{count}");
    Ok(())
}
