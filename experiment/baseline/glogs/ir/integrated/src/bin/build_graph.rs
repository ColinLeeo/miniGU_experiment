use std::fs::{create_dir_all, File};
use std::path::{Path, PathBuf};
use std::time::Instant;

use csv::ReaderBuilder as CsvReaderBuilder;
use graph_store::common::{DefaultId, InternalId, INVALID_LABEL_ID};
use graph_store::config::{GraphDBConfig, JsonConf};
use graph_store::graph_db::{GlobalStoreUpdate, MutableGraphDB};
use graph_store::schema::LDBCGraphSchema;
use ir_core::catalogue::pattern_meta::PatternMeta;
use ir_core::catalogue::PatternLabelId;
use ir_core::plan::meta::Schema;
use ir_core::JsonIO;
use log::info;
use structopt::StructOpt;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[derive(StructOpt)]
pub struct Config {
    #[structopt(long = "schema1")]
    schema: PathBuf,
    #[structopt(long = "schema2")]
    storage_schema: PathBuf,
    #[structopt(short = "d", long = "data_dir")]
    data_dir: PathBuf,
    #[structopt(short = "o", long = "output_dir")]
    output_dir: PathBuf,
    #[structopt(long = "delimiter", default_value = ",")]
    delimiter: char,
}

fn read_vertex(
    graph: &mut MutableGraphDB<DefaultId, InternalId>, vertex_label_id: PatternLabelId, path: &Path,
    delimiter: u8,
) -> anyhow::Result<()> {
    let mut reader = CsvReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)?;
    for record in reader.records() {
        let record = record?;
        let global_id = record.get(0).unwrap().parse()?;
        assert!(graph.add_vertex(global_id, [vertex_label_id as u8, INVALID_LABEL_ID]));
    }
    Ok(())
}

fn read_edge(
    graph: &mut MutableGraphDB<DefaultId, InternalId>, edge_label_id: PatternLabelId, path: &Path,
    delimiter: u8,
) -> anyhow::Result<()> {
    let mut reader = CsvReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)?;
    for record in reader.records() {
        let record = record?;
        let global_src_id = record.get(0).unwrap().parse()?;
        let global_dst_id = record.get(1).unwrap().parse()?;
        assert!(graph.add_edge(global_src_id, global_dst_id, edge_label_id as u8));
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = Config::from_args();
    let schema = Schema::from_json(File::open(config.schema)?)?;
    let storage_schema = LDBCGraphSchema::from_json_file(config.storage_schema)?;
    let pattern_meta = PatternMeta::from(schema);
    let mut graph: MutableGraphDB<DefaultId, InternalId> = GraphDBConfig::default()
        .root_dir(&config.output_dir)
        .new();
    for vertex_label_id in pattern_meta.vertex_label_ids_iter() {
        let vertex_label_name = pattern_meta
            .get_vertex_label_name(vertex_label_id)
            .unwrap();
        let path = config
            .data_dir
            .join(format!("{vertex_label_name}.csv"));
        let start = Instant::now();
        read_vertex(&mut graph, vertex_label_id, &path, config.delimiter as u8)?;
        let time = start.elapsed().as_secs_f64();
        info!("reading {:?} time: {} s", path, time);
    }
    for edge_label_id in pattern_meta.edge_label_ids_iter() {
        let edge_label_name = pattern_meta
            .get_edge_label_name(edge_label_id)
            .unwrap();
        let path = config
            .data_dir
            .join(format!("{edge_label_name}.csv"));
        let start = Instant::now();
        read_edge(&mut graph, edge_label_id, &path, config.delimiter as u8)?;
        let time = start.elapsed().as_secs_f64();
        info!("reading {:?} time: {} s", path, time);
    }
    graph.export().unwrap();

    let schema_dir = config.output_dir.join("graph_schema");
    create_dir_all(&schema_dir)?;
    storage_schema.to_json_file(schema_dir.join("schema.json"))?;
    Ok(())
}
