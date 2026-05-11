use std::sync::Arc;

use itertools::Itertools;
use rayon::iter::{
    IndexedParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator,
};
use rayon::ThreadPool;

use crate::common::{EdgeDirection, LabelId};
use crate::graph::{LabeledGraph, LabeledVertex};
use crate::pattern::{GeneralPattern, GraphPattern, PathPattern};
use crate::statistics::CountVec;

pub struct PathCounter {
    graph: Arc<LabeledGraph>,
    pool: Arc<ThreadPool>,
}

impl PathCounter {
    pub fn new(graph: Arc<LabeledGraph>, pool: Arc<ThreadPool>) -> Self {
        Self { graph, pool }
    }

    pub fn count(&self, path: &PathPattern) -> u128 {
        self.pool.scope(|_| {
            let mut count_vec = self.count_vertex(path.start().label_id());
            for (e, d) in path.edges().iter().zip_eq(path.directions()) {
                let (source_tag_id, neighbor_tag_id, direction) = match d {
                    EdgeDirection::Out => (e.dst(), e.src(), EdgeDirection::In),
                    EdgeDirection::In => (e.src(), e.dst(), EdgeDirection::Out),
                };
                let source_label_id = path.get_vertex(source_tag_id).unwrap().label_id();
                let neighbor_label_id = path.get_vertex(neighbor_tag_id).unwrap().label_id();
                let new_count_vec = self.count_edge(
                    source_label_id,
                    e.label_id(),
                    neighbor_label_id,
                    direction,
                    count_vec,
                );
                count_vec = new_count_vec;
            }
            count_vec.as_ref().par_iter().sum()
        })
    }

    fn count_vertex(&self, vertex_label_id: LabelId) -> CountVec<u128> {
        let vertex_map = self.graph.get_internal_vertex_map(vertex_label_id).unwrap();
        CountVec::with_value(1, vertex_map.len())
    }

    fn count_edge(
        &self,
        vertex_label_id: LabelId,
        edge_label_id: LabelId,
        neighbor_label_id: LabelId,
        direction: EdgeDirection,
        count_vec: CountVec<u128>,
    ) -> CountVec<u128> {
        let vertex_map = self.graph.get_internal_vertex_map(vertex_label_id).unwrap();
        let neighbor_vertex_map = self
            .graph
            .get_internal_vertex_map(neighbor_label_id)
            .unwrap();
        let mut new_count_vec = CountVec::zeroed(vertex_map.len());
        new_count_vec
            .as_mut()
            .par_iter_mut()
            .enumerate()
            .for_each(|(internal_id, count)| {
                let vertex_id = *vertex_map.get_by_right(&(internal_id as u32)).unwrap();
                let neighbors = self
                    .graph
                    .neighbors(
                        LabeledVertex::new(vertex_id, vertex_label_id),
                        edge_label_id,
                        direction,
                    )
                    .unwrap();
                *count = neighbors
                    .par_iter()
                    .map(|neighbor_id| {
                        let neighbor_internal_id =
                            *neighbor_vertex_map.get_by_left(neighbor_id).unwrap();
                        count_vec
                            .as_ref()
                            .get(neighbor_internal_id as usize)
                            .unwrap()
                    })
                    .sum()
            });
        new_count_vec
    }
}

pub struct StarCounter {
    graph: Arc<LabeledGraph>,
    pool: Arc<ThreadPool>,
}

impl StarCounter {
    pub fn new(graph: Arc<LabeledGraph>, pool: Arc<ThreadPool>) -> Self {
        Self { graph, pool }
    }

    pub fn count(&self, star: &GeneralPattern) -> u128 {
        let (center,) = star
            .vertices()
            .iter()
            .filter(|v| star.get_vertex_degree(v.tag_id()).unwrap() > 1)
            .collect_tuple()
            .unwrap();
        self.pool.scope(|_| {
            self.graph
                .vertices(center.label_id())
                .unwrap()
                .par_iter()
                .map(|v| {
                    star.adjacencies(center.tag_id())
                        .unwrap()
                        .map(|adj| match adj.direction() {
                            EdgeDirection::Out => self
                                .graph
                                .outgoing_degree(
                                    LabeledVertex::new(*v, center.label_id()),
                                    adj.edge_label_id(),
                                )
                                .unwrap() as u128,
                            EdgeDirection::In => self
                                .graph
                                .incoming_degree(
                                    LabeledVertex::new(*v, center.label_id()),
                                    adj.edge_label_id(),
                                )
                                .unwrap() as u128,
                        })
                        .product::<u128>()
                })
                .sum()
        })
    }
}

#[cfg(test)]
mod tests {
    use rayon::ThreadPoolBuilder;

    use super::*;
    use crate::pattern::RawPattern;
    use crate::test_utils::build_ldbc_graph;

    #[test]
    fn test_count_vertex() {
        let graph = build_ldbc_graph();
        let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let counter = PathCounter::new(Arc::new(graph), Arc::new(pool));
        let p = RawPattern::new()
            .push_back_vertex((0, 6))
            .to_path()
            .unwrap();
        assert_eq!(counter.count(&p), 50);
    }

    #[test]
    fn test_count_path() {
        let graph = build_ldbc_graph();
        let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let counter = PathCounter::new(Arc::new(graph), Arc::new(pool));
        let p = RawPattern::with_vertices_edges(
            [(0, 6), (1, 6), (2, 6), (3, 6)],
            [(0, 0, 1, 14), (1, 2, 1, 14), (2, 3, 2, 14)],
        )
        .to_path()
        .unwrap();
        assert_eq!(counter.count(&p), 1185);
    }
}
