//! GCard 统计层的主数据结构。
//!
//! 可以把 `Statistic` 理解成“原始统计真身”，而 `DegreeSeqGraphCompressed`
//! 更像是从它派生出来、偏查询友好的压缩视图。
//!
//! 结构上是三层：
//! 1. 端点标签 `label`
//! 2. 路径模式 `AltKey`
//! 3. 对应路径下，每个顶点 rank 的压缩度统计 `BlockStatistic`
//!
//! 同一个 `label` 下，`vertex_ids` 定义了 rank -> 顶点 id 的映射；
//! 该 label 下所有 `path_statistic` 都必须和它保持同样长度。

use std::collections::HashMap;
use std::path::Path;

use minigu_common::types::{LabelId, VertexId};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::block_statistic::BlockStatistic;
use crate::procedures::gcard_query::catalog::{
    AltKey, CompressedDegreeSeq, DegreeSeqGraphCompressed,
};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};

type VertexVec = Vec<VertexId>;

const LEN_U64: usize = 8;

fn string_bincode_size(s: &str) -> usize {
    LEN_U64 + s.len()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct LabelStatistic {
    pub(crate) vertex_ids: VertexVec,
    #[serde(skip, default)]
    pub(crate) rank_index: HashMap<VertexId, usize>,
    pub(crate) path_statistic: HashMap<AltKey, BlockStatistic>,
}

impl LabelStatistic {
    fn rebuild_rank_index(&mut self) {
        self.rank_index = self
            .vertex_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(rank, vid)| (vid, rank))
            .collect();
    }

    #[inline]
    fn rank_of(&self, vertex_id: VertexId) -> Option<usize> {
        self.rank_index.get(&vertex_id).copied()
    }
}

fn alt_key_serialized_size(alt_key: &AltKey) -> usize {
    fn string_vec_bincode_size(values: &[String]) -> usize {
        let mut n = LEN_U64;
        for s in values {
            n += string_bincode_size(s);
        }
        n
    }

    // AltKey derives Serialize/Deserialize, so both `raw` and `normalized`
    // are persisted by bincode. `normalized` is built from the same label/edge
    // strings, so its serialized size matches `raw`.
    string_vec_bincode_size(&alt_key.raw) * 2
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Statistic {
    pub label_path_statistic: HashMap<String, LabelStatistic>,
}

impl Statistic {
    pub fn rebuild_indexes(&mut self) {
        for ls in self.label_path_statistic.values_mut() {
            ls.rebuild_rank_index();
        }
    }

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
            entry.rebuild_rank_index();
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
        // 通过 vertex_id 先查出 rank，再反查该 rank 在压缩块中的 bucket/prefix。
        let ls = self.label_path_statistic.get(label)?;
        let block = ls.path_statistic.get(alt_key)?;
        let i = ls.rank_of(vertex_id)?;
        let bucket_id = *block.bucket_ids.get(i)?;
        let prefix = *block.prefix.get(i)?;
        Some((bucket_id, prefix))
    }

    pub fn to_degree_seq_graph_compressed(&self) -> GCardResult<DegreeSeqGraphCompressed> {
        // 从“可更新真身”转换到“查询友好视图”。
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
        // 增量更新时只改一个顶点、一个路径模式下的统计。
        let Some(ls) = self.label_path_statistic.get_mut(label) else {
            return;
        };
        let Some(rank) = ls.rank_of(vertex_id) else {
            return;
        };
        let Some(bs) = ls.path_statistic.get_mut(altkey) else {
            return;
        };
        // 这里恢复出来的是当前保存的“上界值”，不是原始精确值。
        // 更新后仍然会被重新压回 BlockStatistic 的近似表示。
        let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
        let new_val = (current as i64 + delta).max(0) as u64;
        bs.update_at_rank(rank, new_val);
    }

    pub fn apply_deltas(
        &mut self,
        label: &str,
        altkey: &AltKey,
        deltas: &HashMap<VertexId, i64>,
    ) -> usize {
        // 批量版本：同一个 `(label, altkey)` 下复用一次 HashMap 定位，
        // 避免 1 个顶点调用一次 `apply_delta()` 的重复开销。
        let Some(ls) = self.label_path_statistic.get_mut(label) else {
            return 0;
        };
        let Some(bs) = ls.path_statistic.get_mut(altkey) else {
            return 0;
        };

        let mut applied = 0usize;
        for (&vertex_id, &delta) in deltas {
            if delta == 0 {
                continue;
            }
            let Ok(rank) = ls.vertex_ids.binary_search(&vertex_id) else {
                continue;
            };
            let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
            let new_val = (current as i64 + delta).max(0) as u64;
            bs.update_at_rank(rank, new_val);
            applied += 1;
        }
        applied
    }

    pub fn apply_grouped_deltas(
        &mut self,
        grouped: HashMap<String, Vec<(AltKey, HashMap<VertexId, i64>)>>,
    ) -> usize {
        self.label_path_statistic
            .par_iter_mut()
            .map(|(label, ls)| {
                let Some(entries) = grouped.get(label) else {
                    return 0usize;
                };

                let mut applied = 0usize;
                for (altkey, deltas) in entries {
                    let mut rank_deltas: Vec<(usize, i64)> = Vec::with_capacity(deltas.len());
                    for (&vertex_id, &delta) in deltas {
                        if delta == 0 {
                            continue;
                        }
                        let Some(rank) = ls.rank_of(vertex_id) else {
                            continue;
                        };
                        rank_deltas.push((rank, delta));
                    }

                    let Some(bs) = ls.path_statistic.get_mut(altkey) else {
                        continue;
                    };
                    for (rank, delta) in rank_deltas {
                        let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
                        let new_val = (current as i64 + delta).max(0) as u64;
                        bs.update_at_rank(rank, new_val);
                        applied += 1;
                    }
                }
                applied
            })
            .sum()
    }

    /// Remove `vertex_id` from every label's `vertex_ids` list and the corresponding rank
    /// from every `BlockStatistic`.
    ///
    /// No-op for labels that do not contain `vertex_id`.
    pub fn delete_vertex(&mut self, vertex_id: VertexId) {
        // 顶点被删除后，必须把所有 label 里对应 rank 也一起抹掉；
        // 否则 rank 布局会失配。
        for ls in self.label_path_statistic.values_mut() {
            let Some(rank) = ls.rank_of(vertex_id) else {
                continue;
            };
            ls.vertex_ids.remove(rank);
            for bs in ls.path_statistic.values_mut() {
                bs.remove_at_rank(rank);
            }
            ls.rebuild_rank_index();
        }
    }

    /// Return all `(AltKey, label)` pairs that contain `vertex_id`.
    pub fn keys_for_vertex(&self, vertex_id: VertexId) -> Vec<(AltKey, String)> {
        // 用于增量更新时快速确定“这个点影响了哪些统计项”。
        let mut keys = Vec::new();
        for (label, ls) in &self.label_path_statistic {
            if ls.rank_index.contains_key(&vertex_id) {
                for alt_key in ls.path_statistic.keys() {
                    keys.push((alt_key.clone(), label.clone()));
                }
            }
        }
        keys
    }

    pub fn serialized_size(&self) -> usize {
        // 估算当前 bincode 持久化格式的文件大小。
        let mut total = LEN_U64; // label count
        for (label, ls) in &self.label_path_statistic {
            total += string_bincode_size(label);
            total += LEN_U64 + ls.vertex_ids.len() * LEN_U64; // vertex_ids len + data
            total += LEN_U64; // path_statistic count
            for (alt_key, block) in &ls.path_statistic {
                total += alt_key_serialized_size(alt_key);
                // BlockStatistic uses `serialize_bytes`, so bincode stores:
                // [byte_len: u64][entry_count: u64][payload bytes...]
                total += LEN_U64 + LEN_U64 + block.serialize().len();
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
    // `Statistic` 是可持久化的基础资产：
    // 后续即使进程重启，也能从它重建查询所需的压缩视图。
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
    let mut statistic: Statistic = bincode::deserialize(&bytes)
        .map_err(|e| anyhow::anyhow!("failed to deserialize statistic: {}", e))?;
    statistic.rebuild_indexes();
    println!(
        "Statistic loaded from {} ({:.2} MB)",
        path.display(),
        bytes.len() as f64 / 1024.0 / 1024.0
    );
    Ok(Some(statistic))
}
