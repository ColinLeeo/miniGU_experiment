use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use csv::ReaderBuilder;
use itertools::Itertools;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::slice::ParallelSliceMut;
use rayon::{ThreadPool, ThreadPoolBuilder};
use serde::{Deserialize, Serialize};

use self::csr::{BidirectionalCsr, Csr};
use crate::common::{
    DefaultVertexId, EdgeDirection, InternalId, InternalVertexMap, LabelId, VertexId,
};
use crate::error::{GCardError, GCardResult};
use crate::schema::Schema;

mod csr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabeledVertex {
    pub id: DefaultVertexId,
    pub label_id: LabelId,
}

impl LabeledVertex {
    pub fn new(id: DefaultVertexId, label_id: LabelId) -> Self {
        Self { id, label_id }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LabeledGraph {
    vertex_map: HashMap<LabelId, InternalVertexMap>,
    vertices: HashMap<LabelId, Vec<DefaultVertexId>>,
    csr: HashMap<LabelId, BidirectionalCsr>,
}

impl LabeledGraph {
    pub fn get_num_edges(&self, label_id: LabelId) -> Option<usize> {
        self.csr.get(&label_id).map(|csr| csr.get_num_edges())
    }

    pub fn get_internal_vertex_map(&self, label_id: LabelId) -> Option<&InternalVertexMap> {
        self.vertex_map.get(&label_id)
    }

    pub fn vertex_labels(&self) -> impl Iterator<Item = LabelId> + Clone + '_ {
        self.vertex_map.keys().copied()
    }

    pub fn edge_labels(&self) -> impl Iterator<Item = LabelId> + Clone + '_ {
        self.csr.keys().copied()
    }

    pub fn vertices(&self, label_id: LabelId) -> Option<&[DefaultVertexId]> {
        let vertices = self.vertices.get(&label_id)?;
        Some(vertices)
    }

    pub fn neighbors(
        &self,
        vertex: LabeledVertex,
        edge_label_id: LabelId,
        direction: EdgeDirection,
    ) -> Option<&[DefaultVertexId]> {
        match direction {
            EdgeDirection::Out => self.outgoing_neighbors(vertex, edge_label_id),
            EdgeDirection::In => self.incoming_neighbors(vertex, edge_label_id),
        }
    }

    pub fn outgoing_neighbors(
        &self,
        vertex: LabeledVertex,
        edge_label_id: LabelId,
    ) -> Option<&[DefaultVertexId]> {
        let LabeledVertex { id, label_id } = vertex;
        let vertex_map = self.vertex_map.get(&label_id)?;
        let vertex_id = vertex_map.get_by_left(&id)?;
        let csr = self.csr.get(&edge_label_id)?;
        Some(csr.outgoing_neighbors(*vertex_id))
    }

    pub fn outgoing_degree(&self, vertex: LabeledVertex, edge_label_id: LabelId) -> Option<usize> {
        self.outgoing_neighbors(vertex, edge_label_id)
            .map(<[DefaultVertexId]>::len)
    }

    pub fn incoming_neighbors(
        &self,
        vertex: LabeledVertex,
        edge_label_id: LabelId,
    ) -> Option<&[DefaultVertexId]> {
        let LabeledVertex { id, label_id } = vertex;
        let vertex_map = self.vertex_map.get(&label_id)?;
        let vertex_id = vertex_map.get_by_left(&id)?;
        let csr = self.csr.get(&edge_label_id)?;
        Some(csr.incoming_neighbors(*vertex_id))
    }

    pub fn incoming_degree(&self, vertex: LabeledVertex, edge_label_id: LabelId) -> Option<usize> {
        self.incoming_neighbors(vertex, edge_label_id)
            .map(<[DefaultVertexId]>::len)
    }
}

#[derive(Debug, Clone)]
pub struct LabeledGraphBuilder {
    vertices: HashMap<LabelId, Vec<DefaultVertexId>>,
    edges: HashMap<LabelId, Vec<(DefaultVertexId, DefaultVertexId)>>,
    edge_label_to_vertex_label: HashMap<LabelId, (LabelId, LabelId)>,
    num_threads: usize,
}

impl LabeledGraphBuilder {
    pub fn new(num_threads: usize) -> Self {
        Self {
            vertices: HashMap::new(),
            edges: HashMap::new(),
            edge_label_to_vertex_label: HashMap::new(),
            num_threads,
        }
    }

    pub fn add_vertex_label(mut self, label_id: LabelId) -> Self {
        self.vertices.entry(label_id).or_default();
        self
    }

    pub fn add_edge_label(
        mut self,
        label_id: LabelId,
        src_vertex_label: LabelId,
        dst_vertex_label: LabelId,
    ) -> Self {
        self.edges.entry(label_id).or_default();
        self.edge_label_to_vertex_label
            .entry(label_id)
            .or_insert((src_vertex_label, dst_vertex_label));
        self
    }

    pub fn add_vertex(mut self, vertex_id: DefaultVertexId, label_id: LabelId) -> Self {
        self.vertices.entry(label_id).or_default().push(vertex_id);
        self
    }

    pub fn add_edge(
        mut self,
        src_id: DefaultVertexId,
        dst_id: DefaultVertexId,
        label_id: LabelId,
    ) -> Self {
        self.edges
            .entry(label_id)
            .or_default()
            .push((src_id, dst_id));
        self
    }

    pub fn build(self) -> GCardResult<LabeledGraph> {
        let pool = ThreadPoolBuilder::new()
            .num_threads(self.num_threads)
            .build()?;
        let vertices = self.vertices.clone();
        let vertex_map = pool.scope(|_| {
            self.vertices
                .into_par_iter()
                .map(|(label_id, vertices)| Ok((label_id, build_internal_vertex_map(vertices)?)))
                .try_fold(
                    HashMap::new,
                    |mut vertex_map, local_result: GCardResult<(_, _)>| -> GCardResult<_> {
                        let (label_id, local_vertex_map) = local_result?;
                        vertex_map.insert(label_id, local_vertex_map);
                        Ok(vertex_map)
                    },
                )
                .try_reduce(HashMap::new, |mut vertex_map, partial_vertex_map| {
                    vertex_map.extend(partial_vertex_map);
                    Ok(vertex_map)
                })
        })?;
        let csr = self
            .edges
            .into_iter()
            .map(|(label_id, edges)| -> GCardResult<_> {
                let (src_label, dst_label) = self
                    .edge_label_to_vertex_label
                    .get(&label_id)
                    .ok_or_else(|| {
                        let err = format!("cannot find src and dst label of edge label {label_id}");
                        GCardError::Graph(err)
                    })?;
                let src_vertex_map = vertex_map.get(src_label).ok_or_else(|| {
                    let err = format!("cannot find vertex map of vertex label {src_label}");
                    GCardError::Graph(err)
                })?;
                let dst_vertex_map = vertex_map.get(dst_label).ok_or_else(|| {
                    let err = format!("cannot find vertex map of vertex label {dst_label}");
                    GCardError::Graph(err)
                })?;
                Ok((
                    label_id,
                    build_bidirectional_csr(edges, src_vertex_map, dst_vertex_map, &pool)?,
                ))
            })
            .try_collect()?;
        Ok(LabeledGraph {
            vertex_map,
            vertices,
            csr,
        })
    }
}

fn build_internal_vertex_map(vertices: Vec<DefaultVertexId>) -> GCardResult<InternalVertexMap> {
    let mut internal_vertex_map = InternalVertexMap::new();
    for (internal_id, vertex_id) in vertices.into_iter().enumerate() {
        if !vertex_id.is_valid() {
            let err = format!("invalid vertex id: {vertex_id}");
            return Err(GCardError::Graph(err));
        }
        if internal_vertex_map
            .insert(vertex_id, internal_id as InternalId)
            .did_overwrite()
        {
            let err = format!("duplicate vertex id: {vertex_id} found in the csv");
            return Err(GCardError::Graph(err));
        }
    }
    Ok(internal_vertex_map)
}

fn build_bidirectional_csr(
    edges: Vec<(DefaultVertexId, DefaultVertexId)>,
    src_vertex_map: &InternalVertexMap,
    dst_vertex_map: &InternalVertexMap,
    pool: &ThreadPool,
) -> GCardResult<BidirectionalCsr> {
    let (mut fes, mut bes) = pool.scope(|_| {
        edges
            .into_par_iter()
            .map(|(src, dst)| -> GCardResult<_> {
                let src_internal = src_vertex_map.get_by_left(&src).copied().ok_or_else(|| {
                    let err = format!("cannot find vertex {src} in the vertex map");
                    GCardError::Graph(err)
                })?;
                let dst_internal = dst_vertex_map.get_by_left(&dst).copied().ok_or_else(|| {
                    let err = format!("cannot find vertex {dst} in the vertex map");
                    GCardError::Graph(err)
                })?;
                Ok(((src_internal, dst), (dst_internal, src)))
            })
            .try_fold(
                || (Vec::new(), Vec::new()),
                |(mut fes, mut bes), e: GCardResult<(_, _)>| -> GCardResult<_> {
                    let (fe, be) = e?;
                    fes.push(fe);
                    bes.push(be);
                    Ok((fes, bes))
                },
            )
            .try_reduce(
                || (Vec::new(), Vec::new()),
                |(mut fes, mut bes), (partial_fes, partial_bes)| -> GCardResult<_> {
                    fes.extend(partial_fes);
                    bes.extend(partial_bes);
                    Ok((fes, bes))
                },
            )
    })?;
    pool.scope(|_| {
        fes.as_mut_slice().par_sort_unstable();
        bes.as_mut_slice().par_sort_unstable();
    });
    let src_max_internal_id = src_vertex_map.right_values().max().copied().unwrap();
    let dst_max_internal_id = dst_vertex_map.right_values().max().copied().unwrap();
    let forward = Csr::from_sorted_edges(src_max_internal_id, &fes)?;
    let backward = Csr::from_sorted_edges(dst_max_internal_id, &bes)?;
    Ok(BidirectionalCsr::new(forward, backward))
}

fn read_vertices_from_csv<P: AsRef<Path>>(
    builder: LabeledGraphBuilder,
    label_id: LabelId,
    path: P,
    delimiter: u8,
) -> GCardResult<LabeledGraphBuilder> {
    let mut reader = ReaderBuilder::new().delimiter(delimiter).from_path(path)?;
    reader
        .records()
        .enumerate()
        .try_fold(builder, |builder, (line, record)| {
            let record = record?;
            let vertex_id = record
                .get(0)
                .ok_or_else(|| {
                    let err = format!("expect vertex id in line {line}");
                    GCardError::Graph(err)
                })?
                .parse::<DefaultVertexId>()
                .map_err(|e| GCardError::Graph(e.to_string()))?;
            Ok(builder.add_vertex(vertex_id, label_id))
        })
}

fn read_edges_from_csv<P: AsRef<Path>>(
    builder: LabeledGraphBuilder,
    label_id: LabelId,
    path: P,
    delimiter: u8,
) -> GCardResult<LabeledGraphBuilder> {
    let mut reader = ReaderBuilder::new().delimiter(delimiter).from_path(path)?;
    reader
        .records()
        .enumerate()
        .try_fold(builder, |builder, (line, record)| {
            let record = record?;
            let src = record
                .get(0)
                .ok_or_else(|| {
                    let err = format!("expect src vertex id in line {line}");
                    GCardError::Graph(err)
                })?
                .parse::<DefaultVertexId>()
                .map_err(|e| GCardError::Graph(e.to_string()))?;
            let dst = record
                .get(1)
                .ok_or_else(|| {
                    let err = format!("expect dst vertex id in line {line}");
                    GCardError::Graph(err)
                })?
                .parse::<DefaultVertexId>()
                .map_err(|e| GCardError::Graph(e.to_string()))?;
            Ok(builder.add_edge(src, dst, label_id))
        })
}

impl LabeledGraph {
    pub fn export_bincode<P: AsRef<Path>>(&self, path: P) -> GCardResult<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, self)?;
        Ok(())
    }

    pub fn import_bincode<P: AsRef<Path>>(path: P) -> GCardResult<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let graph = bincode::deserialize_from(reader)?;
        Ok(graph)
    }

    pub fn from_csv<P: AsRef<Path>>(
        dir: P,
        schema: &Schema,
        delimiter: u8,
        num_threads: usize,
    ) -> GCardResult<Self> {
        let dir = dir.as_ref();
        let builder = LabeledGraphBuilder::new(num_threads);
        let builder = schema.vertices().iter().fold(builder, |builder, vertex| {
            builder.add_vertex_label(vertex.label)
        });
        let builder = schema.edges().iter().fold(builder, |builder, edge| {
            builder.add_edge_label(edge.label, edge.from, edge.to)
        });
        let builder = schema
            .vertices()
            .iter()
            .try_fold(builder, |builder, vertex| {
                let label_id = vertex.label;
                let label_name = schema.get_vertex_label_name(label_id).unwrap();
                let path = dir.join(format!("{label_name}.csv"));
                read_vertices_from_csv(builder, label_id, path, delimiter)
            })?;
        let builder = schema.edges().iter().try_fold(builder, |builder, edge| {
            let label_id = edge.label;
            let label_name = schema.get_edge_label_name(label_id).unwrap();
            let path = dir.join(format!("{label_name}.csv"));
            read_edges_from_csv(builder, label_id, path, delimiter)
        })?;
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{build_ldbc_graph, build_ldbc_schema};

    #[test]
    fn test_build_graph() {
        let schema = build_ldbc_schema();
        let graph = build_ldbc_graph();
        let num_vertices = graph
            .vertex_labels()
            .map(|label_id| graph.vertices(label_id).unwrap().len())
            .sum::<usize>();
        assert_eq!(num_vertices, 30191);
        let mut in_deg_sum = 0;
        let mut out_deg_sum = 0;
        for vertex_label in graph.vertex_labels() {
            for e in schema.outgoing_edges(vertex_label).unwrap() {
                let edge_label_id = e.label;
                for v in graph.vertices(vertex_label).unwrap() {
                    let out_deg = graph
                        .outgoing_degree(LabeledVertex::new(*v, vertex_label), edge_label_id)
                        .unwrap_or_default();
                    out_deg_sum += out_deg;
                }
            }
            for e in schema.incoming_edges(vertex_label).unwrap() {
                let edge_label_id = e.label;
                for v in graph.vertices(vertex_label).unwrap() {
                    let in_deg = graph
                        .incoming_degree(LabeledVertex::new(*v, vertex_label), edge_label_id)
                        .unwrap_or_default();
                    in_deg_sum += in_deg;
                }
            }
        }
        assert_eq!(in_deg_sum, 44742);
        assert_eq!(out_deg_sum, 44742);
    }
}
