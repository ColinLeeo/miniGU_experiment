use std::collections::BTreeSet;

use ahash::{HashMap, HashSet, HashSetExt};
use serde::{Deserialize, Serialize};

use crate::common::{LabelId, TagId};
use crate::pattern::PatternVertex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CatalogVertex {
    tag_id: TagId,
    label_id: LabelId,
}

impl CatalogVertex {
    pub fn new(tag_id: TagId, label_id: LabelId) -> Self {
        Self { tag_id, label_id }
    }

    pub fn tag_id(&self) -> TagId {
        self.tag_id
    }

    pub fn label_id(&self) -> LabelId {
        self.label_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogEdgeKind {
    Star { center: TagId },
    Path { src: TagId, dst: TagId },
    General(Vec<TagId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEdge {
    tag_id: TagId,
    label_id: LabelId,
    kind: CatalogEdgeKind,
}

impl CatalogEdge {
    pub fn star(tag_id: TagId, label_id: LabelId, center: TagId) -> Self {
        Self {
            tag_id,
            label_id,
            kind: CatalogEdgeKind::Star { center },
        }
    }

    pub fn path(tag_id: TagId, label_id: LabelId, src: TagId, dst: TagId) -> Self {
        Self {
            tag_id,
            label_id,
            kind: CatalogEdgeKind::Path { src, dst },
        }
    }

    pub fn general(tag_id: TagId, label_id: LabelId, vertices: Vec<TagId>) -> Self {
        Self {
            tag_id,
            label_id,
            kind: CatalogEdgeKind::General(vertices),
        }
    }

    pub fn tag_id(&self) -> TagId {
        self.tag_id
    }

    pub fn label_id(&self) -> LabelId {
        self.label_id
    }

    pub fn kind(&self) -> &CatalogEdgeKind {
        &self.kind
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CatalogPattern {
    vertices: Vec<CatalogVertex>,
    edges: Vec<CatalogEdge>,
    tag_vertex_map: HashMap<TagId, usize>,
    tag_edge_map: HashMap<TagId, usize>,
    adj_list: HashMap<TagId, BTreeSet<TagId>>,
}

impl CatalogPattern {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_vertices_num(&self) -> usize {
        self.tag_vertex_map.len()
    }

    pub fn get_edges_num(&self) -> usize {
        self.tag_edge_map.len()
    }

    pub fn next_edge_tag_id(&self) -> TagId {
        self.edges()
            .map(|e| e.tag_id() + 1)
            .max()
            .unwrap_or_default()
    }

    pub fn add_vertex(&mut self, vertex: CatalogVertex) {
        let index = self.vertices.len();
        assert!(self.tag_vertex_map.insert(vertex.tag_id, index).is_none());
        self.vertices.push(vertex);
        self.adj_list.entry(vertex.tag_id()).or_default();
    }

    pub fn add_edge(&mut self, edge: CatalogEdge) {
        let index = self.edges.len();
        assert!(self.tag_edge_map.insert(edge.tag_id, index).is_none());
        match edge.kind() {
            CatalogEdgeKind::Star { center } => {
                self.adj_list
                    .entry(*center)
                    .or_default()
                    .insert(edge.tag_id());
            }
            CatalogEdgeKind::Path { src, dst } => {
                self.adj_list.entry(*src).or_default().insert(edge.tag_id());
                self.adj_list.entry(*dst).or_default().insert(edge.tag_id());
            }
            CatalogEdgeKind::General(vertices) => {
                for v in vertices {
                    self.adj_list.entry(*v).or_default().insert(edge.tag_id());
                }
            }
        }
        self.edges.push(edge);
    }

    pub fn remove_vertex(&mut self, tag_id: TagId) -> bool {
        if self.tag_vertex_map.remove(&tag_id).is_none() {
            return false;
        }
        let edges = self.adj_list.remove(&tag_id).unwrap();
        for edge_tag_id in edges {
            let index = self.tag_edge_map.remove(&edge_tag_id).unwrap();
            let edge = self.edges.get(index).unwrap();
            let vertices = match edge.kind() {
                CatalogEdgeKind::Star { center } => &[*center][..],
                CatalogEdgeKind::Path { src, dst } => &[*src, *dst][..],
                CatalogEdgeKind::General(vertices) => vertices,
            };
            for vertex in vertices {
                if let Some(edges) = self.adj_list.get_mut(vertex) {
                    edges.remove(&edge_tag_id);
                }
            }
        }
        true
    }

    pub fn remove_edge(&mut self, tag_id: TagId) -> bool {
        let index = self.tag_edge_map.remove(&tag_id);
        let index = if let Some(index) = index {
            index
        } else {
            return false;
        };
        let edge = self.edges.get(index).unwrap();
        let vertices = match edge.kind() {
            CatalogEdgeKind::Star { center } => &[*center][..],
            CatalogEdgeKind::Path { src, dst } => &[*src, *dst][..],
            CatalogEdgeKind::General(vertices) => vertices,
        };
        for vertex in vertices {
            self.adj_list.get_mut(vertex).unwrap().remove(&tag_id);
        }
        true
    }

    pub fn vertices(&self) -> impl Iterator<Item = &CatalogVertex> {
        self.vertices
            .iter()
            .filter(|v| self.tag_vertex_map.contains_key(&v.tag_id))
    }

    pub fn edges(&self) -> impl Iterator<Item = &CatalogEdge> {
        self.edges
            .iter()
            .filter(|e| self.tag_edge_map.contains_key(&e.tag_id))
    }

    pub fn incident_edges(&self, tag_id: TagId) -> Option<impl Iterator<Item = &CatalogEdge>> {
        Some(
            self.adj_list
                .get(&tag_id)?
                .iter()
                .map(|edge_tag_id| self.get_edge(*edge_tag_id).unwrap()),
        )
    }

    pub fn get_vertex(&self, tag_id: TagId) -> Option<&CatalogVertex> {
        let index = self.tag_vertex_map.get(&tag_id)?;
        self.vertices.get(*index)
    }

    pub fn get_edge(&self, tag_id: TagId) -> Option<&CatalogEdge> {
        let index = self.tag_edge_map.get(&tag_id)?;
        self.edges.get(*index)
    }
}

impl From<PatternVertex> for CatalogVertex {
    fn from(value: PatternVertex) -> Self {
        CatalogVertex::new(value.tag_id(), value.label_id())
    }
}

impl From<CatalogVertex> for CatalogPattern {
    fn from(value: CatalogVertex) -> Self {
        let mut graph = CatalogPattern::new();
        graph.add_vertex(value);
        graph
    }
}

impl<'de> Deserialize<'de> for CatalogPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw_edges = <Vec<RawCatalogEdge>>::deserialize(deserializer)?;
        let mut vertices = HashSet::new();
        let mut edges = Vec::new();
        for e in raw_edges {
            let tag_id = edges.len() as TagId;
            let endpoints = e.vertices;
            match endpoints[..] {
                [center] => {
                    vertices.insert(CatalogVertex::new(center, 0));
                    edges.push(CatalogEdge::star(tag_id, e.label_id, center));
                }
                [src, dst] => {
                    vertices.insert(CatalogVertex::new(src, 0));
                    vertices.insert(CatalogVertex::new(dst, 0));
                    edges.push(CatalogEdge::path(tag_id, e.label_id, src, dst));
                }
                _ => unreachable!(),
            };
        }
        let mut pattern = CatalogPattern::new();
        for v in vertices {
            pattern.add_vertex(v);
        }
        for e in edges {
            pattern.add_edge(e);
        }
        Ok(pattern)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RawCatalogEdge {
    label_id: LabelId,
    vertices: Vec<TagId>,
}

#[test]
fn test_ser() {
    let p = vec![
        RawCatalogEdge {
            label_id: 0,
            vertices: vec![3, 4],
        },
        RawCatalogEdge {
            label_id: 2,
            vertices: vec![5],
        },
    ];
    println!("{}", serde_json::to_string_pretty(&p).unwrap());
}
