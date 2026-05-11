use std::sync::Arc;

use rayon::iter::{IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator};

use crate::common::{DefaultVertexId, EdgeDirection, LabelId, VertexId};
use crate::factorization::{ColumnGroup, SingleColumnGroup, Table};
use crate::graph::{LabeledGraph, LabeledVertex};
use crate::pattern::{GraphPattern, PathPattern};

#[derive(Debug)]
pub struct PathSampler {
    graph: Arc<LabeledGraph>,
}

impl PathSampler {
    pub fn new(graph: Arc<LabeledGraph>) -> Self {
        PathSampler { graph }
    }

    pub fn sample(&self, path: &PathPattern) -> Table {
        match path.len() {
            0 => self.sample_0(path),
            1 => self.sample_1(path),
            2 => self.sample_2(path),
            _ => todo!(),
        }
    }

    pub fn extend<const FROM_END: bool>(
        &self,
        base_path: &PathPattern,
        base_table: &Table,
        new_path: &PathPattern,
    ) -> Table {
        let graph = self.graph.clone();
        let extend_start = if FROM_END {
            base_path.end()
        } else {
            base_path.start()
        };
        let extend_end = if FROM_END {
            new_path.end()
        } else {
            new_path.start()
        };
        let extend_edge = if FROM_END {
            new_path.edges().last().unwrap()
        } else {
            new_path.edges().first().unwrap()
        };
        let direction = if FROM_END {
            new_path.directions().last().copied().unwrap()
        } else {
            new_path.directions().first().unwrap().reverse()
        };
        assert_eq!(base_table.num_tags(), base_path.len() + 1);

        let column = base_table.get_column(extend_start.tag_id()).unwrap();
        let mut new_column: Vec<_> = column.values().par_iter().copied().collect();

        new_column
            .par_iter_mut()
            .filter(|id| id.is_valid())
            .for_each(|id| {
                let vertex = LabeledVertex::new(*id, extend_start.label_id());
                let new_id = graph
                    .neighbors(vertex, extend_edge.label_id(), direction)
                    .unwrap()
                    .first();
                *id = if let Some(new_id) = new_id {
                    *new_id
                } else {
                    DefaultVertexId::invalid()
                };
            });

        let new_column = Arc::new(new_column);

        let mut table = base_table.clone();
        let (group_id, _) = table.get_column_pos(extend_start.tag_id()).unwrap();
        let column_id = table.add_column(group_id, new_column);
        table.add_tag(extend_end.tag_id(), group_id, column_id);
        table
    }

    fn sample_0(&self, path: &PathPattern) -> Table {
        assert_eq!(path.len(), 0);
        let start = path.start();
        let mut group = SingleColumnGroup::single();
        group.par_extend(
            self.graph
                .vertices(start.label_id())
                .unwrap()
                .par_iter()
                .copied(),
        );
        let group = ColumnGroup::from(group);
        let mut table = Table::default();
        table.add_group(group);
        table.add_tag(start.tag_id(), 0, 0);
        table
    }

    fn sample_1_inner(
        &self,
        start_label: LabelId,
        edge_label_id: LabelId,
        direction: EdgeDirection,
    ) -> (ColumnGroup, ColumnGroup) {
        let mut start_column = SingleColumnGroup::single();
        let mut end_column = SingleColumnGroup::multiple();
        start_column.par_extend(
            self.graph
                .vertices(start_label)
                .unwrap()
                .par_iter()
                .copied(),
        );
        end_column.par_extend_from_segments(
            self.graph
                .vertices(start_label)
                .unwrap()
                .par_iter()
                .map(|start_id| {
                    let start_vertex = LabeledVertex::new(*start_id, start_label);
                    self.graph
                        .neighbors(start_vertex, edge_label_id, direction)
                        .unwrap()
                }),
        );
        (start_column.into(), end_column.into())
    }

    fn sample_1(&self, path: &PathPattern) -> Table {
        assert_eq!(path.len(), 1);
        let start = path.start();
        let end = path.end();
        let direction = *path.directions().first().unwrap();
        let edge = path.edges().first().unwrap();
        let edge_label_id = edge.label_id();
        let mut table = Table::default();

        if self.graph.vertices(start.label_id()).unwrap().len()
            < self.graph.vertices(end.label_id()).unwrap().len()
        {
            let (start_group, end_group) =
                self.sample_1_inner(start.label_id(), edge_label_id, direction);
            table.add_group(start_group);
            table.add_group(end_group);
        } else {
            let (end_group, start_group) =
                self.sample_1_inner(end.label_id(), edge_label_id, direction.reverse());
            table.add_group(start_group);
            table.add_group(end_group);
        };
        table.add_tag(start.tag_id(), 0, 0);
        table.add_tag(end.tag_id(), 1, 0);
        table
    }

    fn sample_2(&self, path: &PathPattern) -> Table {
        assert_eq!(path.len(), 2);
        let start = path.start();
        let end = path.end();
        let [first_edge, second_edge] = path.edges().first_chunk().unwrap();
        let [first_direction, second_direction] = path.directions().first_chunk().unwrap();
        let mid = match first_direction {
            EdgeDirection::Out => path.get_vertex(first_edge.dst()).unwrap(),
            EdgeDirection::In => path.get_vertex(first_edge.src()).unwrap(),
        };
        let mut mid_column = SingleColumnGroup::single();
        mid_column.par_extend(
            self.graph
                .vertices(mid.label_id())
                .unwrap()
                .par_iter()
                .copied(),
        );

        let mut start_column = SingleColumnGroup::multiple();
        let mut end_column = SingleColumnGroup::multiple();

        start_column.par_extend_from_segments(
            self.graph
                .vertices(mid.label_id())
                .unwrap()
                .par_iter()
                .map(|mid_id| {
                    let mid_vertex = LabeledVertex::new(*mid_id, mid.label_id());
                    let start_column = self
                        .graph
                        .neighbors(mid_vertex, first_edge.label_id(), first_direction.reverse())
                        .unwrap();
                    start_column
                }),
        );
        end_column.par_extend_from_segments(
            self.graph
                .vertices(mid.label_id())
                .unwrap()
                .par_iter()
                .map(|mid_id| {
                    let mid_vertex = LabeledVertex::new(*mid_id, mid.label_id());
                    let end_column = self
                        .graph
                        .neighbors(mid_vertex, second_edge.label_id(), *second_direction)
                        .unwrap();
                    end_column
                }),
        );

        let mut table = Table::default();
        table.add_group(start_column.into());
        table.add_group(mid_column.into());
        table.add_group(end_column.into());
        table.add_tag(start.tag_id(), 0, 0);
        table.add_tag(mid.tag_id(), 1, 0);
        table.add_tag(end.tag_id(), 2, 0);
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::RawPattern;
    use crate::test_utils::build_ldbc_graph;

    fn build_path(len: u8) -> PathPattern {
        let mut raw = RawPattern::new();
        for i in 0..=len {
            raw.push_back_vertex((i, 6));
        }
        for i in 0..len {
            raw.push_back_edge((i, i, i + 1, 14));
        }
        raw.to_path().unwrap()
    }

    #[test]
    fn test_sample_0() {
        let path = build_path(0);
        let graph = build_ldbc_graph();
        let sampler = PathSampler::new(Arc::new(graph));
        let table = sampler.sample(&path);
        assert_eq!(table.count(), 50);
    }

    #[test]
    fn test_sample_1() {
        let path = build_path(1);
        let graph = build_ldbc_graph();
        let sampler = PathSampler::new(Arc::new(graph));
        let table = sampler.sample(&path);
        assert_eq!(table.count(), 88);
    }

    #[test]
    fn test_sample_2() {
        let path = build_path(2);
        let graph = build_ldbc_graph();
        let sampler = PathSampler::new(Arc::new(graph));
        let table = sampler.sample(&path);
        assert_eq!(table.count(), 246);
    }

    #[test]
    fn test_extend() {
        let path = build_path(2);
        let graph = build_ldbc_graph();
        let sampler = PathSampler::new(Arc::new(graph));
        let table = sampler.sample(&path);
        let new_path = RawPattern::from(&path)
            .push_back_vertex((3, 0))
            .push_back_edge((2, 2, 3, 13))
            .to_path()
            .unwrap();
        let table = sampler.extend::<true>(&path, &table, &new_path);
        assert_eq!(table.count(), 246);
    }
}
