mod path;
mod path_v2;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use bimap::BiHashMap;
pub use path::*;
pub use path_v2::*;
use serde::{Deserialize, Serialize};

use crate::common::{EdgeCardinality, LabelId};
use crate::error::{GCardError, GCardResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaVertex {
    pub label: LabelId,
    pub discrete: bool,
}

impl From<(LabelId, bool)> for SchemaVertex {
    fn from((label, discrete): (LabelId, bool)) -> Self {
        Self { label, discrete }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchemaEdge {
    pub from: LabelId,
    pub to: LabelId,
    pub label: LabelId,
    pub card: EdgeCardinality,
}

impl From<(LabelId, LabelId, LabelId, EdgeCardinality)> for SchemaEdge {
    fn from((from, to, label, card): (LabelId, LabelId, LabelId, EdgeCardinality)) -> Self {
        Self {
            from,
            to,
            label,
            card,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaUnchecked {
    vertex_labels: HashMap<String, LabelId>,
    edge_labels: HashMap<String, LabelId>,
    vertices: Vec<SchemaVertex>,
    edges: Vec<SchemaEdge>,
}

impl SchemaUnchecked {
    pub fn add_vertex_label(mut self, name: String, id: LabelId) -> Self {
        self.vertex_labels.insert(name, id);
        self
    }

    pub fn add_edge_label(mut self, name: String, id: LabelId) -> Self {
        self.edge_labels.insert(name, id);
        self
    }

    pub fn add_vertex<V>(mut self, vertex: V) -> Self
    where
        V: Into<SchemaVertex>,
    {
        self.vertices.push(vertex.into());
        self
    }

    pub fn add_edge<E>(mut self, edge: E) -> Self
    where
        E: Into<SchemaEdge>,
    {
        self.edges.push(edge.into());
        self
    }
}

impl TryFrom<SchemaUnchecked> for Schema {
    type Error = GCardError;

    fn try_from(unchecked: SchemaUnchecked) -> GCardResult<Schema> {
        let vertex_label_map: BiHashMap<_, _> = unchecked.vertex_labels.into_iter().collect();
        let edge_label_map: BiHashMap<_, _> = unchecked.edge_labels.into_iter().collect();
        let mut label_to_vertex_id = HashMap::with_capacity(vertex_label_map.len());
        let mut label_to_edge_id = HashMap::with_capacity(edge_label_map.len());
        for (vertex_id, vertex) in unchecked.vertices.iter().enumerate() {
            if !vertex_label_map.contains_right(&vertex.label) {
                let err = format!("vertex label id: {} does not exist", vertex.label);
                return Err(GCardError::Schema(err));
            }
            if label_to_vertex_id.insert(vertex.label, vertex_id).is_some() {
                let err = format!("duplicate vertex: {:?}", vertex);
                return Err(GCardError::Schema(err));
            }
        }
        for (edge_id, edge) in unchecked.edges.iter().enumerate() {
            if !edge_label_map.contains_right(&edge.label) {
                let err = format!("edge label id: {} does not exist", edge.label);
                return Err(GCardError::Schema(err));
            }
            if label_to_edge_id.insert(edge.label, edge_id).is_some() {
                let err = format!("duplicate edge: {:?}", edge);
                return Err(GCardError::Schema(err));
            }
        }
        let mut outgoing_adj_lists: HashMap<_, _> = vertex_label_map
            .right_values()
            .map(|label_id| (*label_id, Vec::new()))
            .collect();
        let mut incoming_adj_lists = outgoing_adj_lists.clone();

        for (edge_id, edge) in unchecked.edges.iter().enumerate() {
            if let Some(adj_list) = outgoing_adj_lists.get_mut(&edge.from) {
                adj_list.push(edge_id)
            } else {
                let err = format!("vertex with label id: {} does not exist", edge.from);
                return Err(GCardError::Schema(err));
            }
            if let Some(adj_list) = incoming_adj_lists.get_mut(&edge.to) {
                adj_list.push(edge_id)
            } else {
                let err = format!("vertex with label id: {} does not exist", edge.to);
                return Err(GCardError::Schema(err));
            }
        }
        let schema = Schema {
            vertex_label_map,
            edge_label_map,
            vertices: unchecked.vertices,
            edges: unchecked.edges,
            label_to_vertex_id,
            label_to_edge_id,
            outgoing_adj_lists,
            incoming_adj_lists,
        };
        let wcc = schema.weak_connected_components();
        if wcc.len() != 1 {
            let err = format!("schema not connected, cc: {wcc:?}");
            return Err(GCardError::Schema(err));
        }
        Ok(schema)
    }
}

impl From<Schema> for SchemaUnchecked {
    fn from(value: Schema) -> Self {
        let vertices = value.vertices;
        let edges = value.edges;
        let vertex_labels = value.vertex_label_map.into_iter().collect();
        let edge_labels = value.edge_label_map.into_iter().collect();
        Self {
            vertex_labels,
            edge_labels,
            vertices,
            edges,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SchemaUnchecked")]
#[serde(into = "SchemaUnchecked")]
pub struct Schema {
    vertex_label_map: BiHashMap<String, LabelId>,
    edge_label_map: BiHashMap<String, LabelId>,
    vertices: Vec<SchemaVertex>,
    edges: Vec<SchemaEdge>,
    label_to_vertex_id: HashMap<LabelId, usize>,
    label_to_edge_id: HashMap<LabelId, usize>,
    outgoing_adj_lists: HashMap<LabelId, Vec<usize>>,
    incoming_adj_lists: HashMap<LabelId, Vec<usize>>,
}

impl Schema {
    pub fn export_json<P: AsRef<Path>>(&self, path: P) -> GCardResult<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        Ok(())
    }

    pub fn import_json<P: AsRef<Path>>(path: P) -> GCardResult<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let schema = serde_json::from_reader(reader)?;
        Ok(schema)
    }

    pub fn get_vertex(&self, label_id: LabelId) -> Option<&SchemaVertex> {
        self.label_to_vertex_id
            .get(&label_id)
            .map(|vertex_id| &self.vertices[*vertex_id])
    }

    pub fn get_edge(&self, label_id: LabelId) -> Option<&SchemaEdge> {
        self.label_to_edge_id
            .get(&label_id)
            .map(|edge_id| &self.edges[*edge_id])
    }

    pub fn vertices(&self) -> &[SchemaVertex] {
        &self.vertices
    }

    pub fn edges(&self) -> &[SchemaEdge] {
        &self.edges
    }

    pub fn outgoing_edges(
        &self,
        vertex_label_id: LabelId,
    ) -> Option<impl Iterator<Item = &SchemaEdge>> {
        let adj_list = self.outgoing_adj_lists.get(&vertex_label_id)?;
        Some(adj_list.iter().map(|&edge_id| &self.edges[edge_id]))
    }

    pub fn incoming_edges(
        &self,
        vertex_label_id: LabelId,
    ) -> Option<impl Iterator<Item = &SchemaEdge>> {
        let adj_list = self.incoming_adj_lists.get(&vertex_label_id)?;
        Some(adj_list.iter().map(|&edge_id| &self.edges[edge_id]))
    }

    pub fn get_vertex_label_id(&self, name: &str) -> Option<LabelId> {
        self.vertex_label_map.get_by_left(name).copied()
    }

    pub fn get_vertex_label_name(&self, id: LabelId) -> Option<&String> {
        self.vertex_label_map.get_by_right(&id)
    }

    pub fn get_edge_label_id(&self, name: &str) -> Option<LabelId> {
        self.edge_label_map.get_by_left(name).copied()
    }

    pub fn get_edge_label_name(&self, id: LabelId) -> Option<&String> {
        self.edge_label_map.get_by_right(&id)
    }

    fn weak_connected_components(&self) -> Vec<Vec<LabelId>> {
        if self.vertices.is_empty() {
            return vec![];
        }
        let mut cc = Vec::new();
        let mut visited = HashSet::new();
        for start in &self.vertices {
            if visited.contains(&start.label) {
                continue;
            }
            let mut current_cc = HashSet::new();
            let mut stack = vec![&start.label];
            while let Some(u) = stack.pop() {
                visited.insert(*u);
                current_cc.insert(*u);
                let edges = self.outgoing_edges(*u).unwrap();
                stack.extend(edges.map(|e| &e.to).filter(|v| !visited.contains(*v)));
                let edges = self.incoming_edges(*u).unwrap();
                stack.extend(edges.map(|e| &e.from).filter(|v| !visited.contains(*v)));
            }
            cc.push(current_cc.into_iter().collect());
        }
        cc
    }
}

#[cfg(test)]
mod tests {
    use maplit::hashset;

    use super::*;

    fn build_test_schema() -> Schema {
        SchemaUnchecked::default()
            .add_vertex_label("person".into(), 0)
            .add_vertex_label("city".into(), 1)
            .add_vertex_label("country".into(), 2)
            .add_edge_label("knows".into(), 0)
            .add_edge_label("isLocatedIn".into(), 1)
            .add_edge_label("isPartOf".into(), 2)
            .add_vertex((0, false))
            .add_vertex((1, true))
            .add_vertex((2, true))
            .add_edge((0, 0, 0, EdgeCardinality::ManyToMany))
            .add_edge((0, 1, 1, EdgeCardinality::ManyToOne))
            .add_edge((1, 2, 2, EdgeCardinality::ManyToOne))
            .try_into()
            .unwrap()
    }

    #[test]
    fn test_schema() {
        let schema = build_test_schema();
        let vertices_expected: HashSet<_> = hashset! {
            SchemaVertex {label: 0, discrete: false},
            SchemaVertex {label: 1, discrete: true},
            SchemaVertex {label: 2, discrete: true},
        };
        let vertices_actual: HashSet<_> = schema.vertices().iter().cloned().collect();
        assert_eq!(vertices_actual, vertices_expected);

        let edges: HashSet<_> = schema.outgoing_edges(0).unwrap().cloned().collect();
        let expected = hashset![
            (0, 1, 1, EdgeCardinality::ManyToOne,).into(),
            (0, 0, 0, EdgeCardinality::ManyToMany,).into(),
        ];
        assert_eq!(edges, expected);

        let edges: HashSet<_> = schema.incoming_edges(0).unwrap().cloned().collect();
        let expected = hashset![(0, 0, 0, EdgeCardinality::ManyToMany,).into()];
        assert_eq!(edges, expected);

        let edges: HashSet<_> = schema.outgoing_edges(1).unwrap().cloned().collect();
        let expected = hashset![(1, 2, 2, EdgeCardinality::ManyToOne).into()];
        assert_eq!(edges, expected);
    }

    #[test]
    fn test_serde() {
        let schema = build_test_schema();
        let expected = r#"{
    "vertex_labels": {
      "country": 2,
      "city": 1,
      "person": 0
    },
    "edge_labels": {
      "knows": 0,
      "isLocatedIn": 1,
      "isPartOf": 2
    },
    "vertices": [
      {
        "label": 0,
        "discrete": false
      },
      {
        "label": 1,
        "discrete": true
      },
      {
        "label": 2,
        "discrete": true
      }
    ],
    "edges": [
      {
        "from": 0,
        "to": 0,
        "label": 0,
        "card": "ManyToMany"
      },
      {
        "from": 0,
        "to": 1,
        "label": 1,
        "card": "ManyToOne"
      },
      {
        "from": 1,
        "to": 2,
        "label": 2,
        "card": "ManyToOne"
      }
    ]
}"#;
        assert_eq!(serde_json::from_str::<Schema>(expected).unwrap(), schema);
    }
}
