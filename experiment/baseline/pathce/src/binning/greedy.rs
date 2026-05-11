use ahash::{HashMap, HashMapExt};
use itertools::Itertools;

use crate::common::{BucketId, DefaultVertexId, LocalBucketMap, VertexId};
use crate::factorization::ColumnRef;

type BucketValuesMap = HashMap<BucketId, Vec<(DefaultVertexId, usize)>>;

#[derive(Debug, Clone)]
pub struct GreedyBinner {
    budget: usize,
    current_num_buckets: usize,
    bucket_map: LocalBucketMap,
}

impl GreedyBinner {
    pub fn new<I>(budget: usize, vertices: I) -> Self
    where
        I: IntoIterator<Item = DefaultVertexId> + Clone,
    {
        let initial_budget = budget.div_ceil(2);
        let budget = budget - initial_budget;
        let bucket_map = build_initial_bucket_map(initial_budget, vertices);
        let current_num_buckets = initial_budget;
        Self {
            budget,
            current_num_buckets,
            bucket_map,
        }
    }

    pub fn should_finish(&self) -> bool {
        self.budget == 0
    }

    pub fn update(&mut self, vertex_column: &ColumnRef, neighbor_column: &ColumnRef) {
        const PK_THRESHOLD: f64 = 0.99;
        assert_eq!(vertex_column.num_items(), neighbor_column.num_items());
        if self.budget == 0 {
            return;
        }
        let count_map = compute_count_map(self.bucket_map.len(), vertex_column, neighbor_column);
        // Ignore PK column
        if count_map.values().filter(|c| **c == 1).count() as f64
            > PK_THRESHOLD * count_map.len() as f64
        {
            return;
        }
        let bucket_values_map = compute_bucket_values(&count_map, &self.bucket_map);
        let bucket_count_mean_variance = compute_bucket_count_mean_variance(&bucket_values_map);

        let num_buckets_to_add = if self.budget >= 2 { self.budget / 2 } else { 1 };
        let bucket_split_num_map =
            compute_bucket_split_num(&bucket_count_mean_variance, num_buckets_to_add);
        let new_num_buckets = split_buckets(
            self.current_num_buckets,
            &mut self.bucket_map,
            &bucket_values_map,
            &bucket_split_num_map,
        );
        let num_buckets_added = new_num_buckets - self.current_num_buckets;
        assert!(num_buckets_added <= num_buckets_to_add);
        self.budget -= num_buckets_added;
        self.current_num_buckets = new_num_buckets;
    }

    pub fn finish(self) -> LocalBucketMap {
        self.bucket_map
    }
}

fn split_buckets(
    mut num_buckets: usize,
    bucket_map: &mut LocalBucketMap,
    bucket_values_map: &BucketValuesMap,
    bucket_split_num_map: &HashMap<BucketId, usize>,
) -> usize {
    for (bucket_id, bucket_split_num) in bucket_split_num_map {
        let bucket_values = bucket_values_map.get(bucket_id).unwrap();
        let bucket_values_counts = bucket_values
            .iter()
            .map(|(_, count)| *count as f64)
            .collect_vec();
        let lowerbounds =
            ckmeans::ckmeans_lowerbound(&bucket_values_counts, *bucket_split_num as u8 + 1)
                .unwrap();
        assert!(!lowerbounds.is_empty());
        assert!(lowerbounds.len() <= bucket_split_num + 1);
        let mut current_lowerbound_idx = 0;
        for (value, count) in bucket_values {
            if current_lowerbound_idx != lowerbounds.len() - 1
                && *count as f64 >= lowerbounds[current_lowerbound_idx + 1]
            {
                current_lowerbound_idx += 1;
            }
            if current_lowerbound_idx == 0 {
                continue;
            }
            let new_bucket_id = num_buckets + current_lowerbound_idx - 1;
            *bucket_map.get_mut(value).unwrap() = new_bucket_id;
        }
        num_buckets += current_lowerbound_idx;
    }
    num_buckets
}

/// Compute the best-effort bucket split num.
fn compute_bucket_split_num(
    bucket_count_mean_variance: &HashMap<BucketId, (usize, f64, f64)>,
    mut budget: usize,
) -> HashMap<BucketId, usize> {
    const SMALL_VARIANCE_THRESHOLD: f64 = 2.;
    assert_ne!(budget, 0);

    let mut bucket_split_num = HashMap::with_capacity(bucket_count_mean_variance.len());
    // Return if there is not a bucket whose variance is large enough.
    if !bucket_count_mean_variance
        .values()
        .any(|(_, _, variance)| *variance > SMALL_VARIANCE_THRESHOLD)
    {
        return bucket_split_num;
    }

    let bucket_ids = bucket_count_mean_variance
        .keys()
        .copied()
        .sorted_unstable_by(|b1, b2| {
            let v1 = bucket_count_mean_variance.get(b1).unwrap().2;
            let v2 = bucket_count_mean_variance.get(b2).unwrap().2;
            v2.total_cmp(&v1)
        })
        .collect_vec();

    'outer: while budget > 0 {
        let old_budget = budget;
        for bucket_id in bucket_ids.iter() {
            let (count, _, variance) = bucket_count_mean_variance.get(bucket_id).unwrap();
            if *variance > SMALL_VARIANCE_THRESHOLD {
                let split_num = bucket_split_num.entry(*bucket_id).or_default();
                if *split_num + 1 == *count {
                    // Continue if there is not enough value to divide into `split_num + 2` buckets.
                    continue;
                }
                *split_num += 1;
                budget -= 1;

                if budget == 0 {
                    break 'outer;
                }
            }
        }
        if budget == old_budget {
            // Jump out if the target budget cannot be achieved
            break;
        }
    }

    bucket_split_num.retain(|_, split_num| *split_num > 0);
    bucket_split_num
}

fn compute_count_map(
    vertex_count: usize,
    vertex_column: &ColumnRef,
    neighbor_column: &ColumnRef,
) -> HashMap<DefaultVertexId, usize> {
    let mut count_map = HashMap::with_capacity(vertex_count);
    for (vertices, neighbors) in vertex_column.items().zip(neighbor_column.items()) {
        let multiplicity = neighbors.iter().filter(|v| v.is_valid()).count();
        if multiplicity == 0 {
            continue;
        }
        vertices.iter().filter(|v| v.is_valid()).for_each(|v| {
            *count_map.entry(*v).or_default() += multiplicity;
        })
    }
    count_map
}

fn compute_bucket_values(
    count_map: &HashMap<DefaultVertexId, usize>,
    bucket_map: &LocalBucketMap,
) -> BucketValuesMap {
    let mut bucket_values: HashMap<_, Vec<_>> = HashMap::new();
    for (vertex, count) in count_map {
        let bucket_id = bucket_map.get(vertex).unwrap();
        bucket_values
            .entry(*bucket_id)
            .or_default()
            .push((*vertex, *count));
    }
    // Make sure the bucket values are sorted by count.
    for values in bucket_values.values_mut() {
        values.sort_unstable_by_key(|(value, count)| (*count, *value));
    }
    bucket_values
}

fn compute_bucket_count_mean_variance(
    bucket_values: &BucketValuesMap,
) -> HashMap<BucketId, (usize, f64, f64)> {
    bucket_values
        .iter()
        .map(|(bucket_id, values)| {
            let sum: usize = values.iter().map(|(_, count)| *count).sum();
            let square_sum: usize = values.iter().map(|(_, count)| count * count).sum();
            let mean = sum as f64 / values.len() as f64;
            let variance = square_sum as f64 / values.len() as f64 - mean * mean;
            (*bucket_id, (values.len(), mean, variance))
        })
        .collect()
}

fn build_initial_bucket_map<I>(budget: usize, vertices: I) -> LocalBucketMap
where
    I: IntoIterator<Item = DefaultVertexId> + Clone,
{
    let vertex_count = vertices.clone().into_iter().count();
    let big_bucket_count = vertex_count % budget;
    let big_bucket_size = vertex_count.div_ceil(budget);
    let small_bucket_size = vertex_count / budget;
    let mut bucket_map = HashMap::with_capacity(vertex_count);

    for (bucket_id, bucket_values) in vertices
        .clone()
        .into_iter()
        .take(big_bucket_size * big_bucket_count)
        .chunks(big_bucket_size)
        .into_iter()
        .enumerate()
    {
        for value in bucket_values {
            bucket_map.insert(value, bucket_id);
        }
    }
    if small_bucket_size > 0 {
        for (mut bucket_id, bucket_values) in vertices
            .into_iter()
            .skip(big_bucket_count * big_bucket_size)
            .chunks(small_bucket_size)
            .into_iter()
            .enumerate()
        {
            bucket_id += big_bucket_count;
            for value in bucket_values {
                bucket_map.insert(value, bucket_id);
            }
        }
    }
    assert_eq!(bucket_map.len(), vertex_count);
    bucket_map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_initial_bucket_map() {
        let bucket_map = build_initial_bucket_map(4, (0..10).collect_vec());
        let expected = [
            (0, 0),
            (1, 0),
            (2, 0),
            (3, 1),
            (4, 1),
            (5, 1),
            (6, 2),
            (7, 2),
            (8, 3),
            (9, 3),
        ]
        .into_iter()
        .collect();
        assert_eq!(bucket_map, expected);
    }

    #[test]
    fn test_compute_bucket_values() {
        let count_map = [(1, 100), (2, 1), (3, 1), (6, 2), (8, 3), (11, 1), (13, 1)]
            .into_iter()
            .collect();
        let bucket_map = [
            (1, 0),
            (2, 0),
            (3, 0),
            (4, 0),
            (5, 0),
            (6, 1),
            (7, 1),
            (8, 1),
            (9, 1),
            (10, 2),
            (11, 2),
            (13, 2),
        ]
        .into_iter()
        .collect();

        let bucket_values = compute_bucket_values(&count_map, &bucket_map);
        let expected = [
            (0, vec![(2, 1), (3, 1), (1, 100)]),
            (1, vec![(6, 2), (8, 3)]),
            (2, vec![(11, 1), (13, 1)]),
        ]
        .into_iter()
        .collect();
        assert_eq!(bucket_values, expected)
    }

    #[test]
    fn test_compute_bucket_count_mean_variance() {
        let bucket_values = [
            (0, vec![(2, 1), (3, 1), (1, 100)]),
            (1, vec![(6, 2), (8, 3)]),
            (2, vec![(11, 1), (13, 1)]),
        ]
        .into_iter()
        .collect();
        let bucket_mean_variance_map = compute_bucket_count_mean_variance(&bucket_values);
        let expected = [(0, (3, 34., 2178.)), (1, (2, 2.5, 0.25)), (2, (2, 1., 0.))]
            .into_iter()
            .collect();
        assert_eq!(bucket_mean_variance_map, expected);
    }

    #[test]
    fn test_compute_bucket_split_num() {
        let bucket_count_mean_variance =
            [(0, (3, 34., 2178.)), (1, (2, 2.5, 3.)), (2, (2, 1., 0.))]
                .into_iter()
                .collect();
        let result = compute_bucket_split_num(&bucket_count_mean_variance, 4);
        let expected = [(0, 2), (1, 1)].into_iter().collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_split_buckets() {
        let mut bucket_map = [
            (1, 0),
            (2, 0),
            (3, 0),
            (4, 0),
            (5, 0),
            (6, 1),
            (7, 1),
            (8, 1),
            (9, 1),
            (10, 2),
            (11, 2),
            (12, 2),
            (13, 2),
        ]
        .into_iter()
        .collect();
        let bucket_values_map = [
            (0, vec![(2, 1), (3, 1), (1, 100)]),
            (1, vec![(6, 2), (8, 3)]),
            (2, vec![(11, 1), (13, 1)]),
        ]
        .into_iter()
        .collect();
        let bucket_split_num_map = [(0, 2), (1, 1)].into_iter().collect();
        split_buckets(
            3,
            &mut bucket_map,
            &bucket_values_map,
            &bucket_split_num_map,
        );
        assert_eq!(bucket_map.values().unique().count(), 5);
    }

    // #[test]
    // fn test_greedy_binning() {
    //     let mut binner = GreedyBinner::new(8, 1..=13);
    //     let mut vertex_column = FactorizedColumn::new();
    //     vertex_column.extend_one(1);
    //     vertex_column.extend_one(2);
    //     vertex_column.extend_one(3);
    //     vertex_column.extend_one(6);
    //     vertex_column.extend_one(8);
    //     vertex_column.extend_one(11);
    //     vertex_column.extend_one(13);

    //     let mut neighbor_column = FactorizedColumn::new();
    //     neighbor_column.extend(0..100);
    //     neighbor_column.extend(0..1);
    //     neighbor_column.extend(0..1);
    //     neighbor_column.extend(0..2);
    //     neighbor_column.extend(0..3);
    //     neighbor_column.extend(0..1);
    //     neighbor_column.extend(0..1);

    //     binner.update(Arc::new(vertex_column), Arc::new(neighbor_column));

    //     assert_eq!(binner.finish().values().unique().count(), 5);
    // }
}
