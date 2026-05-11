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
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use ir_core::catalogue::catalog::Catalogue;
use ir_core::catalogue::pattern::{Pattern, PatternEdge, PatternVertex};
use ir_core::catalogue::pattern_meta::PatternMeta;
use ir_core::catalogue::{PatternId, PatternLabelId};
use itertools::Itertools;
use runtime_integration::read_pattern_meta;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(short = "c", long = "catalog")]
    catalog: PathBuf,
    #[structopt(short = "i", long = "input")]
    input: PathBuf,
    #[structopt(short = "o", long = "output")]
    output: PathBuf,
}

fn parse_pattern(meta: &PatternMeta, line: &str) -> Pattern {
    let (edges, labels) = line.split(',').skip(1).collect_tuple().unwrap();
    let edges = edges
        .split(';')
        .map(|e| {
            let (src, dst) = e
                .split('-')
                .map(|v| v.parse::<PatternId>().unwrap())
                .collect_tuple()
                .unwrap();
            (src, dst)
        })
        .collect_vec();
    let labels = labels
        .split("->")
        .map(|l| l.parse::<PatternLabelId>().unwrap())
        .collect_vec();
    let edges = edges
        .into_iter()
        .zip_eq(labels)
        .enumerate()
        .map(|(i, ((src, dst), label))| {
            let ((src_label, dst_label),) = meta
                .associated_vlabels_iter_by_elabel(label)
                .collect_tuple()
                .unwrap();
            let start_vertex = PatternVertex::new(src, src_label);
            let end_vertex = PatternVertex::new(dst, dst_label);
            PatternEdge::new(i, label, start_vertex, end_vertex)
        })
        .collect_vec();
    edges.try_into().unwrap()
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = Config::from_args();
    let catalog = Catalogue::import(config.catalog)?;
    let pattern_meta = read_pattern_meta()?;
    let decom = {
        let file = File::open(config.input)?;
        BufReader::new(file)
    };
    let mut output_lines = Vec::new();
    for line in decom.lines() {
        let line = line?;
        let pattern = parse_pattern(&pattern_meta, &line);
        let count = if let Some(index) = catalog.get_pattern_index(&pattern.encode_to()) {
            let weight = catalog.get_pattern_weight(index).unwrap();
            weight.get_count()
        } else {
            0.into()
        };
        output_lines.push((line, count));
    }
    let mut output = {
        let file = File::create(config.output)?;
        BufWriter::new(file)
    };
    for (line, count) in output_lines {
        writeln!(&mut output, "{line},{count}")?;
    }
    Ok(())
}
