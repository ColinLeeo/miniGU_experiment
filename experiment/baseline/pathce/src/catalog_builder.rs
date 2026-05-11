use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use ahash::HashMap;
use log::{debug, info};
use murmur3::murmur3_32;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::ThreadPool;

use crate::binning::GreedyBinner;
use crate::catalog::DuckCatalog;
use crate::common::GlobalBucketMap;
use crate::error::GCardResult;
use crate::graph::LabeledGraph;
use crate::pattern::PathPattern;
use crate::sample::PathSampler;
use crate::schema::Schema;
use crate::statistics::StatisticsAnalyzer;

#[derive(Debug, Clone)]
pub struct CatalogBuilder {
    schema: Arc<Schema>,
    graph: Arc<LabeledGraph>,
    pool: Arc<ThreadPool>,
    max_path_length: usize,
    max_star_length: usize,
    max_star_degree: usize,
    buckets: usize,
    enable_greedy_bucket: bool,
    save_bucket_map: bool,
    skip_path: bool,
}

impl CatalogBuilder {
    pub fn new(schema: Arc<Schema>, graph: Arc<LabeledGraph>, pool: Arc<ThreadPool>) -> Self {
        Self {
            schema,
            graph,
            pool,
            max_path_length: 3,
            max_star_length: 3,
            max_star_degree: 4,
            buckets: 200,
            enable_greedy_bucket: true,
            save_bucket_map: false,
            skip_path: false,
        }
    }

    pub fn skip_path(mut self, skip: bool) -> Self {
        self.skip_path = skip;
        self
    }

    pub fn max_path_length(mut self, len: usize) -> Self {
        self.max_path_length = len;
        self
    }

    pub fn max_star_length(mut self, len: usize) -> Self {
        self.max_star_length = len;
        self
    }

    pub fn max_star_degree(mut self, degree: usize) -> Self {
        self.max_star_degree = degree;
        self
    }

    pub fn buckets(mut self, buckets: usize) -> Self {
        self.buckets = buckets;
        self
    }

    pub fn enable_greedy_bucket(mut self, enable: bool) -> Self {
        self.enable_greedy_bucket = enable;
        self
    }

    pub fn save_bucket_map(mut self, enable: bool) -> Self {
        self.save_bucket_map = enable;
        self
    }

    pub fn build(self) -> GCardResult<DuckCatalog> {
        let start = Instant::now();
        let edges = self.schema.generate_paths(1);
        info!("path generation: {} s", start.elapsed().as_secs_f64());

        let start = Instant::now();
        let global_bucket_map = if self.enable_greedy_bucket {
            self.greedy_binning(&edges)
        } else {
            self.hash_binning()
        };
        let global_bucket_map = Arc::new(global_bucket_map);
        info!("binning: {} s", start.elapsed().as_secs_f64());

        let analyzer = StatisticsAnalyzer::new(
            self.graph.clone(),
            self.schema.clone(),
            global_bucket_map.clone(),
            self.buckets,
            self.max_path_length,
            self.max_star_length,
            self.max_star_degree,
        );

        let path_stat_map = if !self.skip_path {
            let start = Instant::now();
            let path_stat_map: BTreeMap<_, _> = self
                .pool
                .scope(|_| analyzer.compute_path_statistics())
                .into_iter()
                .collect();
            info!(
                "path statistics: {} s, count: {}",
                start.elapsed().as_secs_f64(),
                path_stat_map.len()
            );
            path_stat_map
        } else {
            Default::default()
        };

        let start = Instant::now();
        let star_stat_map: BTreeMap<_, _> = self
            .pool
            .scope(|_| analyzer.compute_star_statistics())
            .into_iter()
            .collect();
        info!(
            "star statistics: {} s, count: {}",
            start.elapsed().as_secs_f64(),
            star_stat_map.len()
        );

        let start = Instant::now();
        let mut catalog = DuckCatalog::init()?;
        for stats in path_stat_map.into_values() {
            catalog.add_path(stats)?;
        }
        for stats in star_stat_map.into_values() {
            catalog.add_star(stats)?;
        }

        // Update edge counts
        for e in self.schema.edges() {
            let count = self.graph.get_num_edges(e.label).unwrap();
            catalog.add_edge_count(e.label, count);
        }

        if self.save_bucket_map {
            for (label_id, bucket_map) in global_bucket_map.as_ref() {
                catalog.add_bucket_map(*label_id, bucket_map)?;
            }
        }
        info!("build catalog: {} s", start.elapsed().as_secs_f64());
        Ok(catalog)
    }

    fn hash_binning(&self) -> GlobalBucketMap {
        self.schema
            .vertices()
            .par_iter()
            .map(|v| {
                let local_bucket_map: HashMap<_, _> = self
                    .graph
                    .vertices(v.label)
                    .unwrap()
                    .par_iter()
                    .map(|v| {
                        let bucket = murmur3_32(&mut v.to_le_bytes().as_slice(), 0).unwrap();
                        (*v, bucket as usize % self.buckets)
                    })
                    .collect();
                (v.label, local_bucket_map)
            })
            .collect()
    }

    fn greedy_binning(&self, base_paths: &[PathPattern]) -> GlobalBucketMap {
        let mut binners: HashMap<_, _> = self
            .schema
            .vertices()
            .iter()
            .map(|v| {
                let vertices = self.graph.vertices(v.label).unwrap();
                let binner = GreedyBinner::new(self.buckets, vertices.iter().copied());
                (v.label, binner)
            })
            .collect();

        let num_paths = base_paths.len();
        let sampler = PathSampler::new(self.graph.clone());

        self.pool.scope(|_| {
            base_paths.iter().enumerate().for_each(|(i, path)| {
                let path_start = path.start();
                let path_end = path.end();

                let start_should_finish =
                    binners.get(&path_start.label_id()).unwrap().should_finish();
                let end_should_finish = binners.get(&path_end.label_id()).unwrap().should_finish();

                if start_should_finish && end_should_finish {
                    debug!("[{:0>4}/{:0>4}] path: {}, skipped", i + 1, num_paths, path,);
                    return;
                }

                let start = Instant::now();
                let table = sampler.sample(path);
                debug!(
                    "[{:0>4}/{:0>4}] path: {}, sample time: {} s",
                    i + 1,
                    num_paths,
                    path,
                    start.elapsed().as_secs_f64()
                );

                let start_col = table.get_column(path_start.tag_id()).unwrap();
                let end_col = table.get_column(path_end.tag_id()).unwrap();
                binners
                    .get_mut(&path_start.label_id())
                    .unwrap()
                    .update(&start_col, &end_col);
                binners
                    .get_mut(&path_end.label_id())
                    .unwrap()
                    .update(&end_col, &start_col);
            });
        });

        binners
            .into_iter()
            .map(|(label_id, binner)| (label_id, binner.finish()))
            .collect()
    }
}
