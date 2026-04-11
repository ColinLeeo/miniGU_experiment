//! Definitions for Statistic.
//!
//! Statistic is a three-level map: LabelId -> LabelStatistic; each LabelStatistic 含 vertex_ids 与
//! path_statistic (AltKey -> BlockStatistic). vertex_ids 的长度与所有 path_statistic 里
//! BlockStatistic 的 bucket_ids 总长一致。

use std::collections::HashMap;
use std::path::Path;

use minigu_common::types::{LabelId, VertexId};
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::block_statistic::BlockStatistic;
use crate::procedures::gcard_query::catalog::{
    AltKey, CompressedDegreeSeq, DegreeSeqGraphCompressed,
};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};

type VertexVec = Vec<VertexId>;

const LEN_U64: usize = 8;
const LEN_LABEL: usize = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct LabelStatistic {
    pub(crate) vertex_ids: VertexVec,
    pub(crate) path_statistic: HashMap<AltKey, BlockStatistic>,
}

fn alt_key_serialized_size(alt_key: &AltKey) -> usize {
    let mut n = LEN_U64;
    for s in &alt_key.raw {
        n += LEN_U64 + s.as_bytes().len();
    }
    n
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Statistic {
    pub label_path_statistic: HashMap<String, LabelStatistic>,
}

impl Statistic {
    pub fn insert_or_update(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        alt_key: AltKey,
        frequencies: &[u64],
    ) -> GCardResult<()> {
        if vertex_ids.len() != frequencies.len() {
            return Err(GCardError::InvalidData(
                "vertex_ids and frequencies length mismatch".into(),
            ));
        }
        let mut pairs: Vec<(VertexId, u64)> = vertex_ids
            .iter()
            .zip(frequencies.iter())
            .map(|(&v, &f)| (v, f))
            .collect();
        pairs.sort_by_key(|p| p.0);
        let (sorted_vertex_ids, sorted_frequencies): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();

        let block = BlockStatistic::from_u64_sequence(&sorted_frequencies)?;
        let entry = self
            .label_path_statistic
            .entry(label.to_string())
            .or_default();

        if !entry.vertex_ids.is_empty() {
            if entry.vertex_ids.len() != sorted_vertex_ids.len() {
                return Err(GCardError::InvalidData(
                    "vertex_ids length differs from existing".into(),
                ));
            }
            let (ex_min, ex_max) = (
                *entry.vertex_ids.iter().min().unwrap(),
                *entry.vertex_ids.iter().max().unwrap(),
            );
            let (new_min, new_max) = (
                *sorted_vertex_ids.first().unwrap_or(&0),
                *sorted_vertex_ids.last().unwrap_or(&0),
            );
            if ex_min != new_min || ex_max != new_max {
                return Err(GCardError::InvalidData(
                    "vertex_ids min/max differs from existing".into(),
                ));
            }
        } else {
            entry.vertex_ids = sorted_vertex_ids;
        }

        entry.path_statistic.insert(alt_key, block);
        Ok(())
    }

    pub fn upper_limit_ratio(&self) -> f64 {
        let mut total = 0usize;
        let mut at_limit_weighted = 0.0f64;
        for ls in self.label_path_statistic.values() {
            for bs in ls.path_statistic.values() {
                let n = bs.bucket_ids.len();
                if n > 0 {
                    at_limit_weighted += bs.upper_limit_ratio() * n as f64;
                    total += n;
                }
            }
        }
        if total == 0 {
            0.0
        } else {
            at_limit_weighted / total as f64
        }
    }

    pub fn upper_limit_ratio_for_path(&self, alt_key: &AltKey) -> Option<f64> {
        let mut total = 0usize;
        let mut at_limit_weighted = 0.0f64;
        for ls in self.label_path_statistic.values() {
            if let Some(bs) = ls.path_statistic.get(alt_key) {
                let n = bs.bucket_ids.len();
                if n > 0 {
                    at_limit_weighted += bs.upper_limit_ratio() * n as f64;
                    total += n;
                }
            }
        }
        if total == 0 {
            None
        } else {
            Some(at_limit_weighted / total as f64)
        }
    }

    pub fn get_bucket_ids(&self, label: &str, alt_key: &AltKey) -> Option<&[u8]> {
        self.label_path_statistic
            .get(label)
            .and_then(|ls| ls.path_statistic.get(alt_key))
            .map(|b| b.bucket_ids())
    }

    pub fn get_vertex_bucket_prefix(
        &self,
        label: &str,
        alt_key: &AltKey,
        vertex_id: VertexId,
    ) -> Option<(u8, u8)> {
        let ls = self.label_path_statistic.get(label)?;
        let block = ls.path_statistic.get(alt_key)?;
        let i = ls.vertex_ids.binary_search(&vertex_id).ok()?;
        let bucket_id = *block.bucket_ids.get(i)?;
        let prefix = *block.prefix.get(i)?;
        Some((bucket_id, prefix))
    }

    pub fn to_degree_seq_graph_compressed(&self) -> GCardResult<DegreeSeqGraphCompressed> {
        let mut edge_set_to_endpoints: HashMap<AltKey, HashMap<String, CompressedDegreeSeq>> =
            HashMap::new();
        for (node_name, ls) in &self.label_path_statistic {
            for (alt_key, block) in &ls.path_statistic {
                if let Some(seq) = block.get_compressed_degree_seq()? {
                    edge_set_to_endpoints
                        .entry(alt_key.clone())
                        .or_default()
                        .insert(node_name.clone(), seq);
                }
            }
        }
        Ok(DegreeSeqGraphCompressed {
            edge_set_to_endpoints,
        })
    }

    pub fn apply_delta(&mut self, label: &str, altkey: &AltKey, vertex_id: VertexId, delta: i64) {
        let Some(ls) = self.label_path_statistic.get_mut(label) else {
            return;
        };
        let Ok(rank) = ls.vertex_ids.binary_search(&vertex_id) else {
            return;
        };
        let Some(bs) = ls.path_statistic.get_mut(altkey) else {
            return;
        };
        let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
        let new_val = (current as i64 + delta).max(0) as u64;
        bs.update_at_rank(rank, new_val);
    }

    /// Remove `vertex_id` from every label's `vertex_ids` list and the corresponding rank
    /// from every `BlockStatistic`.
    ///
    /// No-op for labels that do not contain `vertex_id`.
    pub fn delete_vertex(&mut self, vertex_id: VertexId) {
        for ls in self.label_path_statistic.values_mut() {
            let Ok(rank) = ls.vertex_ids.binary_search(&vertex_id) else {
                continue;
            };
            ls.vertex_ids.remove(rank);
            for bs in ls.path_statistic.values_mut() {
                bs.remove_at_rank(rank);
            }
        }
    }

    pub fn serialized_size(&self) -> usize {
        let mut total = LEN_U64; // label count
        for (_label_id, ls) in &self.label_path_statistic {
            total += LEN_LABEL; // label_id
            total += LEN_U64 + ls.vertex_ids.len() * LEN_U64; // vertex_ids len + data
            total += LEN_U64; // path_statistic count
            for (alt_key, block) in &ls.path_statistic {
                total += alt_key_serialized_size(alt_key);
                total += block.serialize().len();
            }
        }
        total
    }

    pub fn report_compressed_sizes(&self) {
        super::compression::report_component_sizes(self);
    }
}

pub fn save_statistic(
    db_path: &Path,
    graph_name: &str,
    statistic: &Statistic,
) -> Result<(), anyhow::Error> {
    let path = crate::catalog_persistence::statistic_path(db_path, graph_name);
    let bytes = bincode::serialize(statistic)
        .map_err(|e| anyhow::anyhow!("failed to serialize statistic: {}", e))?;
    std::fs::write(&path, &bytes)?;
    println!(
        "Statistic saved to {} ({:.2} MB)",
        path.display(),
        bytes.len() as f64 / 1024.0 / 1024.0
    );
    Ok(())
}

pub fn load_statistic(path: &Path) -> Result<Option<Statistic>, anyhow::Error> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let statistic: Statistic = bincode::deserialize(&bytes)
        .map_err(|e| anyhow::anyhow!("failed to deserialize statistic: {}", e))?;
    println!(
        "Statistic loaded from {} ({:.2} MB)",
        path.display(),
        bytes.len() as f64 / 1024.0 / 1024.0
    );
    Ok(Some(statistic))
}
