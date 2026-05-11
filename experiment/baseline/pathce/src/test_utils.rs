use std::fs::File;
use std::path::PathBuf;

use ahash::HashMapExt;

use crate::common::GlobalBucketMap;
use crate::graph::LabeledGraph;
use crate::schema::Schema;

pub fn build_ldbc_schema() -> Schema {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/ldbc_pathce_schema.json");
    let file = File::open(path).unwrap();
    serde_json::from_reader(file).unwrap()
}

pub fn build_ldbc_graph() -> LabeledGraph {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/sf0.003");
    let schema = build_ldbc_schema();
    LabeledGraph::from_csv(path, &schema, b',', 4).unwrap()
}

pub fn build_bucket_map(graph: &LabeledGraph, buckets: usize) -> GlobalBucketMap {
    let mut global_bucket_map = GlobalBucketMap::new();
    for vlabel in graph.vertex_labels() {
        let local_bucket_map = graph
            .vertices(vlabel)
            .unwrap()
            .iter()
            .map(|vid| (*vid, vid % buckets))
            .collect();
        global_bucket_map.insert(vlabel, local_bucket_map);
    }
    global_bucket_map
}
