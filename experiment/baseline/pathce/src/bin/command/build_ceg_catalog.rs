use std::path::PathBuf;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use clap::Args;
use csv::StringRecord;
use pathce::common::{EdgeDirection, LabelId, TagId};
use pathce::counter::PathCounter;
use pathce::graph::LabeledGraph;
use pathce::pattern::{PathPattern, PatternEdge, PatternVertex, RawPattern};
use pathce::schema::Schema;
use itertools::Itertools;
use rayon::ThreadPoolBuilder;

#[derive(Debug, Args)]
pub struct BuildCegCatalogArgs {
    /// Specify the graph path.
    #[arg(short, long, value_name = "GRAPH_FILE")]
    graph: PathBuf,
    /// Specify the schema path.
    #[arg(short, long, value_name = "SCHEMA_FILE")]
    schema: PathBuf,
    /// Specify the decom file.
    #[arg(short, long, value_name = "DECOM")]
    decom: PathBuf,
    /// Specify the number of threads.
    #[arg(short, long, default_value = "4")]
    threads: usize,
    /// Specify the output path.
    #[arg(short, long)]
    output: PathBuf,
}

fn parse_record(schema: &Schema, record: &StringRecord) -> PathPattern {
    let edges = record
        .get(1)
        .unwrap()
        .split(";")
        .map(|edge| {
            let (src, dst) = edge.split("-").collect_tuple().unwrap();
            let src: TagId = src.parse().unwrap();
            let dst: TagId = dst.parse().unwrap();
            (src, dst)
        })
        .collect_vec();
    let edge_labels: Vec<LabelId> = record
        .get(2)
        .unwrap()
        .split("->")
        .map(|label| label.parse().unwrap())
        .collect_vec();
    let edges: HashMap<_, _> = edges
        .into_iter()
        .zip_eq(edge_labels)
        .enumerate()
        .map(|(i, ((src, dst), l))| (i as TagId, PatternEdge::new(i as _, src, dst, l)))
        .collect();
    let mut vertices = HashMap::new();
    let mut adj_map: HashMap<_, Vec<_>> = HashMap::new();
    for e in edges.values() {
        adj_map
            .entry(e.src())
            .or_default()
            .push((e.tag_id(), EdgeDirection::Out));
        adj_map
            .entry(e.dst())
            .or_default()
            .push((e.tag_id(), EdgeDirection::In));
        let schema_edge = schema.get_edge(e.label_id()).unwrap();
        let src_label_id = schema_edge.from;
        let dst_label_id = schema_edge.to;
        vertices
            .entry(e.src())
            .or_insert_with(|| PatternVertex::new(e.src(), src_label_id));
        vertices
            .entry(e.dst())
            .or_insert_with(|| PatternVertex::new(e.dst(), dst_label_id));
    }
    let mut raw = RawPattern::new();
    let (start, _) = adj_map.iter().find(|(_, adj)| adj.len() == 1).unwrap();
    raw.push_back_vertex(*vertices.get(start).unwrap());
    let mut current = *start;
    let mut added_vertices = HashSet::new();
    added_vertices.insert(current);
    while let Some((edge_tag_id, direction)) =
        adj_map
            .get(&current)
            .unwrap()
            .iter()
            .find(|(edge_tag_id, direction)| {
                let edge = edges.get(edge_tag_id).unwrap();
                match direction {
                    EdgeDirection::Out => !added_vertices.contains(&edge.dst()),
                    EdgeDirection::In => !added_vertices.contains(&edge.src()),
                }
            })
    {
        let edge = edges.get(edge_tag_id).unwrap();
        raw.push_back_edge(*edge);
        let next_vertex_tag_id = match direction {
            EdgeDirection::Out => edge.dst(),
            EdgeDirection::In => edge.src(),
        };
        let next_vertex = vertices.get(&next_vertex_tag_id).unwrap();
        raw.push_back_vertex(*next_vertex);
        added_vertices.insert(next_vertex_tag_id);
        current = next_vertex_tag_id;
    }
    raw.to_path().unwrap()
}

pub fn build_ceg_catalog(args: BuildCegCatalogArgs) {
    let graph = Arc::new(LabeledGraph::import_bincode(args.graph).unwrap());
    let pool = Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build()
            .unwrap(),
    );
    let counter = PathCounter::new(graph, pool);
    let schema = Schema::import_json(args.schema).unwrap();
    let records = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(args.decom)
        .unwrap()
        .into_records()
        .map(|record| {
            let mut record = record.unwrap();
            let path = parse_record(&schema, &record);
            let count = counter.count(&path);
            record.push_field(&count.to_string());
            record
        })
        .collect_vec();
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_path(args.output)
        .unwrap();
    for record in records {
        writer.write_record(&record).unwrap();
    }
}
