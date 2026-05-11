use std::cell::Cell;
use std::collections::hash_map::Entry;
use std::fmt::Display;
use std::fs::{create_dir_all, exists, remove_file, File};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use ahash::HashMap;
use duckdb::Connection;
use itertools::Itertools;
use log::trace;
use serde::{Deserialize, Serialize};

use super::Catalog;
use crate::common::{LabelId, LocalBucketMap, TagId};
use crate::error::{GCardError, GCardResult};
use crate::pattern::{GeneralPattern, GraphPattern, PathPattern};
use crate::statistics::{PathStatistics, StarStatistics};

const METADATA: &str = "metadata.bincode";
const DATA: &str = "data.db";
const DATA_WAL: &str = "data.db.wal";
const PATH_STATS: &str = "path_stats.bincode";
const STAR_STATS: &str = "star_stats.bincode";

#[derive(Debug)]
pub struct DuckCatalog {
    metadata: Metadata,
    conn: Connection,
    next_table_id: Cell<usize>,
    path_statistics: Vec<PathStatistics>,
    star_statistics: Vec<StarStatistics>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Metadata {
    paths: Vec<PathPattern>,
    stars: Vec<GeneralPattern>,
    path_label_map: HashMap<Vec<u8>, LabelId>,
    star_label_map: HashMap<(TagId, Vec<u8>), LabelId>,
    edge_count_map: HashMap<LabelId, usize>,
}

impl Display for DuckCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let metadata = &self.metadata;
        for (i, path) in metadata.paths.iter().enumerate() {
            writeln!(f, "Label {i}, Path: {path}")?;
        }
        Ok(())
    }
}

fn execute_sql(conn: &Connection, sql: &str) -> GCardResult<()> {
    trace!("{sql}");
    conn.execute_batch(sql)?;
    Ok(())
}

impl DuckCatalog {
    pub fn init() -> GCardResult<Self> {
        let conn = Connection::open_in_memory()?;
        // To avoid the error when exporting the database
        let sql = "set max_expression_depth = 9999999";
        execute_sql(&conn, sql)?;
        let next_table_id = Cell::new(0);
        let metadata = Default::default();
        let ret = Self {
            metadata,
            conn,
            next_table_id,
            path_statistics: Vec::new(),
            star_statistics: Vec::new(),
        };
        // Add empty star
        let table_name = format!("star_{}", LabelId::MAX / 2);
        ret.add_star_stats(&table_name, vec![], vec![])?;
        // Add empty path
        let table_name = format!("path_{}", LabelId::MAX / 2);
        ret.add_path_stats(&table_name, vec![], vec![], vec![])?;
        Ok(ret)
    }

    pub fn import<P: AsRef<Path>>(dir: P) -> GCardResult<Self> {
        let data_path = dir.as_ref().join(DATA);
        let metadata_path = dir.as_ref().join(METADATA);

        let file = File::open(metadata_path)?;
        let reader = BufReader::new(file);
        let metadata = bincode::deserialize_from(reader)?;

        let conn = Connection::open_in_memory()?;
        let sql = "set max_expression_depth = 9999999";
        execute_sql(&conn, sql)?;

        let sql = format!(
            "attach '{}' as input (READ_ONLY)",
            data_path.to_str().unwrap()
        );
        execute_sql(&conn, &sql)?;

        let sql = "copy from database input to memory";
        execute_sql(&conn, sql)?;

        let sql = "detach input";
        execute_sql(&conn, sql)?;

        Ok(Self {
            metadata,
            conn,
            next_table_id: Cell::new(0),
            path_statistics: Vec::new(),
            star_statistics: Vec::new(),
        })
    }

    pub fn export<P: AsRef<Path>>(&self, dir: P) -> GCardResult<()> {
        let db_path = dir.as_ref().join(DATA);
        let wal_path = dir.as_ref().join(DATA_WAL);
        let metadata_path = dir.as_ref().join(METADATA);
        let path_stats_path = dir.as_ref().join(PATH_STATS);
        let star_stats_path = dir.as_ref().join(STAR_STATS);
        create_dir_all(dir)?;

        if exists(&db_path)? {
            remove_file(&db_path)?;
        }
        if exists(&wal_path)? {
            remove_file(&wal_path)?;
        }

        let sql = format!("attach '{}' as output", db_path.to_str().unwrap());
        execute_sql(&self.conn, &sql)?;

        let sql = "copy from database memory to output";
        execute_sql(&self.conn, sql)?;

        let sql = "detach output";
        execute_sql(&self.conn, sql)?;

        let file = File::create(metadata_path)?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, &self.metadata)?;

        let file = File::create(path_stats_path)?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, &self.path_statistics)?;

        let file = File::create(star_stats_path)?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, &self.star_statistics)?;

        Ok(())
    }

    fn add_star_stats(
        &self,
        table_name: &str,
        count: Vec<u64>,
        max_degree: Vec<u64>,
    ) -> GCardResult<()> {
        let sql = format!("create table {table_name} (id uint16, _mode uint64, _count uint64)");
        execute_sql(&self.conn, &sql)?;
        let mut appender = self.conn.appender(table_name)?;
        appender.append_rows(
            max_degree
                .into_iter()
                .zip_eq(count)
                .enumerate()
                .map(|(i, (max_degree, count))| [i as u64, max_degree, count]),
        )?;
        Ok(())
    }

    pub fn add_star(&mut self, star: StarStatistics) -> GCardResult<LabelId> {
        let stats_cloned = star.clone();
        let StarStatistics {
            star,
            center_rank,
            count,
            max_degree,
        } = star;
        let empty_stats = count.iter().all(|count| *count == 0);
        let mut label_id = self.metadata.stars.len() as LabelId;
        if empty_stats {
            label_id += LabelId::MAX / 2 + 1;
        }
        match self
            .metadata
            .star_label_map
            .entry((center_rank, star.encode()))
        {
            Entry::Occupied(entry) => {
                return Err(GCardError::Catalog(format!(
                    "star already exists in the catalog, label_id: {}",
                    entry.get()
                )))
            }
            Entry::Vacant(entry) => {
                entry.insert(label_id);
            }
        }
        self.metadata.stars.push(star);
        if !empty_stats {
            let table_name = format!("star_{label_id}");
            self.add_star_stats(&table_name, count, max_degree)?;
            self.star_statistics.push(stats_cloned);
        }
        Ok(label_id)
    }

    fn add_path_stats(
        &self,
        table_name: &str,
        count: Vec<Box<[u64]>>,
        start_max_degree: Vec<Box<[u64]>>,
        end_max_degree: Vec<Box<[u64]>>,
    ) -> GCardResult<()> {
        let sql = format!("create table {table_name} (s uint16, t uint16, _mode_s uint64, _mode_t uint64, _count uint64)");
        execute_sql(&self.conn, &sql)?;

        let mut appender = self.conn.appender(table_name)?;
        for (i, ((max_degree_s, max_degree_t), count)) in start_max_degree
            .into_iter()
            .zip_eq(end_max_degree)
            .zip_eq(count)
            .enumerate()
        {
            for (j, ((max_degree_s, max_degree_t), count)) in max_degree_s
                .iter()
                .zip_eq(max_degree_t.iter())
                .zip_eq(count.iter())
                .enumerate()
                .filter(|(_, ((_, _), count))| **count != 0)
            {
                appender.append_row([i as u64, j as u64, *max_degree_s, *max_degree_t, *count])?;
            }
        }
        Ok(())
    }

    pub fn add_path(&mut self, stats: PathStatistics) -> GCardResult<LabelId> {
        let stats_cloned = stats.clone();
        let PathStatistics {
            path,
            count,
            start_max_degree,
            end_max_degree,
        } = stats;
        // Optimize empty statistics
        let empty_stats = count
            .iter()
            .all(|count| count.iter().all(|count| *count == 0));
        let mut label_id = self.metadata.paths.len() as LabelId;
        if empty_stats {
            label_id += LabelId::MAX / 2 + 1;
        }
        match self.metadata.path_label_map.entry(path.encode()) {
            Entry::Occupied(entry) => {
                return Err(GCardError::Catalog(format!(
                    "path {} already exists in the catalog, label_id: {}",
                    path,
                    entry.get()
                )))
            }
            Entry::Vacant(entry) => {
                entry.insert(label_id);
            }
        }
        self.metadata.paths.push(path);
        if !empty_stats {
            let table_name = format!("path_{label_id}");
            self.add_path_stats(&table_name, count, start_max_degree, end_max_degree)?;
            self.path_statistics.push(stats_cloned);
        }

        Ok(label_id)
    }

    pub fn add_edge_count(&mut self, edge_label_id: LabelId, count: usize) {
        assert!(self
            .metadata
            .edge_count_map
            .insert(edge_label_id, count)
            .is_none());
    }

    pub fn add_bucket_map(
        &self,
        label_id: LabelId,
        bucket_map: &LocalBucketMap,
    ) -> GCardResult<()> {
        let table_name = format!("bucket_{label_id}");
        let sql = format!("create table {table_name} (id uint64, bucket_id uint16)");
        execute_sql(&self.conn, &sql)?;
        let mut appender = self.conn.appender(&table_name)?;
        appender.append_rows(bucket_map.iter().map(|(id, bucket_id)| [id, bucket_id]))?;
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn next_table_id(&self) -> &Cell<usize> {
        &self.next_table_id
    }
}

impl Catalog for DuckCatalog {
    fn get_path_label_id(&self, code: &[u8]) -> Option<LabelId> {
        self.metadata.path_label_map.get(code).copied()
    }

    fn get_path(&self, label_id: LabelId) -> Option<&PathPattern> {
        self.metadata.paths.get(label_id as usize)
    }

    fn get_star_label_id(&self, rank: TagId, code: &[u8]) -> Option<LabelId> {
        self.metadata
            .star_label_map
            .get(&(rank, code.to_owned()))
            .copied()
    }

    fn get_star(&self, label_id: LabelId) -> Option<&GeneralPattern> {
        self.metadata.stars.get(label_id as usize)
    }

    fn get_edge_count(&self, label_id: LabelId) -> Option<usize> {
        self.metadata.edge_count_map.get(&label_id).copied()
    }
}
