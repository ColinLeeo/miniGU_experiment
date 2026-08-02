//! GCard catalog 的压缩存储层。
//!
//! 这里解决两个问题：
//! 1. 如何把“某条路径模式在某个端点标签上的度分布统计”组织成可查结构；
//! 2. 如何把这些统计压缩后持久化，并在查询时快速恢复为 PCF。

use std::collections::HashMap;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::degreepiecewise::PiecewiseConstantFunction;
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::utils::StarStatKey;
use crate::procedures::gcard_query::{functional_refine_enabled, max_bucket_enabled};

mod compact_persistence;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeCardinality {
    ManyToMany,
    ManyToOne,
    OneToMany,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltKey {
    /// 原始交错序列，例如：
    /// `[person, knows, person, likes, post]`
    /// 也就是 顶点标签 / 边标签 / 顶点标签 ... 交替出现。
    pub raw: Vec<String>,
    /// 规范化后的形式。
    /// 其目的是让一条路径和它的反向路径映射到同一个 key，
    /// 避免统计层重复存两份。
    normalized: Vec<String>,
}

impl fmt::Display for AltKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw.join(", "))
    }
}

impl AltKey {
    pub fn new(raw: Vec<String>) -> Self {
        // 规范化逻辑：
        // 1. 先把交错序列拆成顶点序列 vs 和边序列 es；
        // 2. 再和反向的 (rvs, res) 做字典序比较；
        // 3. 选择更小的一边作为 normalized。
        //
        // 这样 A-B-C 与 C-B-A 这种本质相同的路径模式就能共用一个统计项。
        let lowered: Vec<String> = raw.iter().map(|s| s.to_lowercase()).collect();
        let vs: Vec<&str> = lowered.iter().step_by(2).map(|s| s.as_str()).collect();
        let es: Vec<&str> = lowered
            .iter()
            .skip(1)
            .step_by(2)
            .map(|s| s.as_str())
            .collect();

        let rvs: Vec<&str> = vs.iter().rev().copied().collect();
        let res: Vec<&str> = es.iter().rev().copied().collect();

        let normalized = if (&vs, &es) <= (&rvs, &res) {
            lowered
        } else {
            let mut out = Vec::with_capacity(raw.len());
            for i in 0..rvs.len() {
                out.push(rvs[i].to_string());
                if i < res.len() {
                    out.push(res[i].to_string());
                }
            }
            out
        };
        Self { raw, normalized }
    }
}

impl PartialEq for AltKey {
    fn eq(&self, other: &Self) -> bool {
        self.normalized == other.normalized
    }
}

impl Hash for AltKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.normalized.hash(state);
    }
}

impl Eq for AltKey {}

pub fn make_alt_key(node_seq: &[String], edge_seq: &[String]) -> AltKey {
    // 把分离保存的 node_seq / edge_seq 重新交错，生成统一 key。
    let mut out = Vec::with_capacity(node_seq.len() + edge_seq.len());
    for i in 0..node_seq.len() {
        out.push(node_seq[i].clone());
        if i < edge_seq.len() {
            out.push(edge_seq[i].clone());
        }
    }
    AltKey::new(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompressedDegreeSeq {
    SafeBound {
        /// 更保守、信息更完整的表示：直接保存 PCF。
        function: PiecewiseConstantFunction,
    },
    FastCompressor {
        /// 桶的数量。
        len: usize,
        /// 指数桶底数，通常是 2。
        base: u64,
        /// 每个桶中有多少条目。
        counts: Vec<u64>,
    },
    BucketMax {
        /// 每个桶中有多少条目。
        counts: Vec<u64>,
        /// 每个桶使用从 BlockStatistic 恢复出的观测上界代表值。
        bucket_max_values: Vec<u64>,
    },
}

impl CompressedDegreeSeq {
    /// Convert to a [`PiecewiseConstantFunction`] directly.
    ///
    /// For `SafeBound` this is a clone.  For `FastCompressor` the histogram
    /// bins are mapped straight to PCF segments (CDF model) without expanding
    /// into a full degree sequence — O(num_bins) instead of O(num_vertices).
    pub fn to_pcf(&self) -> PiecewiseConstantFunction {
        // 查询阶段最终都希望消费统一的 PCF 形式，
        // 所以这里把不同压缩表示收敛到同一种接口。
        match self {
            CompressedDegreeSeq::SafeBound { function } => function.clone(),
            CompressedDegreeSeq::FastCompressor {
                len: _,
                base,
                counts,
            } => Self::fast_compressor_to_pcf(*base, counts),
            CompressedDegreeSeq::BucketMax {
                counts,
                bucket_max_values,
            } => {
                if max_bucket_enabled() {
                    Self::bucket_values_to_pcf(counts, bucket_max_values)
                } else {
                    let fallback_values = (0..counts.len())
                        .map(|i| 2u64.saturating_pow(i as u32))
                        .collect::<Vec<_>>();
                    Self::bucket_values_to_pcf(counts, &fallback_values)
                }
            }
        }
    }

    /// Build a PCF directly from the exponential-bin histogram.
    ///
    /// Each non-zero `counts[i]` becomes one PCF segment with:
    ///   - `constant  = base^i`  (the degree value represented by this bin)
    ///   - `width     = rows_in_bin / base^i`  (number of "x-axis slots" in CDF model)
    ///   - `cum_rows` accumulated left-to-right (high degree → low degree)
    ///
    /// The bins are emitted in **descending degree order** (highest `i` first)
    /// to match the convention used by `from_degree_sequence` with `model_cdf = true`.
    fn fast_compressor_to_pcf(base: u64, counts: &[u64]) -> PiecewiseConstantFunction {
        // 这里不把桶展开回完整 degree sequence，
        // 而是直接从“桶计数”构造分段常数函数，避免 O(num_vertices) 的恢复成本。
        let bucket_values = (0..counts.len())
            .map(|i| base.pow(i as u32))
            .collect::<Vec<_>>();
        Self::bucket_values_to_pcf(counts, &bucket_values)
    }

    fn bucket_values_to_pcf(counts: &[u64], bucket_values: &[u64]) -> PiecewiseConstantFunction {
        let mut buckets = counts
            .iter()
            .copied()
            .zip(bucket_values.iter().copied())
            .filter(|&(count, value)| count > 0 && value > 0)
            .collect::<Vec<_>>();

        buckets.sort_by(|a, b| b.1.cmp(&a.1));

        let total_rows: f64 = buckets
            .iter()
            .map(|&(count, value)| value as u128 * count as u128)
            .sum::<u128>() as f64;

        if total_rows == 0.0 {
            return PiecewiseConstantFunction::empty();
        }

        let mut constants = Vec::new();
        let mut right_interval_edges = Vec::new();
        let mut cumulative_rows = Vec::new();

        let mut cum_row: f64 = 0.0;
        let mut cum_x: f64 = 0.0;

        // 按度值从大到小输出段，和 CDF 模型保持一致。
        for (c, degree) in buckets {
            let degree_val = degree as f64;
            let rows_in_bin = degree_val * c as f64;

            // CDF 模型里，一段的横向宽度等于 “该桶总行数 / 桶代表的度值”，
            // 在这里恰好就是 c。
            // 末段要做截断，避免累计行数超过 total_rows。
            if cum_row + rows_in_bin >= total_rows {
                let remaining = total_rows - cum_row;
                let width = remaining / degree_val;
                cum_x += width.ceil().max(1.0);
                cum_row = total_rows;
                constants.push(degree_val);
                right_interval_edges.push(cum_x);
                cumulative_rows.push(cum_row);
                break;
            }

            let width = c as f64; // rows_in_bin / degree_val = c
            cum_x += width;
            cum_row += rows_in_bin;
            constants.push(degree_val);
            right_interval_edges.push(cum_x);
            cumulative_rows.push(cum_row);
        }

        if constants.is_empty() {
            return PiecewiseConstantFunction::empty();
        }

        PiecewiseConstantFunction {
            constants,
            right_interval_edges,
            cumulative_rows,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathAlias {
    pub source: AltKey,
    #[serde(default)]
    pub endpoint_map: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DegreeSeqGraphCompressed {
    /// 路径模式 -> 端点标签 -> 压缩后的度序列统计。
    ///
    /// 例如：
    /// `Person-knows-Person` 这条路径下，
    /// 既可能存“以左端 Person 为观察端点”的统计，也可能存右端 Person 的统计。
    pub edge_set_to_endpoints: HashMap<AltKey, HashMap<String, CompressedDegreeSeq>>,
    /// Schema-only functional extensions. These keys are not scanned; they
    /// redirect a longer path to an existing <=K source path.
    #[serde(default)]
    pub path_aliases: HashMap<AltKey, PathAlias>,
    #[serde(default)]
    pub star_stats: HashMap<StarStatKey, CompressedDegreeSeq>,
    #[serde(default)]
    pub edge_cardinalities: HashMap<String, EdgeCardinality>,
}

impl DegreeSeqGraphCompressed {
    pub fn new() -> Self {
        Self {
            edge_set_to_endpoints: HashMap::new(),
            path_aliases: HashMap::new(),
            star_stats: HashMap::new(),
            edge_cardinalities: HashMap::new(),
        }
    }

    pub fn get_piece_func_by_path(
        &self,
        path: &AltKey,
        target_node: &str,
    ) -> PiecewiseConstantFunction {
        // 这里会忽略 target_node 大小写，减少上层调用时的大小写耦合。
        let (path, target_node) = if functional_refine_enabled() {
            self.path_aliases
                .get(path)
                .map(|alias| {
                    let mapped_target = alias
                        .endpoint_map
                        .iter()
                        .find(|(alias_label, _)| alias_label.eq_ignore_ascii_case(target_node))
                        .map(|(_, source_label)| source_label.as_str())
                        .unwrap_or(target_node);
                    (&alias.source, mapped_target)
                })
                .unwrap_or((path, target_node))
        } else {
            (path, target_node)
        };

        if let Some(endpoints) = self.edge_set_to_endpoints.get(path) {
            endpoints
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(target_node))
                .map(|(_, v)| v.to_pcf())
                .unwrap_or_else(PiecewiseConstantFunction::empty)
        } else {
            PiecewiseConstantFunction::empty()
        }
    }

    /// Return an endpoint PCF only when the requested path was actually
    /// materialized by the catalog scan.
    ///
    /// A functional path alias may preserve the relation cardinality or the
    /// PCF at the endpoint opposite the extension, but it does not preserve
    /// both endpoint PCFs.  In particular, extending `Person -> Country` and
    /// then observing the path from `Country` must aggregate all matching
    /// persons per country.  Callers that need a complete two-ended abstract
    /// edge must therefore use materialized statistics rather than an alias.
    pub fn get_materialized_piece_func_by_path(
        &self,
        path: &AltKey,
        target_node: &str,
    ) -> PiecewiseConstantFunction {
        self.edge_set_to_endpoints
            .get(path)
            .and_then(|endpoints| {
                endpoints
                    .iter()
                    .find(|(label, _)| label.eq_ignore_ascii_case(target_node))
                    .map(|(_, degree_seq)| degree_seq.to_pcf())
            })
            .unwrap_or_else(PiecewiseConstantFunction::empty)
    }

    pub fn path_has_materialized_endpoint_pair(
        &self,
        path: &AltKey,
        src_label: &str,
        dst_label: &str,
    ) -> bool {
        Self::endpoint_pair_exists(self.edge_set_to_endpoints.get(path), src_label, dst_label)
    }

    pub fn path_has_endpoint_pair(&self, path: &AltKey, src_label: &str, dst_label: &str) -> bool {
        if self.path_has_materialized_endpoint_pair(path, src_label, dst_label) {
            return true;
        }

        if !functional_refine_enabled() {
            return false;
        }

        let Some(alias) = self.path_aliases.get(path) else {
            return false;
        };
        let src_label = self.map_alias_endpoint(alias, src_label);
        let dst_label = self.map_alias_endpoint(alias, dst_label);
        Self::endpoint_pair_exists(
            self.edge_set_to_endpoints.get(&alias.source),
            &src_label,
            &dst_label,
        )
    }

    pub fn alias_endpoint_sources(
        &self,
        path: &AltKey,
        src_label: &str,
        dst_label: &str,
    ) -> Option<(String, String)> {
        if !functional_refine_enabled() {
            return None;
        }
        let alias = self.path_aliases.get(path)?;
        let src_label = self.map_alias_endpoint(alias, src_label);
        let dst_label = self.map_alias_endpoint(alias, dst_label);
        if Self::endpoint_pair_exists(
            self.edge_set_to_endpoints.get(&alias.source),
            &src_label,
            &dst_label,
        ) {
            Some((src_label, dst_label))
        } else {
            None
        }
    }

    fn map_alias_endpoint(&self, alias: &PathAlias, label: &str) -> String {
        alias
            .endpoint_map
            .iter()
            .find(|(alias_label, _)| alias_label.eq_ignore_ascii_case(label))
            .map(|(_, source_label)| source_label.clone())
            .unwrap_or_else(|| label.to_string())
    }

    fn endpoint_pair_exists(
        endpoints: Option<&HashMap<String, CompressedDegreeSeq>>,
        src_label: &str,
        dst_label: &str,
    ) -> bool {
        endpoints.is_some_and(|endpoints| {
            endpoints
                .keys()
                .any(|label| label.eq_ignore_ascii_case(src_label))
                && endpoints
                    .keys()
                    .any(|label| label.eq_ignore_ascii_case(dst_label))
        })
    }

    pub fn get_piece_func_by_star(&self, star: &StarStatKey) -> PiecewiseConstantFunction {
        self.star_stats
            .get(star)
            .map(|v| v.to_pcf())
            .unwrap_or_else(PiecewiseConstantFunction::empty)
    }

    pub fn edge_cardinality(&self, edge_label: &str) -> Option<EdgeCardinality> {
        self.edge_cardinalities
            .iter()
            .find(|(label, _)| label.eq_ignore_ascii_case(edge_label))
            .map(|(_, card)| *card)
    }

    /// Incrementally rebuild only the dirty `(AltKey, label)` entries
    /// from the given `Statistic`, instead of full reconstruction.
    pub fn update_dirty(
        &mut self,
        statistic: &super::Statistic,
        dirty_keys: &std::collections::HashSet<(AltKey, String)>,
    ) {
        self.update_dirty_with_stars(
            statistic,
            dirty_keys,
            &std::collections::HashSet::<StarStatKey>::new(),
        );
    }

    /// Same as `update_dirty` but also refreshes the listed star entries
    /// in `self.star_stats` from `statistic.label_path_statistic[*].star_statistic`.
    pub fn update_dirty_with_stars(
        &mut self,
        statistic: &super::Statistic,
        dirty_keys: &std::collections::HashSet<(AltKey, String)>,
        dirty_star_keys: &std::collections::HashSet<StarStatKey>,
    ) {
        // 增量更新时，只刷新受影响的 key，避免每次 compact 后全量重建 catalog。
        #[derive(Debug)]
        enum DirtyAction {
            Upsert {
                altkey: AltKey,
                label: String,
                seq: CompressedDegreeSeq,
            },
            Remove {
                altkey: AltKey,
                label: String,
            },
        }

        #[derive(Debug)]
        enum StarAction {
            Upsert(StarStatKey, CompressedDegreeSeq),
            Remove(StarStatKey),
        }

        let t_total = std::time::Instant::now();
        let t_encode = std::time::Instant::now();
        let actions: Vec<DirtyAction> = dirty_keys
            .par_iter()
            .map(|(altkey, label)| {
                let new_seq = statistic
                    .label_path_statistic
                    .get(label)
                    .and_then(|ls| ls.path_statistic.get(altkey))
                    .and_then(|block| block.get_compressed_degree_seq().ok().flatten());

                match new_seq {
                    Some(seq) => DirtyAction::Upsert {
                        altkey: altkey.clone(),
                        label: label.clone(),
                        seq,
                    },
                    None => DirtyAction::Remove {
                        altkey: altkey.clone(),
                        label: label.clone(),
                    },
                }
            })
            .collect();

        let star_actions: Vec<StarAction> = dirty_star_keys
            .par_iter()
            .map(|star_key| {
                let new_seq = statistic
                    .label_path_statistic
                    .get(&star_key.center_label)
                    .and_then(|ls| ls.star_statistic.get(star_key))
                    .and_then(|block| block.get_bucket_max_degree_seq().ok().flatten());
                match new_seq {
                    Some(seq) => StarAction::Upsert(star_key.clone(), seq),
                    None => StarAction::Remove(star_key.clone()),
                }
            })
            .collect();
        eprintln!(
            "[dsgc] encode_dirty: {:.2}s ({} path keys, {} star keys)",
            t_encode.elapsed().as_secs_f64(),
            dirty_keys.len(),
            dirty_star_keys.len()
        );

        let t_apply = std::time::Instant::now();
        for action in actions {
            match action {
                DirtyAction::Upsert { altkey, label, seq } => {
                    self.edge_set_to_endpoints
                        .entry(altkey)
                        .or_default()
                        .insert(label, seq);
                }
                DirtyAction::Remove { altkey, label } => {
                    // 该路径/标签下已经没有有效统计时，删掉对应项。
                    if let Some(map) = self.edge_set_to_endpoints.get_mut(&altkey) {
                        map.remove(&label);
                        if map.is_empty() {
                            self.edge_set_to_endpoints.remove(&altkey);
                        }
                    }
                }
            }
        }
        for action in star_actions {
            match action {
                StarAction::Upsert(star_key, seq) => {
                    self.star_stats.insert(star_key, seq);
                }
                StarAction::Remove(star_key) => {
                    self.star_stats.remove(&star_key);
                }
            }
        }
        eprintln!(
            "[dsgc] apply_dirty: {:.2}s ({} path keys, {} star keys)",
            t_apply.elapsed().as_secs_f64(),
            dirty_keys.len(),
            dirty_star_keys.len()
        );
        eprintln!(
            "[dsgc] update_dirty total: {:.2}s ({} path keys, {} star keys)",
            t_total.elapsed().as_secs_f64(),
            dirty_keys.len(),
            dirty_star_keys.len()
        );
    }

    pub fn num_edge_sets(&self) -> usize {
        self.edge_set_to_endpoints.len()
    }

    pub fn export_bincode<P: AsRef<Path>>(&self, path: P) -> GCardResult<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, self)
            .map_err(|e| GCardError::InvalidData(format!("Failed to serialize: {}", e)))?;
        Ok(())
    }

    pub fn import_bincode<P: AsRef<Path>>(path: P) -> GCardResult<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let graph = bincode::deserialize_from(reader)
            .map_err(|e| GCardError::InvalidData(format!("Failed to deserialize: {}", e)))?;
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{AltKey, CompressedDegreeSeq, DegreeSeqGraphCompressed};

    #[test]
    fn update_dirty_removes_stale_endpoint_and_empty_altkey() {
        let altkey = AltKey::new(vec![
            "person".to_string(),
            "knows".to_string(),
            "person".to_string(),
        ]);
        let label = "person".to_string();

        let mut dsgc = DegreeSeqGraphCompressed::new();
        dsgc.edge_set_to_endpoints.insert(
            altkey.clone(),
            HashMap::from([(
                label.clone(),
                CompressedDegreeSeq::FastCompressor {
                    len: 1,
                    base: 2,
                    counts: vec![1],
                },
            )]),
        );

        let statistic = crate::procedures::gcard_query::Statistic::default();
        let dirty_keys = HashSet::from([(altkey.clone(), label.clone())]);

        dsgc.update_dirty(&statistic, &dirty_keys);

        assert!(!dsgc.edge_set_to_endpoints.contains_key(&altkey));
    }
}
