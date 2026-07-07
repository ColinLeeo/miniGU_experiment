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

use std::collections::{HashMap, HashSet};
use std::path::Path;

use minigu_common::types::{LabelId, VertexId};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::block_statistic::BlockStatistic;
use crate::procedures::gcard_query::catalog::{
    AltKey, CompressedDegreeSeq, DegreeSeqGraphCompressed,
};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::utils::StarStatKey;

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
    #[serde(default)]
    pub(crate) star_statistic: HashMap<StarStatKey, BlockStatistic>,
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

fn star_key_serialized_size(star_key: &StarStatKey) -> usize {
    let mut total = string_bincode_size(&star_key.center_label);
    total += LEN_U64; // degree
    total += LEN_U64; // max_arm_len
    total += LEN_U64; // arms count
    for arm in &star_key.arms {
        total += LEN_U64;
        for v in &arm.vs {
            total += string_bincode_size(v);
        }
        total += LEN_U64;
        for e in &arm.es {
            total += string_bincode_size(e);
        }
    }
    total
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Statistic {
    pub label_path_statistic: HashMap<String, LabelStatistic>,
    /// (center_label, single-arm AltKey) -> star keys that mention this arm.
    /// Used by incremental star updates to find which star blocks need a
    /// product recompute after a path arm degree changes. Skipped during
    /// serialization; rebuilt via `rebuild_indexes()` and incrementally on
    /// `insert_or_update_star()`.
    #[serde(skip, default)]
    pub(crate) arm_to_star_keys: HashMap<(String, AltKey), Vec<StarStatKey>>,
}

impl Statistic {
    pub fn rebuild_indexes(&mut self) {
        for ls in self.label_path_statistic.values_mut() {
            ls.rebuild_rank_index();
        }
        self.rebuild_arm_to_star_keys();
    }

    pub(crate) fn rebuild_arm_to_star_keys(&mut self) {
        let mut idx: HashMap<(String, AltKey), Vec<StarStatKey>> = HashMap::new();
        for ls in self.label_path_statistic.values() {
            for star_key in ls.star_statistic.keys() {
                for arm in &star_key.arms {
                    let entry = idx
                        .entry((star_key.center_label.clone(), arm.to_alt_key()))
                        .or_default();
                    entry.push(star_key.clone());
                }
            }
        }
        self.arm_to_star_keys = idx;
    }

    pub fn insert_or_update(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        alt_key: AltKey,
        frequencies: &[u64],
    ) -> GCardResult<()> {
        self.insert_or_update_with(label, vertex_ids, frequencies, |entry, block| {
            entry.path_statistic.insert(alt_key, block);
        })
    }

    pub fn insert_or_update_star(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        star_key: StarStatKey,
        frequencies: &[u64],
    ) -> GCardResult<()> {
        let center = star_key.center_label.clone();
        let arm_keys: Vec<AltKey> = star_key.arms.iter().map(|arm| arm.to_alt_key()).collect();
        let star_key_for_index = star_key.clone();
        self.insert_or_update_with(label, vertex_ids, frequencies, |entry, block| {
            entry.star_statistic.insert(star_key, block);
        })?;
        // Keep the reverse index consistent without a full rebuild.
        for arm_key in arm_keys {
            self.arm_to_star_keys
                .entry((center.clone(), arm_key))
                .or_default()
                .push(star_key_for_index.clone());
        }
        Ok(())
    }

    pub(crate) fn insert_prebuilt_path_block(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        alt_key: AltKey,
        block: BlockStatistic,
    ) -> GCardResult<()> {
        self.insert_prebuilt_with(label, vertex_ids, |entry| {
            entry.path_statistic.insert(alt_key, block);
        })
    }

    pub(crate) fn insert_prebuilt_star_block(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        star_key: StarStatKey,
        block: BlockStatistic,
    ) -> GCardResult<()> {
        let center = star_key.center_label.clone();
        let arm_keys: Vec<AltKey> = star_key.arms.iter().map(|arm| arm.to_alt_key()).collect();
        let star_key_for_index = star_key.clone();
        self.insert_prebuilt_with(label, vertex_ids, |entry| {
            entry.star_statistic.insert(star_key, block);
        })?;
        for arm_key in arm_keys {
            self.arm_to_star_keys
                .entry((center.clone(), arm_key))
                .or_default()
                .push(star_key_for_index.clone());
        }
        Ok(())
    }

    fn insert_prebuilt_with<F>(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        insert: F,
    ) -> GCardResult<()>
    where
        F: FnOnce(&mut LabelStatistic),
    {
        if !vertex_ids.windows(2).all(|w| w[0] <= w[1]) {
            return Err(GCardError::InvalidData(
                "prebuilt statistic vertex_ids must be sorted".into(),
            ));
        }

        let entry = self
            .label_path_statistic
            .entry(label.to_string())
            .or_default();

        if !entry.vertex_ids.is_empty() {
            if entry.vertex_ids.len() != vertex_ids.len() {
                return Err(GCardError::InvalidData(
                    "vertex_ids length differs from existing".into(),
                ));
            }
            let (ex_min, ex_max) = (
                *entry.vertex_ids.first().unwrap_or(&0),
                *entry.vertex_ids.last().unwrap_or(&0),
            );
            let (new_min, new_max) = (
                *vertex_ids.first().unwrap_or(&0),
                *vertex_ids.last().unwrap_or(&0),
            );
            if ex_min != new_min || ex_max != new_max {
                return Err(GCardError::InvalidData(
                    "vertex_ids min/max differs from existing".into(),
                ));
            }
        } else {
            entry.vertex_ids = vertex_ids.to_vec();
            entry.rebuild_rank_index();
        }

        insert(entry);
        Ok(())
    }

    fn insert_or_update_with<F>(
        &mut self,
        label: &str,
        vertex_ids: &[VertexId],
        frequencies: &[u64],
        insert: F,
    ) -> GCardResult<()>
    where
        F: FnOnce(&mut LabelStatistic, BlockStatistic),
    {
        if vertex_ids.len() != frequencies.len() {
            return Err(GCardError::InvalidData(
                "vertex_ids and frequencies length mismatch".into(),
            ));
        }
        let input_sorted = vertex_ids.windows(2).all(|w| w[0] <= w[1]);

        let entry = self
            .label_path_statistic
            .entry(label.to_string())
            .or_default();

        if input_sorted {
            let block = BlockStatistic::from_u64_sequence(frequencies)?;
            if !entry.vertex_ids.is_empty() {
                if entry.vertex_ids.len() != vertex_ids.len() {
                    return Err(GCardError::InvalidData(
                        "vertex_ids length differs from existing".into(),
                    ));
                }
                let (ex_min, ex_max) = (
                    *entry.vertex_ids.first().unwrap_or(&0),
                    *entry.vertex_ids.last().unwrap_or(&0),
                );
                let (new_min, new_max) = (
                    *vertex_ids.first().unwrap_or(&0),
                    *vertex_ids.last().unwrap_or(&0),
                );
                if ex_min != new_min || ex_max != new_max {
                    return Err(GCardError::InvalidData(
                        "vertex_ids min/max differs from existing".into(),
                    ));
                }
            } else {
                entry.vertex_ids = vertex_ids.to_vec();
                entry.rebuild_rank_index();
            }

            insert(entry, block);
            return Ok(());
        }

        let mut pairs: Vec<(VertexId, u64)> = vertex_ids
            .iter()
            .zip(frequencies.iter())
            .map(|(&v, &f)| (v, f))
            .collect();
        pairs.sort_by_key(|p| p.0);
        let (sorted_vertex_ids, sorted_frequencies): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();

        let block = BlockStatistic::from_u64_sequence(&sorted_frequencies)?;

        if !entry.vertex_ids.is_empty() {
            if entry.vertex_ids.len() != sorted_vertex_ids.len() {
                return Err(GCardError::InvalidData(
                    "vertex_ids length differs from existing".into(),
                ));
            }
            let (ex_min, ex_max) = (
                *entry.vertex_ids.first().unwrap_or(&0),
                *entry.vertex_ids.last().unwrap_or(&0),
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

        insert(entry, block);
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
            for bs in ls.star_statistic.values() {
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
        let mut star_stats: HashMap<StarStatKey, CompressedDegreeSeq> = HashMap::new();
        for (node_name, ls) in &self.label_path_statistic {
            for (alt_key, block) in &ls.path_statistic {
                if let Some(seq) = block.get_compressed_degree_seq()? {
                    edge_set_to_endpoints
                        .entry(alt_key.clone())
                        .or_default()
                        .insert(node_name.clone(), seq);
                }
            }
            for (star_key, block) in &ls.star_statistic {
                if let Some(seq) = block.get_bucket_max_degree_seq()? {
                    star_stats.insert(star_key.clone(), seq);
                }
            }
        }
        Ok(DegreeSeqGraphCompressed {
            edge_set_to_endpoints,
            path_aliases: HashMap::new(),
            star_stats,
            edge_cardinalities: HashMap::new(),
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
        let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
        let new_val = (current as i64 + delta).max(0) as u64;
        bs.apply_value_updates(&[(rank, new_val)]);
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

        let mut updates: Vec<(usize, u64)> = Vec::with_capacity(deltas.len());
        for (&vertex_id, &delta) in deltas {
            if delta == 0 {
                continue;
            }
            let Ok(rank) = ls.vertex_ids.binary_search(&vertex_id) else {
                continue;
            };
            let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
            let new_val = (current as i64 + delta).max(0) as u64;
            updates.push((rank, new_val));
        }
        let applied = updates.len();
        bs.apply_value_updates(&updates);
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
                    let mut updates: Vec<(usize, u64)> = Vec::with_capacity(rank_deltas.len());
                    for (rank, delta) in rank_deltas {
                        let current = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
                        let new_val = (current as i64 + delta).max(0) as u64;
                        updates.push((rank, new_val));
                    }
                    applied += updates.len();
                    bs.apply_value_updates(&updates);
                }
                applied
            })
            .sum()
    }

    /// Recompute star block entries that depend on any path arm whose degree
    /// just changed.
    ///
    /// `path_dirty` maps `(arm_altkey, center_label) -> vertex set` and is
    /// the same shape produced by `compact_and_apply_flat` for path deltas.
    /// Must be called **after** path deltas have been applied via
    /// `apply_grouped_deltas`, because the product re-reads each arm's
    /// current degree from `path_statistic`.
    ///
    /// Returns the set of `StarStatKey`s whose block was touched so callers
    /// can incrementally refresh `DegreeSeqGraphCompressed::star_stats`.
    pub fn apply_star_updates_from_path_dirty(
        &mut self,
        path_dirty: &HashMap<(AltKey, String), HashMap<VertexId, i64>>,
    ) -> HashSet<StarStatKey> {
        // 1. Collect (star_key, center_label) -> set of dirty center vertices.
        let mut star_dirty: HashMap<(StarStatKey, String), HashSet<VertexId>> = HashMap::new();
        for ((arm_key, center_label), vertex_deltas) in path_dirty {
            let lookup = (center_label.clone(), arm_key.clone());
            let Some(star_keys) = self.arm_to_star_keys.get(&lookup) else {
                continue;
            };
            for star_key in star_keys {
                let slot = star_dirty
                    .entry((star_key.clone(), center_label.clone()))
                    .or_default();
                for vid in vertex_deltas.keys() {
                    slot.insert(*vid);
                }
            }
        }

        let mut dirty_star_keys: HashSet<StarStatKey> = HashSet::new();
        if star_dirty.is_empty() {
            return dirty_star_keys;
        }

        // 2. For each (star_key, center_label), recompute the product over arms for every dirty
        //    center vertex and write back into the star block.
        for ((star_key, center_label), dirty_vertices) in star_dirty {
            let Some(ls) = self.label_path_statistic.get_mut(&center_label) else {
                continue;
            };
            // Pre-resolve each arm's block via index into `path_statistic`.
            // Bail out for this star key if any required arm block is missing
            // (would yield a zero product otherwise, which would be wrong).
            let arm_keys: Vec<AltKey> = star_key.arms.iter().map(|arm| arm.to_alt_key()).collect();
            let arms_ok = arm_keys
                .iter()
                .all(|alt| ls.path_statistic.contains_key(alt));
            if !arms_ok {
                continue;
            }

            let mut updates: Vec<(usize, u64)> = Vec::with_capacity(dirty_vertices.len());
            for vid in dirty_vertices {
                let Some(rank) = ls.rank_of(vid) else {
                    continue;
                };
                let mut product: u64 = 1;
                for alt in &arm_keys {
                    let bs = ls
                        .path_statistic
                        .get(alt)
                        .expect("arm block presence checked above");
                    let d = bs.recover_upper_bound_at_rank(rank).unwrap_or(0);
                    product = product.saturating_mul(d);
                    if product == 0 {
                        // Once any factor is zero the star degree is zero;
                        // no point in multiplying further.
                        break;
                    }
                }
                updates.push((rank, product));
            }

            if updates.is_empty() {
                continue;
            }
            if let Some(star_bs) = ls.star_statistic.get_mut(&star_key) {
                star_bs.apply_value_updates(&updates);
                dirty_star_keys.insert(star_key);
            }
        }

        dirty_star_keys
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
            for bs in ls.star_statistic.values_mut() {
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
        // Estimate the actual bincode persistence format. This stays aligned with
        // `BlockStatistic`'s custom serde payload, including compressed star blocks.
        bincode::serialized_size(self).unwrap_or(0) as usize
    }

    pub fn report_compressed_sizes(&self) {
        super::compression::report_component_sizes(self);
    }
}

pub fn save_statistic(
    db_path: &Path,
    graph_name: &str,
    statistic: &Statistic,
) -> Result<usize, anyhow::Error> {
    // `Statistic` 是可持久化的基础资产：
    // 后续即使进程重启，也能从它重建查询所需的压缩视图。
    let path = crate::catalog_persistence::statistic_path(db_path, graph_name);
    let bytes = bincode::serialize(statistic)
        .map_err(|e| anyhow::anyhow!("failed to serialize statistic: {}", e))?;
    std::fs::write(&path, &bytes)?;
    let size = bytes.len();
    println!(
        "Statistic saved to {} ({:.2} MB)",
        path.display(),
        size as f64 / 1024.0 / 1024.0
    );
    Ok(size)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::procedures::gcard_query::utils::PathPattern;

    fn load_statistic_for_compare(path: PathBuf) -> Statistic {
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
        let mut statistic: Statistic = bincode::deserialize(&bytes)
            .unwrap_or_else(|e| panic!("failed to deserialize {}: {}", path.display(), e));
        statistic.rebuild_indexes();
        statistic
    }

    fn assert_same_vec<T: Eq + std::fmt::Debug>(path: &str, field: &str, left: &[T], right: &[T]) {
        if left == right {
            return;
        }
        let first_diff = left
            .iter()
            .zip(right.iter())
            .position(|(l, r)| l != r)
            .map(|idx| {
                format!(
                    "first_diff={} left={:?} right={:?}",
                    idx, left[idx], right[idx]
                )
            })
            .unwrap_or_else(|| "one vector is a prefix of the other".to_string());
        panic!(
            "{} {} differ: left_len={} right_len={} {}",
            path,
            field,
            left.len(),
            right.len(),
            first_diff
        );
    }

    fn assert_same_block_statistic(path: &str, left: &BlockStatistic, right: &BlockStatistic) {
        assert_same_vec(path, "bucket_ids", &left.bucket_ids, &right.bucket_ids);
        assert_same_vec(path, "prefix", &left.prefix, &right.prefix);
        assert_same_vec(path, "res_vec", &left.res_vec, &right.res_vec);
        assert_same_vec(
            path,
            "bucket_max_values",
            &left.bucket_max_values,
            &right.bucket_max_values,
        );
    }

    fn sorted_path_blocks(
        map: &HashMap<AltKey, BlockStatistic>,
    ) -> Vec<(Vec<u8>, &AltKey, &BlockStatistic)> {
        let mut out: Vec<_> = map
            .iter()
            .map(|(key, block)| {
                (
                    bincode::serialize(key).expect("serialize AltKey"),
                    key,
                    block,
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn sorted_star_blocks(
        map: &HashMap<StarStatKey, BlockStatistic>,
    ) -> Vec<(Vec<u8>, &StarStatKey, &BlockStatistic)> {
        let mut out: Vec<_> = map
            .iter()
            .map(|(key, block)| {
                (
                    bincode::serialize(key).expect("serialize StarStatKey"),
                    key,
                    block,
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn assert_same_statistic_degree_sequences(left: &Statistic, right: &Statistic) {
        let mut left_labels: Vec<_> = left.label_path_statistic.keys().collect();
        let mut right_labels: Vec<_> = right.label_path_statistic.keys().collect();
        left_labels.sort();
        right_labels.sort();
        assert_eq!(left_labels, right_labels, "label sets differ");

        let mut path_block_count = 0usize;
        let mut star_block_count = 0usize;
        let mut vertex_count = 0usize;
        for label in left_labels {
            let left_label = &left.label_path_statistic[label];
            let right_label = &right.label_path_statistic[label];
            assert_eq!(
                left_label.vertex_ids, right_label.vertex_ids,
                "vertex_ids differ for label {}",
                label
            );
            vertex_count += left_label.vertex_ids.len();

            let left_paths = sorted_path_blocks(&left_label.path_statistic);
            let right_paths = sorted_path_blocks(&right_label.path_statistic);
            assert_eq!(
                left_paths.len(),
                right_paths.len(),
                "path block count differs for label {}",
                label
            );
            for ((left_key_bytes, left_key, left_block), (right_key_bytes, _, right_block)) in
                left_paths.iter().zip(right_paths.iter())
            {
                assert_eq!(
                    left_key_bytes, right_key_bytes,
                    "path key set/order differs for label {}",
                    label
                );
                let path = format!("label={} path={:?}", label, left_key);
                assert_same_block_statistic(&path, left_block, right_block);
            }
            path_block_count += left_paths.len();

            let left_stars = sorted_star_blocks(&left_label.star_statistic);
            let right_stars = sorted_star_blocks(&right_label.star_statistic);
            assert_eq!(
                left_stars.len(),
                right_stars.len(),
                "star block count differs for label {}",
                label
            );
            for ((left_key_bytes, left_key, left_block), (right_key_bytes, _, right_block)) in
                left_stars.iter().zip(right_stars.iter())
            {
                assert_eq!(
                    left_key_bytes, right_key_bytes,
                    "star key set/order differs for label {}",
                    label
                );
                let path = format!("label={} star={:?}", label, left_key);
                assert_same_block_statistic(&path, left_block, right_block);
            }
            star_block_count += left_stars.len();
        }

        eprintln!(
            "statistic degree sequence compare OK: labels={} vertex_ids={} path_blocks={} star_blocks={}",
            left.label_path_statistic.len(),
            vertex_count,
            path_block_count,
            star_block_count
        );
    }

    #[test]
    #[ignore]
    fn compare_statistic_degree_sequences_from_env() {
        let left = std::env::var("GCARD_STAT_COMPARE_LEFT")
            .expect("GCARD_STAT_COMPARE_LEFT must point to a statistic.bin");
        let right = std::env::var("GCARD_STAT_COMPARE_RIGHT")
            .expect("GCARD_STAT_COMPARE_RIGHT must point to a statistic.bin");
        let left = load_statistic_for_compare(PathBuf::from(left));
        let right = load_statistic_for_compare(PathBuf::from(right));
        assert_same_statistic_degree_sequences(&left, &right);
    }

    #[test]
    fn star_statistic_is_stored_once_for_center_vertices() {
        let star_key = StarStatKey::new(
            "person".to_string(),
            vec![PathPattern::new_without_reverse(
                vec!["person".to_string(), "post".to_string()],
                vec!["created".to_string()],
            )],
        );
        let mut statistic = Statistic::default();

        statistic
            .insert_or_update_star("person", &[1, 2, 3], star_key.clone(), &[4, 5, 6])
            .unwrap();

        let person_stats = statistic.label_path_statistic.get("person").unwrap();
        assert_eq!(person_stats.star_statistic.len(), 1);
        assert_eq!(person_stats.vertex_ids, vec![1, 2, 3]);

        let compressed = statistic.to_degree_seq_graph_compressed().unwrap();
        assert_eq!(compressed.star_stats.len(), 1);
        assert!(compressed.star_stats.contains_key(&star_key));
        assert!(
            compressed.edge_set_to_endpoints.is_empty(),
            "star statistics should not be expanded into path-style endpoint maps"
        );
    }

    #[test]
    fn star_update_recomputes_product_from_current_path_degrees() {
        // Build a tiny statistic with center=person and two single-edge arms.
        // After bumping arm A's degree for one vertex, the star block at that
        // rank must equal `deg(arm_A) * deg(arm_B)` read from path_statistic.
        let arm_a = PathPattern::new(
            vec!["person".to_string(), "person".to_string()],
            vec!["knows".to_string()],
        );
        let arm_b = PathPattern::new(
            vec!["person".to_string(), "city".to_string()],
            vec!["livesin".to_string()],
        );
        let star_key = StarStatKey::new("person".to_string(), vec![arm_a.clone(), arm_b.clone()]);

        let mut statistic = Statistic::default();
        let vids = [10u64, 20, 30, 40];

        // Initial path degrees for each arm.
        let arm_a_seq: [u64; 4] = [2, 1, 0, 0];
        let arm_b_seq: [u64; 4] = [1, 1, 1, 0];
        statistic
            .insert_or_update("person", &vids, arm_a.to_alt_key(), &arm_a_seq)
            .unwrap();
        statistic
            .insert_or_update("person", &vids, arm_b.to_alt_key(), &arm_b_seq)
            .unwrap();

        // Initial star degree = elementwise product of arm_a_seq * arm_b_seq.
        let star_seq: [u64; 4] = [
            arm_a_seq[0] * arm_b_seq[0],
            arm_a_seq[1] * arm_b_seq[1],
            arm_a_seq[2] * arm_b_seq[2],
            arm_a_seq[3] * arm_b_seq[3],
        ];
        statistic
            .insert_or_update_star("person", &vids, star_key.clone(), &star_seq)
            .unwrap();

        // The reverse index must contain both arms for this star key.
        let arm_a_alt = arm_a.to_alt_key();
        let arm_b_alt = arm_b.to_alt_key();
        assert!(
            statistic
                .arm_to_star_keys
                .get(&("person".to_string(), arm_a_alt.clone()))
                .map(|v| v.contains(&star_key))
                .unwrap_or(false),
            "arm A should map to the star key"
        );
        assert!(
            statistic
                .arm_to_star_keys
                .get(&("person".to_string(), arm_b_alt.clone()))
                .map(|v| v.contains(&star_key))
                .unwrap_or(false),
            "arm B should map to the star key"
        );

        // Bump arm A for vertex 30 by +1, the way `compact_and_apply_flat`
        // would after observing a new edge that touches that vertex.
        let mut deltas: HashMap<VertexId, i64> = HashMap::new();
        deltas.insert(30, 1);
        let mut grouped: HashMap<String, Vec<(AltKey, HashMap<VertexId, i64>)>> = HashMap::new();
        grouped
            .entry("person".to_string())
            .or_default()
            .push((arm_a_alt.clone(), deltas.clone()));
        statistic.apply_grouped_deltas(grouped);

        // Now drive the star recompute via the same shape compact_and_apply_flat uses.
        let mut path_dirty: HashMap<(AltKey, String), HashMap<VertexId, i64>> = HashMap::new();
        path_dirty.insert((arm_a_alt.clone(), "person".to_string()), deltas);
        let dirty_star_keys = statistic.apply_star_updates_from_path_dirty(&path_dirty);
        assert!(
            dirty_star_keys.contains(&star_key),
            "star key must be marked dirty"
        );

        // Verify the star block at vertex 30's rank equals new product
        // (arm_a after +1 * arm_b unchanged) = 1 * 1 = 1.
        let ls = statistic.label_path_statistic.get("person").unwrap();
        let rank_30 = ls.rank_of(30).unwrap();
        let star_bs = ls.star_statistic.get(&star_key).unwrap();
        let arm_a_bs = ls.path_statistic.get(&arm_a_alt).unwrap();
        let arm_b_bs = ls.path_statistic.get(&arm_b_alt).unwrap();
        let d_a = arm_a_bs.recover_upper_bound_at_rank(rank_30).unwrap();
        let d_b = arm_b_bs.recover_upper_bound_at_rank(rank_30).unwrap();
        let star_val = star_bs.recover_upper_bound_at_rank(rank_30).unwrap();
        assert_eq!(d_a, 1, "arm A degree at v=30 should be 1 after +1");
        assert_eq!(d_b, 1, "arm B degree at v=30 should remain 1");
        assert_eq!(
            star_val,
            d_a * d_b,
            "star block must equal product of current arm degrees"
        );
    }

    #[test]
    fn rebuild_indexes_restores_arm_to_star_index() {
        // Simulate deserialize → rebuild_indexes by clearing the reverse
        // index and rebuilding from `label_path_statistic.*.star_statistic`.
        let arm = PathPattern::new(
            vec!["person".to_string(), "post".to_string()],
            vec!["created".to_string()],
        );
        let star_key = StarStatKey::new("person".to_string(), vec![arm.clone()]);
        let mut statistic = Statistic::default();
        statistic
            .insert_or_update("person", &[1, 2, 3], arm.to_alt_key(), &[1, 1, 1])
            .unwrap();
        statistic
            .insert_or_update_star("person", &[1, 2, 3], star_key.clone(), &[1, 1, 1])
            .unwrap();

        // Wipe the reverse index to mimic a freshly deserialized Statistic.
        statistic.arm_to_star_keys.clear();
        statistic.rebuild_indexes();

        let key = ("person".to_string(), arm.to_alt_key());
        let star_keys = statistic
            .arm_to_star_keys
            .get(&key)
            .expect("reverse index entry should exist after rebuild");
        assert!(star_keys.contains(&star_key));
    }
}
