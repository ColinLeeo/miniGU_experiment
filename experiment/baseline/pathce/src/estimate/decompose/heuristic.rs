use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use ahash::{HashSet, HashSetExt};
use itertools::Itertools;
use log::{debug, trace};
use petgraph::algo::is_cyclic_undirected;
use petgraph::prelude::UnGraphMap;

use super::PatternDecomposer;
use crate::catalog::Catalog;
use crate::common::TagId;
use crate::estimate::catalog_pattern::{CatalogEdge, CatalogEdgeKind, CatalogPattern};
use crate::pattern::{GeneralPattern, GraphPattern, PatternEdge, PatternVertex, RawPattern};

pub struct HeuristicDecomposer<'a, C> {
    catalog: &'a C,
    max_path_length: usize,
    max_star_length: usize,
    max_star_degree: usize,
    limit: usize,
    disable_star: bool,
    disable_prune: bool,
    disable_cyclic: bool,
}

impl<'a, C> HeuristicDecomposer<'a, C> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: &'a C,
        max_path_length: usize,
        mut max_star_length: usize,
        mut max_star_degree: usize,
        limit: usize,
        disable_star: bool,
        disable_prune: bool,
        disable_cyclic: bool,
    ) -> Self {
        if disable_star {
            max_star_length = 0;
            max_star_degree = 0;
        }
        Self {
            catalog,
            max_path_length,
            max_star_length,
            max_star_degree,
            limit,
            disable_star,
            disable_prune,
            disable_cyclic,
        }
    }
}

impl<'a, C: Catalog> HeuristicDecomposer<'a, C> {
    pub fn decompose_with_pivots<P: GraphPattern>(
        &self,
        pattern: &P,
        pivots: &[TagId],
    ) -> CatalogPattern {
        let candidate_paths =
            find_candidate_paths_with_pivots(pattern, &pivots.iter().copied().collect());
        self.decompose_candidate_paths(pattern, candidate_paths)
    }

    fn decompose_candidate_paths<P: GraphPattern>(
        &self,
        pattern: &P,
        candidate_paths: BTreeMap<TagId, Vec<PathRef>>,
    ) -> CatalogPattern {
        let mut edges = Vec::new();
        for (pivot, paths) in candidate_paths {
            let (mut mergeable, unmergeable): (Vec<_>, _) = paths.iter().partition(|path| {
                pattern.get_vertex_degree(path.end()).unwrap() == 1
                    && path.len() <= self.max_star_length
            });
            let remaining_mergeable = (mergeable.len() > self.max_star_degree)
                .then(|| mergeable.split_off(self.max_star_degree))
                .unwrap_or_default();
            if !mergeable.is_empty() {
                let mut segments = mergeable
                    .iter()
                    .map(|path| PathSegmentWrapper::new(path.to_segment(), pattern))
                    .collect_vec();
                segments.sort_unstable();
                let (dedup, mut duplicates) = segments.partition_dedup();
                let segments = dedup.iter().map(|s| s.segment).collect_vec();
                edges.push(self.translate_star(pattern, &segments, pivot));
                while !duplicates.is_empty() {
                    duplicates.sort_unstable();
                    let (dedup, new_duplicates) = duplicates.partition_dedup();
                    let segments = dedup.iter().map(|s| s.segment).collect_vec();
                    edges.push(self.translate_star(pattern, &segments, pivot));
                    duplicates = new_duplicates
                }
            }
            for path in remaining_mergeable.into_iter().chain(unmergeable) {
                let decomposed = self.decompose_path(pattern, path);
                edges.extend(decomposed);
            }
        }
        debug!("decompose edges: {:?}", edges);

        let mut catalog_pattern = CatalogPattern::new();
        let mut added_vertices = HashSet::new();
        for edge in edges {
            match *edge.kind() {
                CatalogEdgeKind::Star { center } => {
                    if !added_vertices.contains(&center) {
                        added_vertices.insert(center);
                        let vertex = pattern.get_vertex(center).unwrap();
                        catalog_pattern.add_vertex(vertex.into());
                    }
                }
                CatalogEdgeKind::Path { src, dst } => {
                    if !added_vertices.contains(&src) {
                        added_vertices.insert(src);
                        let src = pattern.get_vertex(src).unwrap();
                        catalog_pattern.add_vertex(src.into());
                    }
                    if !added_vertices.contains(&dst) {
                        added_vertices.insert(dst);
                        let dst = pattern.get_vertex(dst).unwrap();
                        catalog_pattern.add_vertex(dst.into());
                    }
                }
                _ => unreachable!(),
            }
            catalog_pattern.add_edge(edge);
        }
        catalog_pattern
    }

    fn decompose_acyclic<P: GraphPattern>(&self, pattern: &P) -> CatalogPattern {
        let candidate_paths = find_candidate_paths(pattern);
        self.decompose_candidate_paths(pattern, candidate_paths)
    }

    fn decompose_cyclic<P: GraphPattern>(&self, pattern: &P) -> Vec<CatalogPattern> {
        // First decompose the pattern using spanning trees
        let mut catalog_patterns = generate_spanning_trees(pattern, self.limit)
            .into_iter()
            .map(|p| self.decompose_acyclic(&p))
            .collect_vec();

        if !self.disable_cyclic {
            if pattern.is_cycle() {
                catalog_patterns.extend(pattern.vertices().iter().map(|v| {
                    let candidate_paths =
                        find_candidate_paths_with_pivots(pattern, &[v.tag_id()].into());
                    self.decompose_candidate_paths(pattern, candidate_paths)
                }));
            } else if self.disable_prune {
                catalog_patterns.push(self.decompose_acyclic(pattern))
            } else {
                let pruned = self.prune(pattern);
                catalog_patterns.push(self.decompose_acyclic(&pruned))
            }
        }
        catalog_patterns
    }

    fn prune<P: GraphPattern>(&self, pattern: &P) -> GeneralPattern {
        // TODO: Handle multiedges
        let mut pattern = RawPattern::from(pattern).to_general().unwrap();
        loop {
            let mut neighbors: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
            let candidate_paths = find_candidate_paths(&pattern);
            for (v, paths) in &candidate_paths {
                for p in paths {
                    let end = p.end();
                    if end != *v && pattern.get_vertex_degree(end).unwrap() != 1 {
                        neighbors.entry(*v).or_default().insert(end);
                        neighbors.entry(end).or_default().insert(*v);
                    }
                }
            }
            let mut edges_to_prune = Vec::new();
            while !neighbors.is_empty() {
                let victim = neighbors
                    .iter_mut()
                    .min_by_key(|(_, neighbors)| neighbors.len())
                    .map(|(victim, _)| victim)
                    .copied()
                    .unwrap();
                let victim_neighbors = neighbors.remove(&victim).unwrap();
                if victim_neighbors.len() > 2 {
                    // Should prune now
                    let num_edges_to_prune = victim_neighbors.len() - 2;
                    for p in candidate_paths
                        .get(&victim)
                        .unwrap()
                        .iter()
                        .take(num_edges_to_prune)
                    {
                        let edge = p
                            .edges
                            .iter()
                            .max_by_key(|e| {
                                let label_id = pattern.get_edge(**e).unwrap().label_id();
                                self.catalog.get_edge_count(label_id).unwrap()
                            })
                            .unwrap();
                        edges_to_prune.push(*edge);
                    }
                    assert_eq!(num_edges_to_prune, edges_to_prune.len());
                    break;
                }
                for neighbor in &victim_neighbors {
                    let neighbor_neighbors = neighbors.get_mut(neighbor).unwrap();
                    neighbor_neighbors.remove(&victim);
                    neighbor_neighbors
                        .extend(victim_neighbors.iter().filter(|n| *n != neighbor).copied());
                }
            }
            if edges_to_prune.is_empty() {
                return pattern;
            }
            debug!("prune edges {edges_to_prune:?}");
            let edges = pattern
                .edges()
                .iter()
                .filter(|e| !edges_to_prune.contains(&e.tag_id()))
                .copied();
            pattern = RawPattern::with_vertices_edges(pattern.vertices().iter().copied(), edges)
                .to_general()
                .unwrap();
        }
    }

    fn translate_path<P: GraphPattern>(&self, pattern: &P, segment: PathSegment) -> CatalogEdge {
        assert!(segment.len() > 0);
        let real_start_tag_id = segment.start();
        let real_end_tag_id = segment.end();
        let path = if real_start_tag_id != real_end_tag_id {
            RawPattern::with_vertices_edges(
                segment
                    .vertices
                    .iter()
                    .map(|v| pattern.get_vertex(*v).unwrap()),
                segment.edges.iter().map(|e| pattern.get_edge(*e).unwrap()),
            )
            .to_path()
            .unwrap()
        } else {
            let mut vertices = segment
                .vertices
                .iter()
                .map(|v| pattern.get_vertex(*v).unwrap())
                .collect_vec();
            let mut edges = segment
                .edges
                .iter()
                .map(|e| pattern.get_edge(*e).unwrap())
                .collect_vec();
            let next_vertex_tag_id = vertices
                .iter()
                .map(|v| v.tag_id() + 1)
                .max()
                .unwrap_or_default();
            let end_label_id = vertices.last().unwrap().label_id();
            let end_tag_id = vertices.last().unwrap().tag_id();
            *vertices.last_mut().unwrap() = PatternVertex::new(next_vertex_tag_id, end_label_id);
            let end_edge = edges.last().unwrap();
            let end_edge_src = end_edge.src();
            let end_edge_dst = end_edge.dst();
            let end_edge_label_id = end_edge.label_id();
            let end_edge_tag_id = end_edge.tag_id();
            let new_end_edge = if end_edge_src == end_tag_id {
                PatternEdge::new(
                    end_edge_tag_id,
                    next_vertex_tag_id,
                    end_edge_dst,
                    end_edge_label_id,
                )
            } else if end_edge_dst == end_tag_id {
                PatternEdge::new(
                    end_edge_tag_id,
                    end_edge_src,
                    next_vertex_tag_id,
                    end_edge_label_id,
                )
            } else {
                unreachable!()
            };
            *edges.last_mut().unwrap() = new_end_edge;
            RawPattern::with_vertices_edges(vertices, edges)
                .to_path()
                .unwrap()
        };
        // Use the first edge's tag_id as the path's tag_id
        let edge_tag_id = segment.edges[0];
        let start_rank = path.get_vertex_rank(path.start().tag_id()).unwrap();
        let end_rank = path.get_vertex_rank(path.end().tag_id()).unwrap();
        let label_id = self.catalog.get_path_label_id(&path.encode()).unwrap();
        let catalog_path = self.catalog.get_path(label_id).unwrap();
        let catalog_start_rank = catalog_path
            .get_vertex_rank(catalog_path.start().tag_id())
            .unwrap();
        let catalog_end_rank = catalog_path
            .get_vertex_rank(catalog_path.end().tag_id())
            .unwrap();
        if (start_rank, end_rank) == (catalog_start_rank, catalog_end_rank) {
            CatalogEdge::path(edge_tag_id, label_id, real_start_tag_id, real_end_tag_id)
        } else if (start_rank, end_rank) == (catalog_end_rank, catalog_start_rank) {
            CatalogEdge::path(edge_tag_id, label_id, real_end_tag_id, real_start_tag_id)
        } else {
            unreachable!()
        }
    }

    fn translate_star<P: GraphPattern>(
        &self,
        pattern: &P,
        segments: &[PathSegment],
        center: TagId,
    ) -> CatalogEdge {
        assert!(!segments.is_empty());
        // Segments have the same start vertex (to form a star)
        let start = segments[0].start();
        assert!(segments.iter().all(|segment| segment.start() == start));
        let vertices = segments
            .iter()
            .flat_map(|segment| &segment.vertices[1..])
            .copied()
            .chain([start])
            .map(|v| pattern.get_vertex(v).unwrap());
        // Edges are unique
        let edges = segments
            .iter()
            .flat_map(|segment| segment.edges)
            .copied()
            .map(|e| pattern.get_edge(e).unwrap());
        let star = RawPattern::with_vertices_edges(vertices, edges.clone())
            .to_general()
            .unwrap();
        let center_rank = star.get_vertex_rank(center).unwrap();
        trace!(
            "translate_star: segments: {:?}, center_tag: {}, center_rank: {}",
            segments,
            center,
            center_rank,
        );
        let label_id = self
            .catalog
            .get_star_label_id(center_rank, &star.encode())
            .unwrap();
        // If the star is a single vertex, use the vertex tag_id as the star's tag_id. Otherwise,
        // use the first edge's tag_id.
        let tag_id = edges.map(|e| e.tag_id()).next().unwrap_or(start);
        CatalogEdge::star(tag_id, label_id, center)
    }

    fn decompose_path<P: GraphPattern>(&self, pattern: &P, path: &PathRef) -> Vec<CatalogEdge> {
        assert!(!path.is_empty());
        let mut path = path.to_segment();
        let mut segments = vec![];
        while path.len() > self.max_path_length {
            let (current, remaining) = path.split_at(self.max_path_length);
            segments.push(current);
            path = remaining;
        }
        if path.len() > 0 {
            segments.push(path);
        }
        assert!(!segments.is_empty());
        segments
            .into_iter()
            .map(|segment| {
                let start = segment.start();
                let end = segment.end();
                let start_degree = pattern.get_vertex_degree(start).unwrap();
                let end_degree = pattern.get_vertex_degree(end).unwrap();
                if self.disable_star || (start_degree > 1 && end_degree > 1) {
                    self.translate_path(pattern, segment)
                } else if start_degree == 1 {
                    self.translate_star(pattern, &[segment], end)
                } else if end_degree == 1 {
                    self.translate_star(pattern, &[segment], start)
                } else {
                    unreachable!()
                }
            })
            .collect()
    }
}

fn find_pivots<P: GraphPattern>(pattern: &P) -> Vec<TagId> {
    pattern
        .vertices()
        .iter()
        .map(|v| v.tag_id())
        .filter(|v| {
            let degree = pattern.get_vertex_degree(*v).unwrap();
            degree >= 3
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRef {
    vertices: Vec<TagId>,
    edges: Vec<TagId>,
}

fn split_at<'a>(
    vertices: &'a [TagId],
    edges: &'a [TagId],
    vertex_index: usize,
) -> (PathSegment<'a>, PathSegment<'a>) {
    assert!(vertex_index < vertices.len());
    let first_vertices = &vertices[0..=vertex_index];
    let second_vertices = &vertices[vertex_index..];
    let first_edges = &edges[0..vertex_index];
    let second_edges = &edges[vertex_index..];
    let first = PathSegment {
        vertices: first_vertices,
        edges: first_edges,
    };
    let second = PathSegment {
        vertices: second_vertices,
        edges: second_edges,
    };
    (first, second)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathSegment<'a> {
    vertices: &'a [TagId],
    edges: &'a [TagId],
}

impl<'a> PathSegment<'a> {
    fn len(&self) -> usize {
        self.edges.len()
    }

    fn split_at(&self, vertex_index: usize) -> (PathSegment<'a>, PathSegment<'a>) {
        split_at(self.vertices, self.edges, vertex_index)
    }

    fn start(&self) -> TagId {
        *self.vertices.first().unwrap()
    }

    fn end(&self) -> TagId {
        *self.vertices.last().unwrap()
    }
}

struct PathSegmentWrapper<'a, P> {
    segment: PathSegment<'a>,
    pattern: &'a P,
}

impl<'a, P> PathSegmentWrapper<'a, P> {
    fn new(segment: PathSegment<'a>, pattern: &'a P) -> Self {
        Self { segment, pattern }
    }
}

impl<'a, P: GraphPattern> PartialEq for PathSegmentWrapper<'a, P> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl<'a, P: GraphPattern> Eq for PathSegmentWrapper<'a, P> {}

impl<'a, P: GraphPattern> PartialOrd for PathSegmentWrapper<'a, P> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<'a, P: GraphPattern> Ord for PathSegmentWrapper<'a, P> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.segment
            .vertices
            .iter()
            .map(|v| self.pattern.get_vertex(*v).unwrap().label_id())
            .interleave(
                self.segment
                    .edges
                    .iter()
                    .map(|e| self.pattern.get_edge(*e).unwrap().label_id()),
            )
            .cmp(
                other
                    .segment
                    .vertices
                    .iter()
                    .map(|v| other.pattern.get_vertex(*v).unwrap().label_id())
                    .interleave(
                        other
                            .segment
                            .edges
                            .iter()
                            .map(|e| self.pattern.get_edge(*e).unwrap().label_id()),
                    ),
            )
    }
}

impl PathRef {
    fn new(source: TagId) -> Self {
        Self {
            vertices: vec![source],
            edges: vec![],
        }
    }

    fn to_segment(&self) -> PathSegment {
        let (_, segment) = self.split_at(0);
        segment
    }

    fn split_at(&self, vertex_index: usize) -> (PathSegment, PathSegment) {
        split_at(&self.vertices, &self.edges, vertex_index)
    }

    fn push(&mut self, vertex: TagId, edge: TagId) {
        self.vertices.push(vertex);
        self.edges.push(edge);
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.len() == 0
    }

    // fn start(&self) -> TagId {
    //     *self.vertices.first().unwrap()
    // }

    fn end(&self) -> TagId {
        *self.vertices.last().unwrap()
    }
}

fn find_candidate_paths_with_pivots<P: GraphPattern>(
    pattern: &P,
    pivots: &BTreeSet<TagId>,
) -> BTreeMap<TagId, Vec<PathRef>> {
    let mut visited = HashSet::new();
    pivots
        .iter()
        .map(|v| {
            (
                *v,
                find_candidate_paths_from_vertex(pattern, &mut visited, *v, pivots),
            )
        })
        .collect()
}

pub fn find_candidate_paths<P: GraphPattern>(pattern: &P) -> BTreeMap<TagId, Vec<PathRef>> {
    let pivots = if pattern.is_path() {
        let (start, end) = pattern
            .vertices()
            .iter()
            .filter(|v| pattern.get_vertex_degree(v.tag_id()).unwrap() == 1)
            .collect_tuple()
            .unwrap();
        vec![start.tag_id().min(end.tag_id())]
    } else if pattern.is_cycle() {
        let v = pattern
            .vertices()
            .iter()
            .min_by_key(|v| v.tag_id())
            .unwrap();
        vec![v.tag_id()]
    } else {
        find_pivots(pattern)
    };
    let pivot_set = pivots.iter().copied().collect();
    find_candidate_paths_with_pivots(pattern, &pivot_set)
}

fn find_candidate_paths_from_vertex<P: GraphPattern>(
    pattern: &P,
    visited_edges: &mut HashSet<TagId>,
    source: TagId,
    pivots: &BTreeSet<TagId>,
) -> Vec<PathRef> {
    let mut results = Vec::new();
    for adj in pattern.adjacencies(source).unwrap() {
        let mut edge_tag_id = adj.edge_tag_id();
        let mut neighbor_tag_id = adj.neighbor_tag_id();
        if visited_edges.contains(&edge_tag_id) {
            continue;
        }
        let mut path = PathRef::new(source);
        path.push(neighbor_tag_id, edge_tag_id);
        visited_edges.insert(edge_tag_id);
        while !pivots.contains(&neighbor_tag_id) {
            let adj = pattern
                .adjacencies(neighbor_tag_id)
                .unwrap()
                .find(|adj| !visited_edges.contains(&adj.edge_tag_id()));
            if let Some(adj) = adj {
                neighbor_tag_id = adj.neighbor_tag_id();
                edge_tag_id = adj.edge_tag_id();
                path.push(neighbor_tag_id, edge_tag_id);
                visited_edges.insert(edge_tag_id);
            } else {
                break;
            }
        }
        results.push(path);
    }
    results
}

impl<'a, C: Catalog> PatternDecomposer for HeuristicDecomposer<'a, C> {
    fn decompose<P: GraphPattern>(self, pattern: &P) -> Vec<CatalogPattern> {
        assert!(
            !pattern.vertices().is_empty(),
            "Empty pattern is not allowed"
        );
        if pattern.vertices().len() == 1 && pattern.edges().is_empty() {
            let vertex = *pattern.vertices().first().unwrap();
            let path = PathRef::new(vertex.tag_id());
            let edge = self.translate_star(pattern, &[path.to_segment()], vertex.tag_id());
            debug!("decompose single vertex: {:?}", edge);
            let mut catalog_pattern = CatalogPattern::new();
            catalog_pattern.add_vertex(vertex.into());
            catalog_pattern.add_edge(edge);
            return vec![catalog_pattern];
        }
        if !pattern.is_cyclic() {
            debug!("decompose acyclic pattern");
            vec![self.decompose_acyclic(pattern)]
        } else {
            debug!("decompose cyclic pattern");
            self.decompose_cyclic(pattern)
        }
    }
}

/// Generate at most `limit` spanning trees of `pattern`.
fn generate_spanning_trees<P: GraphPattern>(pattern: &P, limit: usize) -> Vec<GeneralPattern> {
    if limit == 0 {
        return vec![];
    }
    assert!(
        pattern.edges().len() <= 64,
        "only patterns with <= 64 edges are supported"
    );
    let mut trees = vec![];
    let initial_spanning_tree = generate_initial_spanning_tree(pattern);
    let mut tree = UnGraphMap::from(PatternWrapper(&initial_spanning_tree));
    let branch_edges = initial_spanning_tree.edges().to_owned();
    let chord_edges = pattern
        .edges()
        .iter()
        .copied()
        .filter(|e| initial_spanning_tree.get_edge(e.tag_id()).is_none())
        .collect_vec();

    trees.push(initial_spanning_tree);
    if trees.len() == limit {
        return trees;
    }
    for chord_code in 1usize..(1 << chord_edges.len().min(branch_edges.len())) {
        let chord_num = chord_code.count_ones();
        for ce in ones(chord_code).map(|i| chord_edges[i as usize]) {
            tree.add_edge(ce.src(), ce.dst(), ce.tag_id());
        }
        for branch_code in
            (1usize..(1 << branch_edges.len())).filter(|code| code.count_ones() == chord_num)
        {
            for be in ones(branch_code).map(|i| branch_edges[i as usize]) {
                tree.remove_edge(be.src(), be.dst());
            }
            if !is_cyclic_undirected(&tree) {
                let mut raw = RawPattern::new();
                for vertex_tag_id in tree.nodes() {
                    let vertex = pattern.get_vertex(vertex_tag_id).unwrap();
                    raw.push_back_vertex(vertex);
                }
                for (_, _, edge_tag_id) in tree.all_edges() {
                    let edge = pattern.get_edge(*edge_tag_id).unwrap();
                    raw.push_back_edge(edge);
                }
                trees.push(raw.to_general().unwrap());
                if trees.len() == limit {
                    return trees;
                }
            }
            for be in ones(branch_code).map(|i| branch_edges[i as usize]) {
                tree.add_edge(be.src(), be.dst(), be.tag_id());
            }
        }
        for ce in ones(chord_code).map(|i| chord_edges[i as usize]) {
            tree.remove_edge(ce.src(), ce.dst());
        }
    }
    trees
}

fn ones(bits: usize) -> Ones {
    Ones { bits }
}

struct Ones {
    bits: usize,
}

impl Iterator for Ones {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bits == 0 {
            None
        } else {
            let lowbit = (!self.bits).wrapping_add(1) & self.bits;
            self.bits -= lowbit;
            Some(lowbit.ilog2())
        }
    }
}

struct PatternWrapper<'a>(&'a GeneralPattern);

impl<'a> From<PatternWrapper<'a>> for UnGraphMap<TagId, TagId> {
    fn from(value: PatternWrapper<'a>) -> Self {
        let mut graph = UnGraphMap::new();
        for e in value.0.edges() {
            graph.add_edge(e.src(), e.dst(), e.tag_id());
        }
        graph
    }
}

/// Generate a spanning tree by BFS.
fn generate_initial_spanning_tree<P: GraphPattern>(pattern: &P) -> GeneralPattern {
    // Start from the vertex with the minimum degree, as it is more likely to be the endpoint of a
    // path.
    let start = pattern
        .vertices()
        .iter()
        .min_by_key(|v| pattern.get_vertex_degree(v.tag_id()).unwrap())
        .unwrap();
    let mut frontier = vec![start.tag_id()];
    let mut visited = HashSet::new();
    visited.insert(start.tag_id());
    let mut raw = RawPattern::new();
    while let Some(current_tag_id) = frontier.pop() {
        let current_vertex = pattern.get_vertex(current_tag_id).unwrap();
        raw.push_back_vertex(current_vertex);
        for adj in pattern.adjacencies(current_tag_id).unwrap() {
            let neighbor_tag_id = adj.neighbor_tag_id();
            if visited.contains(&neighbor_tag_id) {
                continue;
            }
            visited.insert(neighbor_tag_id);
            frontier.push(neighbor_tag_id);
            let edge = pattern.get_edge(adj.edge_tag_id()).unwrap();
            raw.push_back_edge(edge);
        }
    }
    let tree = raw.to_general().unwrap();
    assert_eq!(tree.vertices().len(), pattern.vertices().len());
    assert_eq!(tree.edges().len(), pattern.vertices().len() - 1);
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::MockCatalog;
    use crate::estimate::catalog_pattern::CatalogVertex;

    fn build_test_catalog() -> MockCatalog {
        let mut catalog = MockCatalog::default();
        let p = RawPattern::with_vertices_edges([(0, 0), (1, 0)], [(0, 0, 1, 0)])
            .to_path()
            .unwrap();
        let label_id = catalog.add_path(p);
        assert_eq!(label_id, 0);

        let p =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_path()
                .unwrap();
        let label_id = catalog.add_path(p);
        assert_eq!(label_id, 1);

        let p = RawPattern::new()
            .push_back_vertex((0, 0))
            .to_general()
            .unwrap();
        let label_id = catalog.add_star(p, 0);
        assert_eq!(label_id, 0);

        let p = RawPattern::with_vertices_edges([(0, 0), (1, 0)], [(0, 0, 1, 0)])
            .to_general()
            .unwrap();
        let rank_s = p.get_vertex_rank(0).unwrap();
        let rank_t = p.get_vertex_rank(1).unwrap();
        let label_id = catalog.add_star(p.clone(), rank_s);
        assert_eq!(label_id, 1);
        let label_id = catalog.add_star(p, rank_t);
        assert_eq!(label_id, 2);

        let p =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        let rank_s = p.get_vertex_rank(0).unwrap();
        let rank_t = p.get_vertex_rank(2).unwrap();
        let rank_m = p.get_vertex_rank(1).unwrap();
        let label_id = catalog.add_star(p.clone(), rank_s);
        assert_eq!(label_id, 3);
        let label_id = catalog.add_star(p.clone(), rank_t);
        assert_eq!(label_id, 4);
        let label_id = catalog.add_star(p, rank_m);
        assert_eq!(label_id, 5);

        catalog.add_edge_count(0, 123);
        catalog.add_edge_count(1, 456);

        catalog
    }

    #[test]
    fn test_decompose() {
        let catalog = build_test_catalog();
        let decom = HeuristicDecomposer::new(&catalog, 2, 0, 0, 0, true, true, true);
        let p = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
            [
                (0, 0, 2, 0),
                (1, 2, 1, 0),
                (2, 2, 3, 0),
                (3, 2, 3, 0),
                (4, 3, 4, 0),
                (5, 5, 3, 0),
            ],
        )
        .to_general()
        .unwrap();
        let catalog_pattern = decom.decompose(&p);
        let mut expected = CatalogPattern::new();
        expected.add_vertex(CatalogVertex::new(2, 0));
        expected.add_vertex(CatalogVertex::new(3, 0));
        expected.add_edge(CatalogEdge::star(1, 1, 2));
        expected.add_edge(CatalogEdge::path(2, 0, 2, 3));
        expected.add_edge(CatalogEdge::path(3, 0, 2, 3));
        expected.add_edge(CatalogEdge::star(0, 2, 2));
        expected.add_edge(CatalogEdge::star(4, 1, 3));
        expected.add_edge(CatalogEdge::star(5, 2, 3));
        assert_eq!(catalog_pattern, vec![expected]);

        // let decom = HeuristicDecomposerV2::new(&catalog, 2, 1, 2);
        // let p = RawPattern::with_vertices_edges(
        //     [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
        //     [
        //         (0, 0, 2, 0),
        //         (1, 2, 1, 0),
        //         (2, 2, 3, 0),
        //         (3, 2, 3, 0),
        //         (4, 3, 4, 0),
        //         (5, 5, 3, 0),
        //     ],
        // )
        // .to_general()
        // .unwrap();
        // let catalog_pattern = decom.decompose(&p);
        // let mut expected = CatalogPattern::new();
        // expected.add_vertex(CatalogVertex::new(2, 0));
        // expected.add_vertex(CatalogVertex::new(3, 0));
        // expected.add_edge(CatalogEdge::star(1, 5, 2));
        // expected.add_edge(CatalogEdge::path(2, 0, 2, 3));
        // expected.add_edge(CatalogEdge::path(3, 0, 2, 3));
        // expected.add_edge(CatalogEdge::star(4, 5, 3));
        // assert_eq!(catalog_pattern, expected);
    }

    #[test]
    fn test_translate_path() {
        let catalog = build_test_catalog();
        let decom = HeuristicDecomposer::new(&catalog, 2, 999, 999, 0, true, true, true);
        let p =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        let paths = find_candidate_paths(&p);
        let path = paths.get(&0).unwrap().first().unwrap();
        let edge = decom.translate_path(&p, path.to_segment());
        assert_eq!(edge, CatalogEdge::path(0, 1, 0, 2));

        let p =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 1, 0, 0), (1, 2, 1, 0)])
                .to_general()
                .unwrap();
        let paths = find_candidate_paths(&p);
        let path = paths.get(&0).unwrap().first().unwrap();
        let edge = decom.translate_path(&p, path.to_segment());
        assert_eq!(edge, CatalogEdge::path(0, 1, 2, 0))
    }

    #[test]
    fn test_translate_star() {
        let catalog = build_test_catalog();
        let decom = HeuristicDecomposer::new(&catalog, 2, 999, 999, 0, true, true, true);
        let p =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        let mut path = PathRef::new(0);
        path.push(1, 0);
        path.push(2, 1);
        let edge = decom.translate_star(&p, &[path.to_segment()], 2);
        assert_eq!(edge, CatalogEdge::star(0, 4, 2));

        let mut path1 = PathRef::new(1);
        path1.push(0, 0);
        let mut path2 = PathRef::new(1);
        path2.push(2, 1);
        let edge = decom.translate_star(&p, &[path2.to_segment(), path1.to_segment()], 1);
        assert_eq!(edge, CatalogEdge::star(1, 5, 1));

        let p = RawPattern::new()
            .push_back_vertex((0, 0))
            .to_general()
            .unwrap();
        let path = PathRef::new(0);
        let edge = decom.translate_star(&p, &[path.to_segment()], 0);
        assert_eq!(edge, CatalogEdge::star(0, 0, 0))
    }

    #[test]
    fn test_decompose_path() {
        let catalog = build_test_catalog();
        let decom = HeuristicDecomposer::new(&catalog, 2, 999, 999, 0, true, true, true);
        let p1 =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        let mut path = PathRef::new(0);
        path.push(1, 0);
        path.push(2, 1);
        assert_eq!(
            decom.decompose_path(&p1, &path),
            vec![CatalogEdge::star(0, 4, 2)]
        );

        let mut path = PathRef::new(1);
        path.push(2, 1);
        assert_eq!(
            decom.decompose_path(&p1, &path),
            vec![CatalogEdge::star(1, 1, 1)]
        );

        let p2 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)],
            [(0, 0, 1, 0), (1, 1, 2, 0), (2, 2, 3, 0), (3, 0, 4, 0)],
        )
        .to_general()
        .unwrap();
        let mut path = PathRef::new(0);
        path.push(1, 0);
        path.push(2, 1);
        path.push(3, 2);
        assert_eq!(
            decom.decompose_path(&p2, &path),
            vec![CatalogEdge::path(0, 1, 0, 2), CatalogEdge::star(2, 1, 2)]
        );
    }

    #[test]
    fn test_path_split_at() {
        let mut p = PathRef::new(0);
        p.push(1, 0);
        p.push(2, 1);
        p.push(3, 2);
        let (first, second) = p.split_at(0);
        assert_eq!(
            first,
            PathSegment {
                vertices: &[0],
                edges: &[]
            }
        );
        assert_eq!(
            second,
            PathSegment {
                vertices: &[0, 1, 2, 3],
                edges: &[0, 1, 2]
            }
        );

        let (first, second) = p.split_at(2);
        assert_eq!(
            first,
            PathSegment {
                vertices: &[0, 1, 2],
                edges: &[0, 1]
            }
        );
        assert_eq!(
            second,
            PathSegment {
                vertices: &[2, 3],
                edges: &[2]
            }
        );

        let (first, second) = p.split_at(3);
        assert_eq!(
            first,
            PathSegment {
                vertices: &[0, 1, 2, 3],
                edges: &[0, 1, 2]
            }
        );
        assert_eq!(
            second,
            PathSegment {
                vertices: &[3],
                edges: &[]
            }
        );
    }

    #[test]
    fn test_find_pivots() {
        let p1 =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        assert!(find_pivots(&p1).is_empty());

        let p2 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0)],
            [(0, 0, 1, 0), (1, 0, 2, 0), (2, 1, 2, 0), (3, 2, 3, 0)],
        )
        .to_general()
        .unwrap();
        assert_eq!(find_pivots(&p2), vec![2]);

        let p3 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
            [
                (0, 0, 2, 0),
                (1, 1, 2, 0),
                (2, 2, 3, 0),
                (3, 2, 3, 0),
                (4, 3, 4, 0),
                (5, 3, 5, 0),
            ],
        )
        .to_general()
        .unwrap();
        assert_eq!(find_pivots(&p3), vec![2, 3]);
    }

    #[test]
    fn test_prune() {
        let catalog = build_test_catalog();
        let decom = HeuristicDecomposer::new(&catalog, 2, 999, 999, 0, true, true, true);
        let p1 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0)],
            [
                (0, 0, 1, 0),
                (1, 0, 2, 0),
                (2, 0, 3, 0),
                (3, 1, 2, 0),
                (4, 1, 3, 0),
                (5, 2, 3, 0),
            ],
        )
        .to_general()
        .unwrap();
        let p1_pruned = decom.prune(&p1);
        let expected = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0)],
            [
                // (0, 0, 1, 0),
                (1, 0, 2, 0),
                (2, 0, 3, 0),
                (3, 1, 2, 0),
                (4, 1, 3, 0),
                (5, 2, 3, 0),
            ],
        )
        .to_general()
        .unwrap();
        assert_eq!(p1_pruned.encode(), expected.encode());

        let p2 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)],
            [
                (0, 0, 1, 0),
                (1, 0, 2, 0),
                (2, 0, 3, 0),
                (3, 0, 4, 0),
                (4, 1, 2, 0),
                (5, 1, 3, 0),
                (6, 1, 4, 0),
                (7, 2, 3, 0),
                (8, 2, 4, 0),
                (9, 3, 4, 0),
            ],
        )
        .to_general()
        .unwrap();
        let p2_pruned = decom.prune(&p2);
        let expected = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)],
            [
                // (0, 0, 1, 0),
                // (1, 0, 2, 0),
                (2, 0, 3, 0),
                (3, 0, 4, 0),
                // (4, 1, 2, 0),
                (5, 1, 3, 0),
                (6, 1, 4, 0),
                (7, 2, 3, 0),
                (8, 2, 4, 0),
                (9, 3, 4, 0),
            ],
        )
        .to_general()
        .unwrap();
        assert_eq!(p2_pruned.encode(), expected.encode());
    }

    #[test]
    fn test_find_candidate_paths() {
        let p1 =
            RawPattern::with_vertices_edges([(0, 0), (1, 0), (2, 0)], [(0, 0, 1, 0), (1, 1, 2, 0)])
                .to_general()
                .unwrap();
        let expected = [(
            0,
            vec![PathRef {
                vertices: vec![0, 1, 2],
                edges: vec![0, 1],
            }],
        )]
        .into();
        assert_eq!(find_candidate_paths(&p1), expected);

        let p2 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0)],
            [(0, 0, 1, 0), (1, 0, 2, 0), (2, 1, 2, 0), (3, 2, 3, 0)],
        )
        .to_general()
        .unwrap();
        let expected = [(
            2,
            vec![
                PathRef {
                    vertices: vec![2, 3],
                    edges: vec![3],
                },
                PathRef {
                    vertices: vec![2, 0, 1, 2],
                    edges: vec![1, 0, 2],
                },
            ],
        )]
        .into();
        assert_eq!(find_candidate_paths(&p2), expected);

        let p3 = RawPattern::with_vertices_edges(
            [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
            [
                (0, 0, 2, 0),
                (1, 1, 2, 0),
                (2, 2, 3, 0),
                (3, 2, 3, 0),
                (4, 3, 4, 0),
                (5, 3, 5, 0),
            ],
        )
        .to_general()
        .unwrap();
        let expected = [
            (
                2,
                vec![
                    PathRef {
                        vertices: vec![2, 3],
                        edges: vec![2],
                    },
                    PathRef {
                        vertices: vec![2, 3],
                        edges: vec![3],
                    },
                    PathRef {
                        vertices: vec![2, 0],
                        edges: vec![0],
                    },
                    PathRef {
                        vertices: vec![2, 1],
                        edges: vec![1],
                    },
                ],
            ),
            (
                3,
                vec![
                    PathRef {
                        vertices: vec![3, 4],
                        edges: vec![4],
                    },
                    PathRef {
                        vertices: vec![3, 5],
                        edges: vec![5],
                    },
                ],
            ),
        ]
        .into();
        assert_eq!(find_candidate_paths(&p3), expected);
    }
}

#[test]
fn test() {
    let p1 =
        RawPattern::with_vertices_edges([(0, 9), (1, 8), (2, 9)], [(0, 1, 0, 22), (1, 1, 2, 22)])
            .to_general()
            .unwrap();

    let p2 =
        RawPattern::with_vertices_edges([(0, 9), (1, 8), (2, 9)], [(0, 1, 0, 22), (1, 1, 2, 22)])
            .to_path()
            .unwrap();

    println!(
        "{:?}, {:?}",
        p1.get_vertex_rank(0).unwrap(),
        p2.get_vertex_rank(0).unwrap()
    );
}
