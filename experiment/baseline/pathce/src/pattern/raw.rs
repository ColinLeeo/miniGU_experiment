use std::collections::{HashMap, HashSet, VecDeque};
use std::iter;

use serde::{Deserialize, Serialize};

use super::general::GeneralPattern;
use super::path::PathPattern;
use super::{canonicalize, GraphPattern, PatternAdjacency, PatternEdge, PatternVertex};
use crate::common::{EdgeCardinality, EdgeDirection, TagId};
use crate::error::{GCardError, GCardResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawPattern {
    vertices: VecDeque<PatternVertex>,
    edges: VecDeque<PatternEdge>,
}

impl RawPattern {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_vertices_edges<V, E, IV, IE>(vertices: IV, edges: IE) -> Self
    where
        IV: IntoIterator<Item = V>,
        IE: IntoIterator<Item = E>,
        V: Into<PatternVertex>,
        E: Into<PatternEdge>,
    {
        let vertices = vertices.into_iter().map(Into::into).collect();
        let edges = edges.into_iter().map(Into::into).collect();
        Self { vertices, edges }
    }

    pub fn max_vertex_tag_id(&self) -> Option<TagId> {
        self.vertices.iter().map(|v| v.tag_id).max()
    }

    pub fn max_edge_tag_id(&self) -> Option<TagId> {
        self.edges.iter().map(|e| e.tag_id).max()
    }

    pub fn next_vertex_tag_id(&self) -> TagId {
        self.max_vertex_tag_id()
            .map(|tag_id| tag_id + 1)
            .unwrap_or_default()
    }

    pub fn next_edge_tag_id(&self) -> TagId {
        self.max_edge_tag_id()
            .map(|tag_id| tag_id + 1)
            .unwrap_or_default()
    }

    pub fn get_vertices_num(&self) -> usize {
        self.vertices.len()
    }

    pub fn get_edges_num(&self) -> usize {
        self.edges.len()
    }

    pub fn push_back_vertex<V: Into<PatternVertex>>(&mut self, vertex: V) -> &mut Self {
        self.vertices.push_back(vertex.into());
        self
    }

    pub fn pop_front_vertex(&mut self) -> &mut Self {
        self.vertices.pop_front();
        self
    }

    pub fn pop_front_edge(&mut self) -> &mut Self {
        self.edges.pop_front();
        self
    }

    pub fn pop_back_vertex(&mut self) -> &mut Self {
        self.vertices.pop_back();
        self
    }

    pub fn pop_back_edge(&mut self) -> &mut Self {
        self.edges.pop_back();
        self
    }

    pub fn push_back_edge<E: Into<PatternEdge>>(&mut self, edge: E) -> &mut Self {
        self.edges.push_back(edge.into());
        self
    }

    pub fn push_front_vertex<V: Into<PatternVertex>>(&mut self, vertex: V) -> &mut Self {
        self.vertices.insert(0, vertex.into());
        self
    }

    pub fn push_front_edge<E: Into<PatternEdge>>(&mut self, edge: E) -> &mut Self {
        self.edges.insert(0, edge.into());
        self
    }

    pub fn to_general(&self) -> GCardResult<GeneralPattern> {
        let tag_vertex_map: HashMap<_, _> = self
            .vertices
            .iter()
            .enumerate()
            .map(|(i, v)| (v.tag_id, i))
            .collect();
        if tag_vertex_map.len() != self.vertices.len() {
            return Err(GCardError::Pattern("duplicate vertex tag id".into()));
        }
        let tag_edge_map: HashMap<_, _> = self
            .edges
            .iter()
            .enumerate()
            .map(|(i, e)| (e.tag_id, i))
            .collect();
        if tag_edge_map.len() != self.edges.len() {
            return Err(GCardError::Pattern("duplicate edge tag id".into()));
        }
        let mut outgoing_adjacencies: HashMap<_, _> =
            self.vertices.iter().map(|v| (v.tag_id, vec![])).collect();
        let mut incoming_adjacencies: HashMap<_, _> =
            self.vertices.iter().map(|v| (v.tag_id, vec![])).collect();
        for e in &self.edges {
            let outgoing_adjacency = PatternAdjacency {
                edge_tag_id: e.tag_id,
                edge_label_id: e.label_id,
                neighbor_tag_id: e.dst,
                direction: EdgeDirection::Out,
            };
            outgoing_adjacencies
                .get_mut(&e.src)
                .ok_or_else(|| {
                    let err = format!("vertex with tag id {} not exist", e.src);
                    GCardError::Pattern(err)
                })?
                .push(outgoing_adjacency);

            let incoming_adjacency = PatternAdjacency {
                edge_tag_id: e.tag_id,
                edge_label_id: e.label_id,
                neighbor_tag_id: e.src,
                direction: EdgeDirection::In,
            };
            incoming_adjacencies
                .get_mut(&e.dst)
                .ok_or_else(|| {
                    let err = format!("vertex with tag id {} not exist", e.dst);
                    GCardError::Pattern(err)
                })?
                .push(incoming_adjacency);
        }
        let vertices = self.vertices.clone();
        let edges = self.edges.clone();
        let mut pattern = GeneralPattern {
            vertices: vertices.into(),
            edges: edges.into(),
            tag_vertex_map,
            tag_edge_map,
            outgoing_adjacencies,
            incoming_adjacencies,
            vertex_rank_map: HashMap::new(),
            edge_rank_map: HashMap::new(),
            rank_vertex_map: HashMap::new(),
            rank_edge_map: HashMap::new(),
        };
        if !is_connected(&pattern) {
            return Err(GCardError::Pattern("pattern not connected".into()));
        }
        let (vertex_rank_map, edge_rank_map) = canonicalize(&pattern);
        let rank_vertex_map = vertex_rank_map
            .iter()
            .map(|(tag_id, rank)| (*rank, *tag_id))
            .collect();
        let rank_edge_map = edge_rank_map
            .iter()
            .map(|(tag_id, rank)| (*rank, *tag_id))
            .collect();
        pattern.vertex_rank_map = vertex_rank_map;
        pattern.edge_rank_map = edge_rank_map;
        pattern.rank_vertex_map = rank_vertex_map;
        pattern.rank_edge_map = rank_edge_map;
        Ok(pattern)
    }

    pub fn to_path(&self) -> GCardResult<PathPattern> {
        let pattern = self.to_general()?;
        if pattern.vertices().is_empty() {
            return Err(GCardError::Pattern("empty path is not allowed".into()));
        }
        let mut directions = Vec::new();
        let mut start = pattern.vertices().first().unwrap().tag_id;
        let end = pattern.vertices().last().unwrap().tag_id;
        for e in pattern.edges() {
            if e.src == start {
                directions.push(EdgeDirection::Out);
                start = e.dst;
            } else if e.dst == start {
                directions.push(EdgeDirection::In);
                start = e.src;
            } else {
                return Err(GCardError::Pattern("invalid path".into()));
            }
        }
        let cards = iter::repeat(EdgeCardinality::default())
            .take(directions.len())
            .collect();
        if start == end {
            Ok(PathPattern {
                pattern,
                directions,
                cards,
            })
        } else {
            Err(GCardError::Pattern("invalid path".into()))
        }
    }
}

impl TryFrom<RawPattern> for GeneralPattern {
    type Error = GCardError;

    fn try_from(value: RawPattern) -> Result<Self, Self::Error> {
        value.to_general()
    }
}

impl TryFrom<RawPattern> for PathPattern {
    type Error = GCardError;

    fn try_from(value: RawPattern) -> Result<Self, Self::Error> {
        value.to_path()
    }
}

impl<P: GraphPattern> From<&P> for RawPattern {
    fn from(value: &P) -> Self {
        Self {
            vertices: value.vertices().iter().copied().collect(),
            edges: value.edges().iter().copied().collect(),
        }
    }
}

impl From<PathPattern> for RawPattern {
    fn from(value: PathPattern) -> Self {
        Self {
            vertices: value.pattern.vertices.into(),
            edges: value.pattern.edges.into(),
        }
    }
}

impl From<GeneralPattern> for RawPattern {
    fn from(value: GeneralPattern) -> Self {
        Self {
            vertices: value.vertices.into(),
            edges: value.edges.into(),
        }
    }
}

fn is_connected<P: GraphPattern>(pattern: &P) -> bool {
    if pattern.vertices().len() <= 1 {
        return true;
    }
    let mut visited = HashSet::new();
    let start = pattern.vertices().first().unwrap();
    let mut stack = vec![start.tag_id];
    while let Some(tag_id) = stack.pop() {
        visited.insert(tag_id);
        stack.extend(
            pattern
                .adjacencies(tag_id)
                .unwrap()
                .map(|v| v.neighbor_tag_id)
                .filter(|tag_id| !visited.contains(tag_id)),
        );
    }
    visited.len() == pattern.vertices().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_general_pattern() {
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .to_general()
            .is_err());
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .push_back_edge((0, 1, 2, 345))
            .to_general()
            .is_err());
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .push_back_edge((1, 1, 2, 345))
            .to_general()
            .is_ok());
        assert!(RawPattern::new().to_general().is_ok());
    }

    #[test]
    fn test_build_path() {
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .to_path()
            .is_err());
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .push_back_edge((0, 1, 2, 345))
            .to_path()
            .is_err());
        assert!(RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((1, 1, 2, 345))
            .push_back_edge((0, 0, 1, 123))
            .to_path()
            .is_err());
        assert!(RawPattern::new().to_path().is_err());

        let p = RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 1))
            .push_back_vertex((2, 2))
            .push_back_edge((0, 0, 1, 123))
            .push_back_edge((1, 2, 1, 345))
            .to_path()
            .unwrap();
        assert_eq!(p.directions(), [EdgeDirection::Out, EdgeDirection::In])
    }
}
