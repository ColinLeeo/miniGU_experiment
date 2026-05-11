use std::collections::hash_map::Entry;

use ahash::HashMap;

use super::Catalog;
use crate::common::{LabelId, TagId};
use crate::pattern::{GeneralPattern, GraphPattern, PathPattern};

#[derive(Debug, Default)]
pub struct MockCatalog {
    paths: Vec<PathPattern>,
    stars: Vec<GeneralPattern>,
    path_label_map: HashMap<Vec<u8>, LabelId>,
    star_label_map: HashMap<(TagId, Vec<u8>), LabelId>,
    edge_count_map: HashMap<LabelId, usize>,
}

impl MockCatalog {
    pub fn add_edge_count(&mut self, edge_label_id: LabelId, count: usize) {
        self.edge_count_map.insert(edge_label_id, count);
    }

    pub fn add_path(&mut self, path: PathPattern) -> LabelId {
        match self.path_label_map.entry(path.encode()) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let label_id = self.paths.len() as LabelId;
                entry.insert(label_id);
                self.paths.push(path);
                label_id
            }
        }
    }

    pub fn add_star(&mut self, star: GeneralPattern, rank: TagId) -> LabelId {
        match self.star_label_map.entry((rank, star.encode())) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let label_id = self.stars.len() as LabelId;
                entry.insert(label_id);
                self.stars.push(star);
                label_id
            }
        }
    }
}

impl Catalog for MockCatalog {
    fn get_path_label_id(&self, code: &[u8]) -> Option<LabelId> {
        self.path_label_map.get(code).copied()
    }

    fn get_path(&self, label_id: LabelId) -> Option<&PathPattern> {
        self.paths.get(label_id as usize)
    }

    fn get_star_label_id(&self, rank: TagId, code: &[u8]) -> Option<LabelId> {
        self.star_label_map.get(&(rank, code.to_owned())).copied()
    }

    fn get_star(&self, label_id: LabelId) -> Option<&GeneralPattern> {
        self.stars.get(label_id as usize)
    }

    fn get_edge_count(&self, label_id: LabelId) -> Option<usize> {
        self.edge_count_map.get(&label_id).copied()
    }
}
