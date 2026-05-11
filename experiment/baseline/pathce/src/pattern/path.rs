use std::fmt::Display;

use itertools::Itertools;
use serde::{Deserialize, Serialize};

use super::general::GeneralPattern;
use super::{GraphPattern, PatternAdjacency, PatternEdge, PatternVertex};
use crate::common::{EdgeCardinality, EdgeDirection, TagId};
use crate::pattern::RawPattern;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPattern")]
#[serde(into = "RawPattern")]
pub struct PathPattern {
    pub(super) pattern: GeneralPattern,
    pub(super) directions: Vec<EdgeDirection>,
    pub(super) cards: Vec<EdgeCardinality>,
}

impl Display for PathPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let start = self.start();
        write!(f, "({}:{})", start.tag_id(), start.label_id())?;
        for (e, d) in self.edges().iter().zip_eq(self.directions()) {
            match d {
                EdgeDirection::Out => {
                    let next = self.get_vertex(e.dst()).unwrap();
                    write!(
                        f,
                        "-[{}:{}]->({}:{})",
                        e.tag_id(),
                        e.label_id(),
                        next.tag_id(),
                        next.label_id()
                    )?;
                }
                EdgeDirection::In => {
                    let next = self.get_vertex(e.src()).unwrap();
                    write!(
                        f,
                        "<-[{}:{}]-({}:{})",
                        e.tag_id(),
                        e.label_id(),
                        next.tag_id(),
                        next.label_id()
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl From<PathPattern> for GeneralPattern {
    fn from(value: PathPattern) -> Self {
        value.pattern
    }
}

impl GraphPattern for PathPattern {
    fn vertices(&self) -> &[PatternVertex] {
        self.pattern.vertices()
    }

    fn edges(&self) -> &[PatternEdge] {
        self.pattern.edges()
    }

    fn get_vertex(&self, tag_id: TagId) -> Option<PatternVertex> {
        self.pattern.get_vertex(tag_id)
    }

    fn get_vertex_rank(&self, tag_id: TagId) -> Option<TagId> {
        self.pattern.get_vertex_rank(tag_id)
    }

    fn get_vertex_from_rank(&self, rank: TagId) -> Option<PatternVertex> {
        self.pattern.get_vertex_from_rank(rank)
    }

    fn get_edge(&self, tag_id: TagId) -> Option<PatternEdge> {
        self.pattern.get_edge(tag_id)
    }

    fn get_edge_rank(&self, tag_id: TagId) -> Option<TagId> {
        self.pattern.get_edge_rank(tag_id)
    }

    fn get_edge_from_rank(&self, rank: TagId) -> Option<PatternEdge> {
        self.pattern.get_edge_from_rank(rank)
    }

    fn outgoing_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]> {
        self.pattern.outgoing_adjacencies(tag_id)
    }

    fn incoming_adjacencies(&self, tag_id: TagId) -> Option<&[PatternAdjacency]> {
        self.pattern.incoming_adjacencies(tag_id)
    }
}

pub fn merge_paths_to_star(paths: &[&PathPattern]) -> (GeneralPattern, TagId) {
    assert!(!paths.is_empty());
    let (first, paths) = paths.split_first().unwrap();
    let mut raw = RawPattern::from(*first);
    let mut vertex_offset = raw.next_vertex_tag_id();
    let mut edge_offset = raw.next_edge_tag_id();
    let first_start = first.start();
    for path in paths {
        let start = path.start();
        assert_eq!(start.label_id(), first_start.label_id());
        for v in path.vertices() {
            if v.tag_id() == start.tag_id() {
                continue;
            }
            raw.push_back_vertex((v.tag_id() + vertex_offset, v.label_id()));
        }
        for e in path.edges() {
            let src_tag = if e.src() != start.tag_id() {
                e.src() + vertex_offset
            } else {
                first_start.tag_id()
            };
            let dst_tag = if e.dst() != start.tag_id() {
                e.dst() + vertex_offset
            } else {
                first_start.tag_id()
            };
            raw.push_back_edge((e.tag_id() + edge_offset, src_tag, dst_tag, e.label_id));
        }
        vertex_offset = raw.next_vertex_tag_id();
        edge_offset = raw.next_edge_tag_id();
    }
    let star = raw.to_general().unwrap();
    let center_rank = star.get_vertex_rank(first_start.tag_id()).unwrap();
    (star, center_rank)
}

impl PathPattern {
    pub fn reverse(&self) -> PathPattern {
        let mut raw = RawPattern::new();
        for v in self.vertices().iter().rev() {
            raw.push_back_vertex(*v);
        }
        for e in self.edges().iter().rev() {
            raw.push_back_edge(*e);
        }
        raw.to_path().unwrap()
    }

    pub fn start(&self) -> PatternVertex {
        *self.vertices().first().unwrap()
    }

    pub fn end(&self) -> PatternVertex {
        *self.vertices().last().unwrap()
    }

    pub fn is_empty(&self) -> bool {
        self.edges().is_empty()
    }

    pub fn len(&self) -> usize {
        self.edges().len()
    }

    pub fn directions(&self) -> &[EdgeDirection] {
        &self.directions
    }

    pub fn cards(&self) -> &[EdgeCardinality] {
        &self.cards
    }

    pub fn cards_mut(&mut self) -> &mut [EdgeCardinality] {
        &mut self.cards
    }

    pub fn is_single_direction(&self) -> bool {
        self.directions().iter().all(|&d| d == EdgeDirection::Out)
    }

    pub fn is_symmetric(&self) -> bool {
        let rev = self.reverse();
        let vertices = self
            .vertices()
            .iter()
            .zip_eq(rev.vertices())
            .all(|(v1, v2)| v1.label_id() == v2.label_id());
        if !vertices {
            return false;
        }
        let edges = self
            .edges()
            .iter()
            .zip_eq(rev.edges())
            .all(|(e1, e2)| e1.label_id() == e2.label_id());
        if !edges {
            return false;
        }
        self.directions()
            .iter()
            .zip_eq(rev.directions())
            .all(|(d1, d2)| d1 == d2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::raw::RawPattern;

    #[test]
    fn test_merge() {
        let p1 = RawPattern::new()
            .push_back_vertex((0, 0))
            .push_back_vertex((1, 0))
            .push_back_edge((0, 0, 1, 0))
            .to_path()
            .unwrap();
        let p2 = RawPattern::new()
            .push_back_vertex((1, 0))
            .push_back_vertex((0, 0))
            .push_back_vertex((2, 0))
            .push_back_edge((0, 0, 1, 0))
            .push_back_edge((1, 0, 2, 0))
            .to_path()
            .unwrap();
        let p3 = RawPattern::new()
            .push_back_vertex((3, 0))
            .to_path()
            .unwrap();
        let (p, center_rank) = merge_paths_to_star(&[&p1, &p2, &p3]);
        let expected = RawPattern::new()
            .push_back_vertex((0, 0))
            .push_back_vertex((1, 0))
            .push_back_vertex((2, 0))
            .push_back_vertex((3, 0))
            .push_back_edge((0, 0, 1, 0))
            .push_back_edge((1, 2, 0, 0))
            .push_back_edge((2, 2, 3, 0))
            .to_general()
            .unwrap();
        let expected_rank = expected.get_vertex_rank(0).unwrap();
        assert_eq!(p.encode(), expected.encode());
        assert_eq!(center_rank, expected_rank);
    }

    #[test]
    fn test_serde_json() {
        let p = RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 8))
            .push_back_edge((0, 0, 1, 2))
            .to_path()
            .unwrap();
        let expected = "{\"vertices\":[{\"tag_id\":0,\"label_id\":1},{\"tag_id\":1,\"label_id\":8}],\"edges\":[{\"tag_id\":0,\"src\":0,\"dst\":1,\"label_id\":2}]}";
        assert_eq!(serde_json::to_string(&p).unwrap(), expected);

        let p_new: PathPattern = serde_json::from_str(expected).unwrap();
        assert_eq!(p_new, p);
    }

    #[test]
    fn test_encode() {
        let p1 = RawPattern::new()
            .push_back_vertex((0, 1))
            .push_back_vertex((1, 8))
            .push_back_edge((0, 0, 1, 2))
            .to_path()
            .unwrap();
        let p2 = RawPattern::new()
            .push_back_vertex((0, 8))
            .push_back_vertex((1, 1))
            .push_back_edge((0, 1, 0, 2))
            .to_general()
            .unwrap();
        assert_eq!(p1.encode(), p2.encode());
    }
}
