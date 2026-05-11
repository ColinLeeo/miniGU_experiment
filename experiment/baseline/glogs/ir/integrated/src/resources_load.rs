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
use std::fs::{read_dir, read_to_string, File};
use std::path::Path;

use graph_store::config::{DIR_GRAPH_SCHEMA, FILE_SCHEMA};
use graph_store::prelude::LargeGraphDB;
use graph_store::prelude::{DefaultId, GraphDBConfig, InternalId};
use ir_core::catalogue::catalog::Catalogue;
use ir_core::catalogue::pattern::{Pattern, PatternWithCount};
use ir_core::catalogue::pattern_meta::PatternMeta;
use ir_core::error::IrError;
use ir_core::plan::meta::Schema;
use ir_core::JsonIO;
use lazy_static::lazy_static;
use runtime::IRJobAssembly;

use crate::{InitializeJobAssembly, QueryExpGraph};

lazy_static! {
    pub(crate) static ref FACTORY: IRJobAssembly = initialize_job_assembly();
}

pub fn read_schema() -> anyhow::Result<Schema> {
    let schema_path = std::env::var("SCHEMA_PATH")?;
    let schema_file = File::open(schema_path)?;
    let schema = Schema::from_json(schema_file)?;
    Ok(schema)
}

pub fn read_pattern_meta() -> anyhow::Result<PatternMeta> {
    Ok(PatternMeta::from(read_schema()?))
}

pub fn read_catalogue() -> anyhow::Result<Catalogue> {
    let catalog_path = std::env::var("CATALOG_PATH")?;
    let catalog = Catalogue::import(catalog_path)?;
    Ok(catalog)
}

pub fn read_graph() -> anyhow::Result<LargeGraphDB<DefaultId, InternalId>> {
    let graph_path = std::env::var("GRAPH_PATH")?;
    read_graph_from_path(graph_path)
}

pub fn read_graphs() -> anyhow::Result<Vec<LargeGraphDB<DefaultId, InternalId>>> {
    let graphs_path = std::env::var("GRAPHS_PATH")?;
    let mut graphs = vec![];
    for graph_path_entry in read_dir(graphs_path)? {
        let graph_path = graph_path_entry?.path();
        graphs.push(read_graph_from_path(graph_path)?)
    }
    Ok(graphs)
}

pub fn read_sample_graph() -> anyhow::Result<LargeGraphDB<DefaultId, InternalId>> {
    let sample_graph_path = std::env::var("SAMPLE_PATH")?;
    read_graph_from_path(sample_graph_path)
}

fn read_graph_from_path<P: AsRef<Path>>(
    graph_path: P,
) -> anyhow::Result<LargeGraphDB<DefaultId, InternalId>> {
    let graph = GraphDBConfig::default()
        .root_dir(&graph_path)
        .partition(1)
        .schema_file(
            graph_path
                .as_ref()
                .join(DIR_GRAPH_SCHEMA)
                .join(FILE_SCHEMA),
        )
        .open()
        .map_err(|_| IrError::MissingData("Sample Graph Open Failture".to_string()))?;
    Ok(graph)
}

pub fn read_pattern() -> anyhow::Result<Pattern> {
    let pattern_path = std::env::var("PATTERN_PATH")?;
    read_pattern_from_path(pattern_path)
}

pub fn read_patterns() -> anyhow::Result<Vec<Pattern>> {
    let patterns_path = std::env::var("PATTERNS_PATH")?;
    let mut patterns = vec![];
    for pattern_path in read_to_string(patterns_path)?.split('\n') {
        let pattern = read_pattern_from_path(pattern_path)?;
        patterns.push(pattern);
    }
    Ok(patterns)
}

pub fn read_pattern_from_path<P: AsRef<Path>>(pattern_path: P) -> anyhow::Result<Pattern> {
    let pattern = PatternWithCount::import(pattern_path)?;
    Ok(pattern.try_into()?)
}

fn initialize_job_assembly() -> IRJobAssembly {
    let query_exp_graph = QueryExpGraph::new(1);
    query_exp_graph.initialize_job_assembly()
}
