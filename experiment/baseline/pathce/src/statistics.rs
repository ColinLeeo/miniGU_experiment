use std::ops::{AddAssign, Index, IndexMut};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use ahash::{HashMap, HashMapExt};
use bimap::BiHashMap;
use itertools::Itertools;
use log::{debug, trace};
use num::PrimInt;
use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator,
    IntoParallelRefMutIterator, ParallelIterator,
};
use serde::Serialize;

use crate::common::{DefaultVertexId, EdgeDirection, GlobalBucketMap, LabelId, TagId};
use crate::graph::{LabeledGraph, LabeledVertex};
use crate::pattern::{merge_paths_to_star, GeneralPattern, GraphPattern, PathPattern, RawPattern};
use crate::schema::{PathTreeNodeRef, Schema};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct CountVec<T>(Box<[T]>);

impl<T> AsRef<[T]> for CountVec<T> {
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T> AsMut<[T]> for CountVec<T> {
    fn as_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl<T: PrimInt> CountVec<T> {
    #[inline]
    pub fn zeroed(len: usize) -> Self {
        Self::with_value(T::zero(), len)
    }

    #[inline]
    pub fn with_value(value: T, len: usize) -> Self {
        Self(vec![value; len].into_boxed_slice())
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn into_inner(self) -> Box<[T]> {
        self.0
    }

    #[inline]
    pub fn maximum<U>(&mut self, rhs: &CountVec<U>)
    where
        U: PrimInt + Into<T>,
    {
        assert_eq!(self.len(), rhs.len());
        self.0.iter_mut().zip(rhs.0.iter()).for_each(|(a, &b)| {
            let b: T = b.into();
            *a = b.max(*a)
        })
    }
}

impl<T, U> AddAssign<&CountVec<U>> for CountVec<T>
where
    T: PrimInt + From<U> + AddAssign,
    U: PrimInt,
{
    #[inline]
    fn add_assign(&mut self, rhs: &CountVec<U>) {
        assert_eq!(self.0.len(), rhs.0.len());
        self.0
            .iter_mut()
            .zip(rhs.0.iter())
            .for_each(|(a, &b)| *a += b.into())
    }
}

impl<T> Index<usize> for CountVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.0.index(index)
    }
}

impl<T> IndexMut<usize> for CountVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.0.index_mut(index)
    }
}

trait Transpose {
    fn transpose(self) -> Self;
}

impl<T: Sync> Transpose for Vec<Box<[T]>> {
    fn transpose(mut self) -> Self {
        let dim: usize = self.len();
        assert!(self.iter().all(|row| row.len() == dim));
        for i in 0..(dim - 1) {
            for j in (i + 1)..dim {
                let pa = ptr::addr_of_mut!(self[i][j]);
                let pb = ptr::addr_of_mut!(self[j][i]);
                // SAFETY: `pa` and `pb` have been created from safe mutable references and refer
                // to elements in the slice and therefore are guaranteed to be valid and aligned.
                // Note that accessing the elements behind `a` and `b` is checked and will
                // panic when out of bounds.
                unsafe {
                    ptr::swap(pa, pb);
                }
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathStatistics {
    pub path: PathPattern,
    pub count: Vec<Box<[u64]>>,
    pub start_max_degree: Vec<Box<[u64]>>,
    pub end_max_degree: Vec<Box<[u64]>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathStatisticsInner {
    path: PathPattern,
    count: Option<Vec<Box<[u64]>>>,
    start_max_degree: Option<Vec<Box<[u64]>>>,
    end_max_degree: Option<Vec<Box<[u64]>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StarStatistics {
    pub star: GeneralPattern,
    pub center_rank: TagId,
    pub count: Vec<u64>,
    pub max_degree: Vec<u64>,
}

#[derive(Debug)]
pub struct StatisticsAnalyzer {
    graph: Arc<LabeledGraph>,
    schema: Arc<Schema>,
    bucket_map: Arc<GlobalBucketMap>,
    buckets: usize,
    max_path_length: usize,
    max_star_length: usize,
    max_star_degree: usize,
    bucket_values: OnceLock<HashMap<LabelId, Vec<Vec<usize>>>>,
}

type StarState = HashMap<LabelId, HashMap<(TagId, Vec<u8>), (PathPattern, CountVec<u64>)>>;

impl StatisticsAnalyzer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        graph: Arc<LabeledGraph>,
        schema: Arc<Schema>,
        bucket_map: Arc<GlobalBucketMap>,
        buckets: usize,
        max_path_length: usize,
        max_star_length: usize,
        max_star_degree: usize,
    ) -> Self {
        Self {
            graph,
            schema,
            bucket_map,
            buckets,
            max_path_length,
            max_star_length,
            max_star_degree,
            bucket_values: OnceLock::new(),
        }
    }

    pub fn compute_star_statistics(&self) -> HashMap<(TagId, Vec<u8>), StarStatistics> {
        self.compute_bucket_values();
        let mut state = StarState::new();
        // NOTE: We compute star statistics for endpoints of all the k-paths
        for i in 0..=self.max_path_length {
            let start = Instant::now();
            self.update_star_state(&mut state, i);
            debug!(
                "compute star state for {i}-path: {} s",
                start.elapsed().as_secs_f64()
            );
        }
        let mut star_statistics = HashMap::new();

        for v in self.schema.vertices() {
            let current_star_state = state.get(&v.label).unwrap();
            // Validation
            assert!(current_star_state
                .values()
                .all(|(p, _)| p.start().label_id() == v.label));
            assert!(current_star_state.iter().all(|((rank, _), (path, _))| {
                let start_rank = path.get_vertex_rank(path.start().tag_id()).unwrap();
                *rank == start_rank
            }));

            // Handle single-vertex path
            let (vertex_path, count_vec) = current_star_state
                .values()
                .find(|(path, _)| path.is_empty())
                .unwrap();
            let bucket_values = self.bucket_values.get().unwrap().get(&v.label).unwrap();
            let vertex_map = self.graph.get_internal_vertex_map(v.label).unwrap();
            let count = self.summarize_count_for_vec(count_vec, vertex_map, bucket_values);
            let max_degree =
                self.summarize_max_degree_for_vec(count_vec, vertex_map, bucket_values);
            let center_rank = vertex_path.get_vertex_rank(0).unwrap();
            star_statistics.insert(
                (center_rank, vertex_path.encode()),
                StarStatistics {
                    star: vertex_path.clone().into(),
                    center_rank,
                    count,
                    max_degree,
                },
            );

            self.combine_star_states_for_paths(current_star_state, &mut star_statistics);

            for i in 1..=self.max_star_degree {
                self.combine_star_states_for_stars(
                    v.label,
                    i,
                    current_star_state,
                    &mut star_statistics,
                );
            }
        }
        star_statistics
    }

    #[allow(clippy::type_complexity)]
    fn combine_star_states_for_paths(
        &self,
        state: &HashMap<(u8, Vec<u8>), (PathPattern, CountVec<u64>)>,
        stats: &mut HashMap<(TagId, Vec<u8>), StarStatistics>,
    ) {
        // Compute star stats for endpoints of all paths
        for (path, vec) in state.values().filter(|(path, _)| !path.is_empty()) {
            let label_id = path.start().label_id();
            let center_rank = path.get_vertex_rank(path.start().tag_id()).unwrap();
            trace!(
                "compute star stats for path: {}, center_tag: {}, center_rank: {}",
                path,
                path.start().tag_id(),
                center_rank
            );
            stats
                .entry((center_rank, path.encode()))
                .or_insert_with(|| {
                    let bucket_values = self.bucket_values.get().unwrap().get(&label_id).unwrap();
                    let vertex_map = self.graph.get_internal_vertex_map(label_id).unwrap();
                    let count = self.summarize_count_for_vec(vec, vertex_map, bucket_values);
                    let max_degree =
                        self.summarize_max_degree_for_vec(vec, vertex_map, bucket_values);
                    StarStatistics {
                        star: path.clone().into(),
                        center_rank,
                        count,
                        max_degree,
                    }
                });
            if path.is_symmetric() {
                let mut stat = stats.get(&(center_rank, path.encode())).unwrap().clone();
                let center_rank = path.get_vertex_rank(path.end().tag_id()).unwrap();
                stat.center_rank = center_rank;
                stats.entry((center_rank, path.encode())).or_insert(stat);
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn combine_star_states_for_stars(
        &self,
        label_id: LabelId,
        degree: usize,
        state: &HashMap<(u8, Vec<u8>), (PathPattern, CountVec<u64>)>,
        stats: &mut HashMap<(TagId, Vec<u8>), StarStatistics>,
    ) {
        // Handle real stars
        for comb in state
            .values()
            .map(|(path, vec)| (path, vec))
            .filter(|(path, _)| !path.is_empty() && path.len() <= self.max_star_length)
            .combinations(degree)
        {
            let (paths, vecs): (Vec<_>, Vec<_>) = comb.into_iter().unzip();
            assert!(vecs.iter().map(|v| v.len()).all_equal());
            let (star, center_rank) = merge_paths_to_star(&paths);
            stats
                .entry((center_rank, star.encode()))
                .or_insert_with(|| {
                    let (first, other) = vecs.split_first().unwrap();
                    let mut vec = (*first).clone();
                    vec.as_mut()
                        .par_iter_mut()
                        .enumerate()
                        .for_each(|(idx, count)| {
                            *count = other.iter().map(|v| v[idx]).fold(*count, |a, b| a * b);
                        });

                    let bucket_values = self.bucket_values.get().unwrap().get(&label_id).unwrap();
                    let vertex_map = self.graph.get_internal_vertex_map(label_id).unwrap();
                    let count = self.summarize_count_for_vec(&vec, vertex_map, bucket_values);
                    let max_degree =
                        self.summarize_max_degree_for_vec(&vec, vertex_map, bucket_values);
                    StarStatistics {
                        star,
                        center_rank,
                        count,
                        max_degree,
                    }
                });
        }
    }

    fn update_star_state_inner(
        &self,
        source_label: LabelId,
        current_length: usize,
        state: &mut StarState,
        direction: EdgeDirection,
    ) {
        let schema_edges: Box<dyn Iterator<Item = _>> = match direction {
            EdgeDirection::Out => Box::new(self.schema.outgoing_edges(source_label).unwrap()),
            EdgeDirection::In => Box::new(self.schema.incoming_edges(source_label).unwrap()),
        };
        let vertex_map = self.graph.get_internal_vertex_map(source_label).unwrap();
        for e in schema_edges {
            let start_label = match direction {
                EdgeDirection::Out => e.to,
                EdgeDirection::In => e.from,
            };
            let mut suffixes = self
                .schema
                .generate_paths_from_vertex(start_label, current_length - 1);
            trace!("generate {} suffixes from {}", suffixes.len(), start_label);
            // Handle symmetric suffixes
            let symmetric_suffixes = suffixes
                .iter()
                .filter_map(|s| (s.start().label_id() == s.end().label_id()).then_some(s.reverse()))
                .collect_vec();
            suffixes.extend(symmetric_suffixes);
            trace!(
                "generate {} suffixes from {} (with symmetric suffixes)",
                suffixes.len(),
                start_label
            );
            for suffix in suffixes {
                let start = suffix.start();
                assert_eq!(start.label_id(), start_label);
                let start_rank = suffix.get_vertex_rank(start.tag_id()).unwrap();
                let start_star_state = state.get(&start.label_id()).unwrap();
                let (_, start_count_vec) = start_star_state
                    .get(&(start_rank, suffix.encode()))
                    .unwrap();
                let start_vertex_map = self
                    .graph
                    .get_internal_vertex_map(start.label_id())
                    .unwrap();
                let mut count_vec = CountVec::zeroed(vertex_map.len());
                count_vec
                    .as_mut()
                    .par_iter_mut()
                    .enumerate()
                    .for_each(|(internal_id, count)| {
                        let vertex_id = vertex_map.get_by_right(&(internal_id as u32)).unwrap();
                        let neighbors = match direction {
                            EdgeDirection::Out => self
                                .graph
                                .outgoing_neighbors(
                                    LabeledVertex::new(*vertex_id, source_label),
                                    e.label,
                                )
                                .unwrap(),
                            EdgeDirection::In => self
                                .graph
                                .incoming_neighbors(
                                    LabeledVertex::new(*vertex_id, source_label),
                                    e.label,
                                )
                                .unwrap(),
                        };
                        *count += neighbors
                            .par_iter()
                            .map(|neighbor_id| {
                                let neighbor_internal_id =
                                    start_vertex_map.get_by_left(neighbor_id).unwrap();
                                start_count_vec[*neighbor_internal_id as usize]
                            })
                            .sum::<u64>();
                    });
                let old_start_tag_id = suffix.start().tag_id();
                let mut path = RawPattern::from(suffix);
                let next_vertex_tag_id = path.next_vertex_tag_id();
                let next_edge_tag_id = path.next_edge_tag_id();
                path.push_front_vertex((next_vertex_tag_id, source_label));
                match direction {
                    EdgeDirection::Out => path.push_front_edge((
                        next_edge_tag_id,
                        next_vertex_tag_id,
                        old_start_tag_id,
                        e.label,
                    )),
                    EdgeDirection::In => path.push_front_edge((
                        next_edge_tag_id,
                        old_start_tag_id,
                        next_vertex_tag_id,
                        e.label,
                    )),
                };
                let path = path.to_path().unwrap();
                assert_eq!(path.len(), current_length);
                let rank = path.get_vertex_rank(next_vertex_tag_id).unwrap();
                let current_star_state = state.entry(source_label).or_default();
                trace!("save star state for {path}, rank: {rank}, tag: {next_vertex_tag_id}");
                current_star_state
                    .entry((rank, path.encode()))
                    .or_insert((path, count_vec));
            }
        }
    }

    fn update_star_state(&self, state: &mut StarState, current_length: usize) {
        for v in self.schema.vertices() {
            if current_length == 0 {
                let current_star_state = state.entry(v.label).or_default();
                let path = RawPattern::new()
                    .push_back_vertex((0, v.label))
                    .to_path()
                    .unwrap();
                let rank = path.get_vertex_rank(0).unwrap();
                let vertex_map = self.graph.get_internal_vertex_map(v.label).unwrap();
                let count_vec = CountVec::with_value(1, vertex_map.len());
                current_star_state
                    .entry((rank, path.encode()))
                    .or_insert((path, count_vec));
            } else {
                self.update_star_state_inner(v.label, current_length, state, EdgeDirection::Out);
                self.update_star_state_inner(v.label, current_length, state, EdgeDirection::In);
            }
        }
    }

    pub fn compute_path_statistics(&self) -> HashMap<Vec<u8>, PathStatistics> {
        self.compute_bucket_values();

        let start = Instant::now();
        let mut results = self.init_path_statistics();
        debug!("init path results: {} s", start.elapsed().as_secs_f64());

        let start = Instant::now();
        for v in self.schema.vertices() {
            let path = RawPattern::new()
                .push_back_vertex((0, v.label))
                .to_path()
                .unwrap();
            let tree = self
                .schema
                .generate_path_tree_from_path_end(&path, self.max_path_length);
            let count_matrix = self.init_path_count_matrix_for_vertex(v.label);
            let vertex_map = self.graph.get_internal_vertex_map(v.label).unwrap();
            for child in tree.root().children() {
                self.compute_path_statistics_recursive(
                    child,
                    vertex_map,
                    &count_matrix,
                    0,
                    &mut results,
                );
            }
        }
        debug!("summarize path: {} s", start.elapsed().as_secs_f64());

        // Validation
        let start = Instant::now();
        for stat in results.values_mut() {
            assert!(stat.count.is_some());
            assert!(stat.end_max_degree.is_some());
            // The path must be symmetric
            if stat.start_max_degree.is_none() {
                stat.start_max_degree = stat.end_max_degree.clone()
            }
        }
        debug!("validate path: {} s", start.elapsed().as_secs_f64());

        results
            .into_iter()
            .map(
                |(
                    code,
                    PathStatisticsInner {
                        path,
                        count,
                        start_max_degree,
                        end_max_degree,
                    },
                )| {
                    (
                        code,
                        PathStatistics {
                            path,
                            count: count.unwrap(),
                            start_max_degree: start_max_degree.unwrap(),
                            end_max_degree: end_max_degree.unwrap(),
                        },
                    )
                },
            )
            .collect()
    }

    fn compute_bucket_values(&self) {
        self.bucket_values.get_or_init(|| {
            let start = Instant::now();
            let bucket_values = self
                .schema
                .vertices()
                .par_iter()
                .cloned()
                .map(|v| (v.label, self.compute_bucket_values_for_label(v.label)))
                .collect();
            debug!("compute bucket values: {} s", start.elapsed().as_secs_f64());
            bucket_values
        });
    }

    fn compute_bucket_values_for_label(&self, vertex_label: LabelId) -> Vec<Vec<DefaultVertexId>> {
        let mut bucket_values = vec![vec![]; self.buckets];
        let bucket_values_mutex = bucket_values.iter_mut().map(Mutex::new).collect_vec();
        let bucket_map = self.bucket_map.get(&vertex_label).unwrap();
        bucket_map.par_iter().for_each(|(vertex_id, bucket_id)| {
            bucket_values_mutex
                .get(*bucket_id)
                .unwrap()
                .lock()
                .unwrap()
                .push(*vertex_id);
        });
        bucket_values
    }

    fn init_path_statistics(&self) -> HashMap<Vec<u8>, PathStatisticsInner> {
        let mut results = HashMap::new();
        for v in self.schema.vertices() {
            let path = RawPattern::new()
                .push_back_vertex((0, v.label))
                .to_path()
                .unwrap();
            let tree = self
                .schema
                .generate_path_tree_from_path_end(&path, self.max_path_length);
            let mut queue = tree.root().children();
            while let Some(node) = queue.pop() {
                let path = node.path().clone();
                results
                    .entry(path.encode())
                    .or_insert_with(|| PathStatisticsInner {
                        path,
                        count: None,
                        start_max_degree: None,
                        end_max_degree: None,
                    });
                queue.extend(node.children());
            }
        }
        results
    }

    fn init_path_count_matrix(&self, len: usize) -> Vec<CountVec<u64>> {
        let vec = CountVec::zeroed(self.buckets);
        (0..len).into_par_iter().map(|_| vec.clone()).collect()
    }

    fn init_path_count_matrix_for_vertex(&self, vertex_label: LabelId) -> Vec<CountVec<u64>> {
        let vertex_map = self.graph.get_internal_vertex_map(vertex_label).unwrap();
        let bucket_map = self.bucket_map.get(&vertex_label).unwrap();
        let mut count_matrix = self.init_path_count_matrix(vertex_map.len());
        count_matrix
            .par_iter_mut()
            .enumerate()
            .for_each(|(internal_id, count_vec)| {
                let vertex_id = vertex_map.get_by_right(&(internal_id as u32)).unwrap();
                let bucket_id = bucket_map.get(vertex_id).unwrap();
                count_vec[*bucket_id] = 1;
            });
        count_matrix
    }

    fn summarize_count_for_vec(
        &self,
        count_vec: &CountVec<u64>,
        vertex_map: &BiHashMap<usize, u32>,
        bucket_values: &[Vec<DefaultVertexId>],
    ) -> Vec<u64> {
        bucket_values
            .into_par_iter()
            .map(|values| {
                values
                    .par_iter()
                    .map(|vertex_id| {
                        let internal_id = vertex_map.get_by_left(vertex_id).unwrap();
                        count_vec[*internal_id as usize]
                    })
                    .sum()
            })
            .collect()
    }

    fn summarize_count(
        &self,
        count_matrix: &[CountVec<u64>],
        vertex_map: &BiHashMap<usize, u32>,
        bucket_values: &[Vec<DefaultVertexId>],
    ) -> Vec<Box<[u64]>> {
        bucket_values
            .into_par_iter()
            .map(|values| {
                values
                    .par_iter()
                    .map(|vertex_id| {
                        let internal_id = vertex_map.get_by_left(vertex_id).unwrap();
                        count_matrix.get(*internal_id as usize).unwrap()
                    })
                    .fold(
                        || CountVec::zeroed(self.buckets),
                        |mut a, b| {
                            a += b;
                            a
                        },
                    )
                    .reduce(
                        || CountVec::zeroed(self.buckets),
                        |mut a, b| {
                            a += &b;
                            a
                        },
                    )
                    .into_inner()
            })
            .collect()
    }

    fn summarize_max_degree(
        &self,
        count_matrix: &[CountVec<u64>],
        vertex_map: &BiHashMap<usize, u32>,
        bucket_values: &[Vec<DefaultVertexId>],
    ) -> Vec<Box<[u64]>> {
        bucket_values
            .into_par_iter()
            .map(|values| {
                values
                    .par_iter()
                    .map(|vertex_id| {
                        let internal_id = vertex_map.get_by_left(vertex_id).unwrap();
                        count_matrix.get(*internal_id as usize).unwrap()
                    })
                    .fold(
                        || CountVec::zeroed(self.buckets),
                        |mut a, b| {
                            a.maximum(b);
                            a
                        },
                    )
                    .reduce(
                        || CountVec::zeroed(self.buckets),
                        |mut a, b| {
                            a.maximum(&b);
                            a
                        },
                    )
                    .into_inner()
            })
            .collect()
    }

    fn summarize_max_degree_for_vec(
        &self,
        count_vec: &CountVec<u64>,
        vertex_map: &BiHashMap<usize, u32>,
        bucket_values: &[Vec<DefaultVertexId>],
    ) -> Vec<u64> {
        bucket_values
            .into_par_iter()
            .map(|values| {
                values
                    .par_iter()
                    .map(|vertex_id| {
                        let internal_id = vertex_map.get_by_left(vertex_id).unwrap();
                        count_vec[*internal_id as usize]
                    })
                    .max()
                    .unwrap_or_default()
            })
            .collect()
    }

    fn compute_path_statistics_recursive(
        &self,
        node: PathTreeNodeRef,
        parent_vertex_map: &BiHashMap<DefaultVertexId, u32>,
        parent_count_matrix: &[CountVec<u64>],
        parent_vertex_tag_id: TagId,
        results: &mut HashMap<Vec<u8>, PathStatisticsInner>,
    ) {
        let path = node.path();
        let edge = path.get_edge(path.max_edge_tag_id().unwrap()).unwrap();
        let vertex = path.get_vertex(path.max_vertex_tag_id().unwrap()).unwrap();
        let vertex_map = self
            .graph
            .get_internal_vertex_map(vertex.label_id())
            .unwrap();
        let mut count_matrix = self.init_path_count_matrix(vertex_map.len());
        let direction = if edge.src() == parent_vertex_tag_id {
            EdgeDirection::In
        } else if edge.dst() == parent_vertex_tag_id {
            EdgeDirection::Out
        } else {
            unreachable!()
        };
        count_matrix
            .par_iter_mut()
            .enumerate()
            .for_each(|(internal_id, count_vec)| {
                let vertex_id = vertex_map.get_by_right(&(internal_id as u32)).unwrap();
                self.graph
                    .neighbors(
                        LabeledVertex::new(*vertex_id, vertex.label_id()),
                        edge.label_id(),
                        direction,
                    )
                    .unwrap()
                    .iter()
                    .map(|nbr_id| {
                        let nbr_internal_id = parent_vertex_map.get_by_left(nbr_id).unwrap();
                        parent_count_matrix.get(*nbr_internal_id as usize).unwrap()
                    })
                    .for_each(|nbr_count_vec| {
                        *count_vec += nbr_count_vec;
                    })
            });
        let local_bucket_values = self
            .bucket_values
            .get()
            .unwrap()
            .get(&vertex.label_id())
            .unwrap();
        let statistics = results.get_mut(&path.encode()).unwrap();
        let vertex_rank = path.get_vertex_rank(vertex.tag_id()).unwrap();
        let path_vertex = statistics.path.get_vertex_from_rank(vertex_rank).unwrap();
        if path_vertex == statistics.path.start() {
            statistics.count.get_or_insert_with(|| {
                self.summarize_count(&count_matrix, vertex_map, local_bucket_values)
            });
            statistics.start_max_degree.get_or_insert_with(|| {
                self.summarize_max_degree(&count_matrix, vertex_map, local_bucket_values)
            });
        } else if path_vertex == statistics.path.end() {
            statistics.count.get_or_insert_with(|| {
                self.summarize_count(&count_matrix, vertex_map, local_bucket_values)
                    .transpose()
            });
            statistics.end_max_degree.get_or_insert_with(|| {
                self.summarize_max_degree(&count_matrix, vertex_map, local_bucket_values)
                    .transpose()
            });
        } else {
            unreachable!()
        }

        for child in node.children() {
            self.compute_path_statistics_recursive(
                child,
                vertex_map,
                &count_matrix,
                vertex.tag_id(),
                results,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{build_bucket_map, build_ldbc_graph, build_ldbc_schema};

    #[test]
    fn test_init_count_matrix() {
        let schema = Arc::new(build_ldbc_schema());
        let graph = Arc::new(build_ldbc_graph());
        let bucket_map = Arc::new(build_bucket_map(&graph, 4));
        let analyzer = StatisticsAnalyzer::new(
            graph.clone(),
            schema.clone(),
            bucket_map.clone(),
            4,
            2,
            2,
            4,
        );
        let mat = analyzer.init_path_count_matrix_for_vertex(4);
        let mut expected = vec![CountVec::zeroed(4); graph.vertices(4).unwrap().len()];
        let vertex_map = graph.get_internal_vertex_map(4).unwrap();
        let local_bucket_map = bucket_map.get(&4).unwrap();
        for v in graph.vertices(4).unwrap() {
            let internal_id = vertex_map.get_by_left(v).unwrap();
            let bucket_id = local_bucket_map.get(v).unwrap();
            expected[*internal_id as usize][*bucket_id] = 1;
        }
        assert_eq!(mat, expected)
    }

    #[test]
    fn test_compute_bucket_values() {
        let schema = Arc::new(build_ldbc_schema());
        let graph = Arc::new(build_ldbc_graph());
        let bucket_map = Arc::new(build_bucket_map(&graph, 4));
        let analyzer = StatisticsAnalyzer::new(
            graph.clone(),
            schema.clone(),
            bucket_map.clone(),
            4,
            2,
            2,
            4,
        );
        let bucket_values = analyzer.compute_bucket_values_for_label(6);
        let bucket_map = bucket_map.get(&6).unwrap();
        assert_eq!(bucket_values.len(), 4);
        assert!(bucket_values
            .into_iter()
            .enumerate()
            .all(|(bucket_id, values)| {
                values
                    .into_iter()
                    .all(|v| *bucket_map.get(&v).unwrap() == bucket_id)
            }));
    }

    #[test]
    fn test_compute() {
        let schema = Arc::new(build_ldbc_schema());
        let graph = Arc::new(build_ldbc_graph());
        let num_buckets = 2;
        let bucket_map = Arc::new(build_bucket_map(&graph, num_buckets));
        let analyzer = StatisticsAnalyzer::new(
            graph.clone(),
            schema.clone(),
            bucket_map.clone(),
            num_buckets,
            2,
            2,
            4,
        );
        let results = analyzer.compute_path_statistics();
        for (i, stat) in results.into_values().enumerate() {
            println!(
                "[{i}] {}, count: {:?}, start_max_degree: {:?}, end_max_degree: {:?}",
                stat.path, stat.count, stat.start_max_degree, stat.end_max_degree,
            )
        }
    }
}
