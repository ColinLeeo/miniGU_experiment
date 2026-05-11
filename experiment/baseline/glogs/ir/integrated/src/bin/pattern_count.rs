//
//! Copyright 2022 Alibaba Group Holding Limited.
//!
//! Licensed under the Apache License, Version 2.0 (the "License");
//! you may not use this file except in compliance with the License.
//! You may obtain a copy of the License at
//!
//! http://www.apache.org/licenses/LICENSE-2.0
//!
//! Unless required by applicable law or agreed to in writing, software
//! distributed under the License is distributed on an "AS IS" BASIS,
//! WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//! See the License for the specific language governing permissions and
//! limitations under the License.
//!
use std::convert::TryInto;
use std::{path::PathBuf, time::Instant};

use ir_core::catalogue::catalog::Catalogue;
use ir_core::catalogue::pattern::{Pattern, PatternWithCount};
use log::debug;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "p", long = "pattern")]
    pattern: PathBuf,
    #[structopt(short = "c", long = "catalog")]
    catalog: PathBuf,
    #[structopt(short = "r", long = "random")]
    random: bool,
    #[structopt(short = "s", long = "sample", default_value = "100")]
    sample: usize,
    #[structopt(long = "seed", default_value = "12345")]
    seed: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = Config::from_args();
    let pattern: Pattern = PatternWithCount::import(config.pattern)?.try_into()?;
    let catalog = Catalogue::import(config.catalog)?;
    let start = Instant::now();
    let pattern_code = pattern.encode_to();
    let pattern_count = if let Some(pattern_index) = catalog.get_pattern_index(&pattern_code) {
        let pattern_count = catalog
            .get_pattern_weight(pattern_index)
            .unwrap()
            .get_count();
        debug!("pattern: {}, index: {:?}, count: {}", pattern, pattern_index, pattern_count);
        pattern_count
    } else {
        let pattern_count = if config.random {
            catalog.estimate_pattern_count_random(&pattern, config.sample, config.seed)
        } else {
            catalog.estimate_pattern_count(&pattern)
        };
        debug!("pattern: {}, count: {}", pattern, pattern_count);
        pattern_count
    };
    let time = start.elapsed().as_secs_f64();
    println!("{pattern_count},{time}");
    Ok(())
}
