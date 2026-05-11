use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Display;
use std::io::BufWriter;
use std::marker::PhantomData;

use ahash::{HashSet, HashSetExt};
use itertools::Itertools;
use ptree::{write_tree, TreeItem};

use super::Schema;
use crate::common::{EdgeCardinality, LabelId, TagId};
use crate::pattern::{
    GeneralPattern, GraphPattern, PathPattern, PatternEdge, PatternVertex, RawPattern,
};

#[derive(Debug, Clone)]
enum PathFamilyNodeKind {
    Root {
        left_children: Vec<usize>,
        right_children: Vec<usize>,
    },
    Left {
        parent: usize,
        children: Vec<usize>,
    },
    Right {
        parent: usize,
        children: Vec<usize>,
    },
}

#[derive(Debug, Clone)]
struct PathFamilyNode {
    id: usize,
    path: PathPattern,
    kind: PathFamilyNodeKind,
}

#[derive(Debug, Clone, Copy)]
pub struct PathFamilyNodeRef<'a> {
    family: &'a PathFamily,
    node: &'a PathFamilyNode,
}

impl<'a> PathFamilyNodeRef<'a> {
    fn new(family: &'a PathFamily, node: &'a PathFamilyNode) -> Self {
        Self { family, node }
    }

    pub fn id(&self) -> usize {
        self.node.id
    }

    pub fn path(&self) -> &PathPattern {
        &self.node.path
    }

    pub fn parent(&self) -> Option<PathFamilyNodeRef> {
        let parent_id = match self.node.kind {
            PathFamilyNodeKind::Left { parent, .. } | PathFamilyNodeKind::Right { parent, .. } => {
                Some(parent)
            }
            PathFamilyNodeKind::Root { .. } => None,
        }?;
        self.family.get_node(parent_id)
    }

    fn kind(&self) -> &PathFamilyNodeKind {
        &self.node.kind
    }
}

#[derive(Debug, Clone)]
pub struct PathFamily {
    nodes: Vec<PathFamilyNode>,
}

impl<'a> TreeItem for PathFamilyNodeRef<'a> {
    type Child = Self;

    fn write_self<W: std::io::Write>(
        &self,
        f: &mut W,
        style: &ptree::Style,
    ) -> std::io::Result<()> {
        write!(
            f,
            "ID: {}, Path: {}",
            style.paint(self.id()),
            style.paint(self.path())
        )
    }

    fn children(&self) -> std::borrow::Cow<[Self::Child]> {
        let children = match self.kind() {
            PathFamilyNodeKind::Root {
                left_children,
                right_children,
            } => left_children
                .iter()
                .chain(right_children)
                .map(|child_id| self.family.get_node(*child_id).unwrap())
                .collect_vec(),
            PathFamilyNodeKind::Left { children, .. }
            | PathFamilyNodeKind::Right { children, .. } => children
                .iter()
                .map(|child_id| self.family.get_node(*child_id).unwrap())
                .collect_vec(),
        };
        Cow::from(children)
    }
}

impl Display for PathFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let root = self.root();
        let mut writer = BufWriter::new(vec![]);
        write_tree(&root, &mut writer).unwrap();
        let tree = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        write!(f, "{}", tree)
    }
}

impl PathFamily {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get_node(&self, id: usize) -> Option<PathFamilyNodeRef> {
        self.nodes
            .get(id)
            .map(|node| PathFamilyNodeRef::new(self, node))
    }

    pub fn root(&self) -> PathFamilyNodeRef {
        self.get_node(0).expect("path family must have a root node")
    }

    pub fn left_iter(&self) -> Iter<LeftIter> {
        Iter {
            family: self,
            nodes: vec![self.root()],
            _marker: PhantomData,
        }
    }

    pub fn right_iter(&self) -> Iter<RightIter> {
        Iter {
            family: self,
            nodes: vec![self.root()],
            _marker: PhantomData,
        }
    }

    fn new(path: PathPattern) -> Self {
        let node = PathFamilyNode {
            id: 0,
            path,
            kind: PathFamilyNodeKind::Root {
                left_children: vec![],
                right_children: vec![],
            },
        };
        Self { nodes: vec![node] }
    }

    fn add_left_node(&mut self, path: PathPattern, parent: usize) -> usize {
        let id = self.len();
        let node = PathFamilyNode {
            id,
            path,
            kind: PathFamilyNodeKind::Left {
                parent,
                children: vec![],
            },
        };
        let parent = self.nodes.get_mut(parent).unwrap();
        match &mut parent.kind {
            PathFamilyNodeKind::Root { left_children, .. } => {
                left_children.push(id);
            }
            PathFamilyNodeKind::Left { children, .. } => {
                children.push(id);
            }
            _ => unreachable!(),
        }
        self.nodes.push(node);
        id
    }

    fn add_right_node(&mut self, path: PathPattern, parent: usize) -> usize {
        let id = self.len();
        let node = PathFamilyNode {
            id,
            path,
            kind: PathFamilyNodeKind::Right {
                parent,
                children: vec![],
            },
        };
        let parent = self.nodes.get_mut(parent).unwrap();
        match &mut parent.kind {
            PathFamilyNodeKind::Root { right_children, .. } => {
                right_children.push(id);
            }
            PathFamilyNodeKind::Right { children, .. } => {
                children.push(id);
            }
            _ => unreachable!(),
        }
        self.nodes.push(node);
        id
    }
}

pub struct LeftIter;
pub struct RightIter;

#[derive(Debug)]
pub struct Iter<'a, T> {
    family: &'a PathFamily,
    nodes: Vec<PathFamilyNodeRef<'a>>,
    _marker: PhantomData<T>,
}

impl<'a> Iterator for Iter<'a, LeftIter> {
    type Item = PathFamilyNodeRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.nodes.pop()?;
        match current.kind() {
            PathFamilyNodeKind::Root { left_children, .. } => {
                for child in left_children {
                    let node = self.family.get_node(*child).unwrap();
                    self.nodes.push(node);
                }
            }
            PathFamilyNodeKind::Left { children, .. } => {
                for child in children {
                    let node = self.family.get_node(*child).unwrap();
                    self.nodes.push(node);
                }
            }
            _ => unreachable!(),
        }
        Some(current)
    }
}

impl<'a> Iterator for Iter<'a, RightIter> {
    type Item = PathFamilyNodeRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.nodes.pop()?;
        match current.kind() {
            PathFamilyNodeKind::Root { right_children, .. } => {
                for child in right_children {
                    let node = self.family.get_node(*child).unwrap();
                    self.nodes.push(node);
                }
            }
            PathFamilyNodeKind::Right { children, .. } => {
                for child in children {
                    let node = self.family.get_node(*child).unwrap();
                    self.nodes.push(node);
                }
            }
            _ => unreachable!(),
        }
        Some(current)
    }
}

struct PathFamilyGenerateState {
    family: PathFamily,
    current_node_id: usize,
    raw: RawPattern,
    label_count: HashMap<LabelId, usize>,
    repeated_label_limit: usize,
    limit: usize,
}

impl PathFamilyGenerateState {
    fn next_vertex_tag_id(&self) -> TagId {
        self.raw
            .max_vertex_tag_id()
            .map(|tag_id| tag_id + 1)
            .unwrap_or_default()
    }

    fn next_edge_tag_id(&self) -> TagId {
        self.raw
            .max_edge_tag_id()
            .map(|tag_id| tag_id + 1)
            .unwrap_or_default()
    }
}

impl Schema {
    fn generate_paths_from_vertex_inner(
        &self,
        vertex_label: LabelId,
        length: usize,
        with_many_to_one: bool,
    ) -> BTreeMap<Vec<u8>, PathPattern> {
        let mut paths = BTreeMap::new();
        let mut queue = VecDeque::new();
        queue.push_back(
            RawPattern::new()
                .push_back_vertex((0, vertex_label))
                .to_path()
                .unwrap(),
        );
        while let Some(path) = queue.pop_front() {
            if path.len() == length {
                paths.entry(path.encode()).or_insert(path.clone());
                continue;
            }
            let end = path.end();
            let mut raw = RawPattern::from(path);
            let next_vertex_tag_id = raw.next_vertex_tag_id();
            let next_edge_tag_id = raw.next_edge_tag_id();
            for e in self.outgoing_edges(end.label_id()).unwrap() {
                if !with_many_to_one
                    && matches!(
                        e.card,
                        EdgeCardinality::OneToOne
                            | EdgeCardinality::ManyToOne
                            | EdgeCardinality::OneToMany
                    )
                {
                    continue;
                }
                let path = raw
                    .push_back_vertex((next_vertex_tag_id, e.to))
                    .push_back_edge((next_edge_tag_id, end.tag_id(), next_vertex_tag_id, e.label))
                    .to_path()
                    .unwrap();
                queue.push_back(path);
                raw.pop_back_edge().pop_back_vertex();
            }
            for e in self.incoming_edges(end.label_id()).unwrap() {
                if !with_many_to_one
                    && matches!(
                        e.card,
                        EdgeCardinality::OneToOne
                            | EdgeCardinality::ManyToOne
                            | EdgeCardinality::OneToMany
                    )
                {
                    continue;
                }
                let path = raw
                    .push_back_vertex((next_vertex_tag_id, e.from))
                    .push_back_edge((next_edge_tag_id, next_vertex_tag_id, end.tag_id(), e.label))
                    .to_path()
                    .unwrap();
                queue.push_back(path);
                raw.pop_back_edge().pop_back_vertex();
            }
        }
        paths
    }

    pub fn generate_paths_from_vertex(
        &self,
        vertex_label: LabelId,
        length: usize,
    ) -> Vec<PathPattern> {
        self.generate_paths_from_vertex_inner(vertex_label, length, true)
            .into_values()
            .collect()
    }

    fn generate_paths_inner(&self, length: usize, with_many_to_one: bool) -> Vec<PathPattern> {
        self.vertices()
            .iter()
            .map(|v| self.generate_paths_from_vertex_inner(v.label, length, with_many_to_one))
            .reduce(|mut a, b| {
                a.extend(b);
                a
            })
            .unwrap_or_default()
            .into_values()
            .collect()
    }

    pub fn generate_paths(&self, length: usize) -> Vec<PathPattern> {
        self.generate_paths_inner(length, true)
    }

    pub fn generate_stars(&self, degree: usize) -> Vec<GeneralPattern> {
        if degree == 0 {
            return vec![];
        }
        let mut stars = BTreeMap::new();
        for v in self.vertices() {
            for comb in self
                .outgoing_edges(v.label)
                .unwrap()
                .chain(self.incoming_edges(v.label).unwrap())
                .combinations(degree)
            {
                let mut raw = RawPattern::new();
                let center_tag_id = raw.next_vertex_tag_id();
                raw.push_back_vertex((center_tag_id, v.label));
                for e in comb {
                    // Outgoing edge
                    if e.from == v.label {
                        let nbr_tag_id = raw.next_vertex_tag_id();
                        raw.push_back_vertex((nbr_tag_id, e.to));
                        raw.push_back_edge((
                            raw.next_edge_tag_id(),
                            center_tag_id,
                            nbr_tag_id,
                            e.label,
                        ));
                    }
                    // Incoming edge
                    else {
                        let nbr_tag_id = raw.next_vertex_tag_id();
                        raw.push_back_vertex((nbr_tag_id, e.from));
                        raw.push_back_edge((
                            raw.next_edge_tag_id(),
                            nbr_tag_id,
                            center_tag_id,
                            e.label,
                        ));
                    }
                }
                let star = raw.to_general().unwrap();
                stars.insert(star.encode(), star);
            }
        }
        stars.into_values().collect()
    }

    pub fn generate_cycles(&self, length: usize) -> Vec<GeneralPattern> {
        if length == 0 {
            return vec![];
        }
        let paths = self.generate_paths(length - 1);
        let mut cycles = Vec::new();
        let mut cycle_set = HashSet::new();
        for p in paths {
            let start = p.start();
            let end = p.end();
            for e in self
                .outgoing_edges(start.label_id())
                .unwrap()
                .filter(|e| e.to == end.label_id())
            {
                let mut raw = RawPattern::from(&p);
                let next_edge_tag_id = raw.next_edge_tag_id();
                let cycle = raw
                    .push_back_edge((next_edge_tag_id, start.tag_id(), end.tag_id(), e.label))
                    .to_general()
                    .unwrap();
                let code = cycle.encode();
                if !cycle_set.contains(&code) {
                    cycles.push(cycle);
                    cycle_set.insert(code);
                }
            }
            for e in self
                .incoming_edges(start.label_id())
                .unwrap()
                .filter(|e| e.from == end.label_id())
            {
                let mut raw = RawPattern::from(&p);
                let next_edge_tag_id = raw.next_edge_tag_id();
                let cycle = raw
                    .push_back_edge((next_edge_tag_id, end.tag_id(), start.tag_id(), e.label))
                    .to_general()
                    .unwrap();
                let code = cycle.encode();
                if !cycle_set.contains(&code) {
                    cycles.push(cycle);
                    cycle_set.insert(code);
                }
            }
        }
        cycles
    }

    pub fn generate_paths_without_many_to_one(&self, length: usize) -> Vec<PathPattern> {
        self.generate_paths_inner(length, false)
    }

    pub fn generate_path_family_from_path(
        &self,
        path: &PathPattern,
        repeated_label_limit: usize,
        limit: usize,
    ) -> PathFamily {
        let family = PathFamily::new(path.clone());
        if path.len() >= limit {
            return family;
        }
        let raw = path.clone().into();
        let mut label_count = HashMap::new();
        for e in path.edges() {
            *label_count.entry(e.label_id()).or_default() += 1;
        }
        let mut state = PathFamilyGenerateState {
            family,
            current_node_id: 0,
            raw,
            label_count,
            repeated_label_limit,
            limit,
        };
        self.generate_path_family_from_path_recursive::<true>(&mut state);
        let mut state = PathFamilyGenerateState {
            current_node_id: 0,
            ..state
        };
        self.generate_path_family_from_path_recursive::<false>(&mut state);
        state.family
    }

    fn generate_path_family_from_path_recursive<const FROM_END: bool>(
        &self,
        state: &mut PathFamilyGenerateState,
    ) {
        if state.raw.get_edges_num() == state.limit {
            return;
        }
        let current_node_id = state.current_node_id;
        let current_node = state.family.get_node(current_node_id).unwrap();
        let current_path = current_node.path();
        let current_path_start = current_path.start();
        let current_path_end = current_path.end();
        let current_vertex_label = if FROM_END {
            current_path_end.label_id()
        } else {
            current_path_start.label_id()
        };
        for e in self
            .outgoing_edges(current_vertex_label)
            .unwrap()
            .filter(|e| {
                matches!(
                    e.card,
                    EdgeCardinality::ManyToOne | EdgeCardinality::OneToOne
                )
            })
        {
            let current_label_count = state.label_count.get(&e.label).copied().unwrap_or_default();
            if current_label_count == state.repeated_label_limit {
                continue;
            }
            let next_vertex_label = e.to;
            let next_vertex_tag_id = state.next_vertex_tag_id();
            let next_edge_tag_id = state.next_edge_tag_id();
            if FROM_END {
                state
                    .raw
                    .push_back_vertex(PatternVertex::new(next_vertex_tag_id, next_vertex_label))
                    .push_back_edge(PatternEdge::new(
                        next_edge_tag_id,
                        current_path_end.tag_id(),
                        next_vertex_tag_id,
                        e.label,
                    ));
            } else {
                state
                    .raw
                    .push_front_vertex(PatternVertex::new(next_vertex_tag_id, next_vertex_label))
                    .push_front_edge(PatternEdge::new(
                        next_edge_tag_id,
                        current_path_start.tag_id(),
                        next_vertex_tag_id,
                        e.label,
                    ));
            }
            let path = state.raw.to_path().unwrap();
            let current_node_id = if FROM_END {
                state.family.add_right_node(path, current_node_id)
            } else {
                state.family.add_left_node(path, current_node_id)
            };
            *state.label_count.entry(e.label).or_default() += 1;
            state.current_node_id = current_node_id;
            self.generate_path_family_from_path_recursive::<FROM_END>(state);
            *state.label_count.get_mut(&e.label).unwrap() -= 1;
            if FROM_END {
                state.raw.pop_back_edge().pop_back_vertex();
            } else {
                state.raw.pop_front_edge().pop_front_vertex();
            }
        }
        for e in self
            .incoming_edges(current_vertex_label)
            .unwrap()
            .filter(|e| {
                matches!(
                    e.card,
                    EdgeCardinality::OneToMany | EdgeCardinality::OneToOne
                )
            })
        {
            let current_label_count = state.label_count.get(&e.label).copied().unwrap_or_default();
            if current_label_count == state.repeated_label_limit {
                continue;
            }
            let next_vertex_label = e.from;
            let next_vertex_tag_id = state.next_vertex_tag_id();
            let next_edge_tag_id = state.next_edge_tag_id();
            if FROM_END {
                state
                    .raw
                    .push_back_vertex(PatternVertex::new(next_vertex_tag_id, next_vertex_label))
                    .push_back_edge(PatternEdge::new(
                        next_edge_tag_id,
                        next_vertex_tag_id,
                        current_path_end.tag_id(),
                        e.label,
                    ));
            } else {
                state
                    .raw
                    .push_front_vertex(PatternVertex::new(next_vertex_tag_id, next_vertex_label))
                    .push_front_edge(PatternEdge::new(
                        next_edge_tag_id,
                        next_vertex_tag_id,
                        current_path_start.tag_id(),
                        e.label,
                    ));
            }
            let path = state.raw.to_path().unwrap();
            let current_node_id = if FROM_END {
                state.family.add_right_node(path, current_node_id)
            } else {
                state.family.add_left_node(path, current_node_id)
            };
            *state.label_count.entry(e.label).or_default() += 1;
            state.current_node_id = current_node_id;
            self.generate_path_family_from_path_recursive::<FROM_END>(state);
            *state.label_count.get_mut(&e.label).unwrap() -= 1;
            if FROM_END {
                state.raw.pop_back_edge().pop_back_vertex();
            } else {
                state.raw.pop_front_edge().pop_front_vertex();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::build_ldbc_schema;

    #[test]
    fn test_generate_paths() {
        let schema = build_ldbc_schema();
        assert_eq!(schema.generate_paths(0).len(), 11);
        assert_eq!(schema.generate_paths(1).len(), 25);
        assert_eq!(schema.generate_paths(2).len(), 186);
        assert_eq!(schema.generate_paths(3).len(), 1021);
    }

    #[test]
    fn test_generate_path_family() {
        let schema = build_ldbc_schema();
        let path = RawPattern::new()
            .push_back_vertex((0, 6))
            .push_back_vertex((1, 6))
            .push_back_edge((0, 0, 1, 14))
            .to_path()
            .unwrap();
        let family = schema.generate_path_family_from_path(&path, 2, 5);
        assert_eq!(family.len(), 7);
        println!("{}", family)
    }
}
