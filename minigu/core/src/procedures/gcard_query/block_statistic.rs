//! `BlockStatistic` 是对一列 u64 频数的轻量压缩表示。
//!
//! 核心思想是把每个值拆成三部分：
//! 1. `bucket_id`：值落在哪个指数桶里；
//! 2. `prefix`：高位的前缀；
//! 3. `res_vec[bucket]`：该桶里所有值共享的“最大余量上界”。
//!
//! 因此它保存的不是逐点精确值，而是“可恢复上界”。
//! 这也是为什么它很适合增量更新阶段：便宜、稳定，但会逐渐变松。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::procedures::gcard_query::catalog::CompressedDegreeSeq;
use crate::procedures::gcard_query::compression::{
    decode_raw_u64s, decode_rle, decode_zstd, encode_raw_u64s, encode_zstd,
};
use crate::procedures::gcard_query::degreepiecewise::fast_compressor::{
    build_bounds, get_bucket_index,
};
use crate::procedures::gcard_query::error::{GCardError, GCardResult};

const BUCKET_BASE: u64 = 2;
const COMPRESSED_MAGIC: &[u8; 4] = b"BSZ1";
const COMPRESSED_ZSTD_MAGIC: &[u8; 4] = b"BSZ2";

#[inline]
fn bucket_upper_bound(bucket_id: u8) -> u64 {
    BUCKET_BASE.saturating_pow(bucket_id as u32)
}

/// 前 8 bit 有效值 + 余量；bucket 作为 shift，余量 = value & ((1<<shift)-1)，shift 上限 56。
#[inline]
fn prefix_and_remainder(value: u64, bucket_id: u8) -> (u8, u64) {
    if value == 0 {
        return (0, 0);
    }
    let shift = (bucket_id as usize).min(56);
    let prefix = (value >> shift) as u8;
    let remainder = value & ((1u64 << shift).saturating_sub(1));
    (prefix, remainder)
}

#[derive(Debug, Clone, Default)]
pub struct BlockStatistic {
    /// 每个 rank 落入哪个桶。
    pub bucket_ids: Vec<u8>,
    /// 每个 rank 的高位前缀。
    pub prefix: Vec<u8>,
    /// 每个桶共享的余量上界。
    /// 同桶的任意条目恢复时都用这里的最大值，所以恢复出来通常是上界。
    pub res_vec: Vec<u64>,
}

impl BlockStatistic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_u64_sequence(values: &[u64]) -> GCardResult<Self> {
        // 从完整频数列压缩成 block 表示。
        if values.is_empty() {
            return Ok(BlockStatistic::default());
        }
        let max_val = values.iter().copied().max().unwrap_or(0);
        let bounds = build_bounds(BUCKET_BASE, max_val);
        let mut bucket_ids: Vec<u8> = Vec::with_capacity(values.len());
        let mut prefix: Vec<u8> = Vec::with_capacity(values.len());
        // res_max[b] = bucket b 中见过的最大 remainder
        let mut res_max: Vec<u64> = Vec::new();
        for &v in values {
            let b = get_bucket_index(&bounds, v);
            let b_u8 = b.min(u8::MAX as usize) as u8;
            let (p, r) = prefix_and_remainder(v, b_u8);
            bucket_ids.push(b_u8);
            prefix.push(p);
            let idx = b.min(u8::MAX as usize);
            if res_max.len() <= idx {
                res_max.resize(idx + 1, 0);
            }
            res_max[idx] = res_max[idx].max(r);
        }
        let max_b = bucket_ids.iter().copied().max().unwrap_or(0) as usize;
        res_max.truncate(max_b + 1);
        Ok(BlockStatistic {
            bucket_ids,
            prefix,
            res_vec: res_max,
        })
    }

    pub fn get_by_rank(&self, rank: usize) -> Option<(u8, u8, u64)> {
        let b = *self.bucket_ids.get(rank)?;
        let p = *self.prefix.get(rank)?;
        let r = self.res_vec.get(b as usize).copied().unwrap_or(0);
        Some((b, p, r))
    }

    pub fn bucket_ids(&self) -> &[u8] {
        &self.bucket_ids
    }

    pub fn recover_upper_bound_at_rank(&self, rank: usize) -> Option<u64> {
        // 用 bucket + prefix + 该桶共享的最大余量，恢复该位置的“上界值”。
        // 公共 remainder 可能把某些 rank 推过本桶理论上界；更新逻辑会把这里
        // 的结果当作当前值继续加减，所以必须截断在 bucket 上界内。
        let (b, p, r) = self.get_by_rank(rank)?;
        let shift = (b as usize).min(56);
        Some((((p as u64) << shift) | r).min(bucket_upper_bound(b)))
    }

    pub fn update_at_rank(&mut self, rank: usize, new_value: u64) {
        // 增量更新时，不会回头重算整个桶，只就地更新这个 rank 的 bucket/prefix，
        // 并扩大对应桶的 remainder 上界。
        if rank >= self.bucket_ids.len() {
            return;
        }
        // Fast bucket index for BUCKET_BASE=2: use bit operations instead of
        // build_bounds (Vec alloc) + get_bucket_index (binary search).
        let new_b = if new_value <= 1 {
            0usize
        } else {
            (u64::BITS - (new_value - 1).leading_zeros()) as usize
        }
        .min(u8::MAX as usize);
        let new_b_u8 = new_b as u8;
        let (new_p, new_r) = prefix_and_remainder(new_value, new_b_u8);

        self.bucket_ids[rank] = new_b_u8;
        self.prefix[rank] = new_p;

        if self.res_vec.len() <= new_b {
            self.res_vec.resize(new_b + 1, 0);
        }
        self.res_vec[new_b] = self.res_vec[new_b].max(new_r);
    }

    pub fn remove_at_rank(&mut self, rank: usize) {
        // 删除顶点时，对应 rank 要从压缩列中删掉。
        if rank < self.bucket_ids.len() {
            self.bucket_ids.remove(rank);
            self.prefix.remove(rank);
        }
    }

    pub fn upper_limit_ratio(&self) -> f64 {
        if self.bucket_ids.is_empty() {
            return 0.0;
        }
        let at_limit = self
            .bucket_ids
            .iter()
            .filter(|&&b| {
                let rem_bits = (b as usize).saturating_sub(8).min(56);
                let max_r = (1u64 << rem_bits).saturating_sub(1);
                self.res_vec.get(b as usize).copied().unwrap_or(0) >= max_r
            })
            .count();
        at_limit as f64 / self.bucket_ids.len() as f64
    }

    pub fn get_compressed_degree_seq(&self) -> GCardResult<Option<CompressedDegreeSeq>> {
        // Path statistics also use the per-bucket recovered maximum instead of
        // the coarse `2^bucket` representative.  This keeps the histogram shape
        // but can make each bucket's degree value tighter.
        self.get_bucket_max_degree_seq()
    }

    pub fn get_bucket_max_degree_seq(&self) -> GCardResult<Option<CompressedDegreeSeq>> {
        if self.bucket_ids.is_empty() {
            return Ok(None);
        }
        let n_buckets = self.res_vec.len();
        let mut counts = vec![0u64; n_buckets];
        let mut bucket_max_values = vec![0u64; n_buckets];
        for (rank, &b) in self.bucket_ids.iter().enumerate() {
            let idx = b as usize;
            if idx >= n_buckets {
                continue;
            }
            counts[idx] += 1;
            let theoretical_upper = bucket_upper_bound(b);
            let value = self
                .recover_upper_bound_at_rank(rank)
                .unwrap_or(0)
                .min(theoretical_upper);
            bucket_max_values[idx] = bucket_max_values[idx].max(value);
        }
        Ok(Some(CompressedDegreeSeq::BucketMax {
            counts,
            bucket_max_values,
        }))
    }

    pub fn disk_size(&self) -> usize {
        self.serialize_compressed().len()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let n = self.bucket_ids.len();
        let rlen = self.res_vec.len();
        let mut out = Vec::with_capacity(n + n + 8 + rlen * 8);
        out.extend_from_slice(&self.bucket_ids);
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&(rlen as u64).to_le_bytes());
        for &v in &self.res_vec {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    pub fn serialize_compressed(&self) -> Vec<u8> {
        let bucket_ids = encode_zstd(&self.bucket_ids);
        let prefix = encode_zstd(&self.prefix);
        let res_vec = encode_raw_u64s(&self.res_vec);

        let mut out = Vec::with_capacity(
            COMPRESSED_MAGIC.len()
                + 8
                + 8
                + bucket_ids.len()
                + 8
                + prefix.len()
                + 8
                + res_vec.len(),
        );
        out.extend_from_slice(COMPRESSED_ZSTD_MAGIC);
        out.extend_from_slice(&(self.bucket_ids.len() as u64).to_le_bytes());
        out.extend_from_slice(&(bucket_ids.len() as u64).to_le_bytes());
        out.extend_from_slice(&bucket_ids);
        out.extend_from_slice(&(prefix.len() as u64).to_le_bytes());
        out.extend_from_slice(&prefix);
        out.extend_from_slice(&(res_vec.len() as u64).to_le_bytes());
        out.extend_from_slice(&res_vec);
        out
    }

    pub fn deserialize(bytes: &[u8], entry_count: usize) -> GCardResult<Self> {
        let need = entry_count
            .checked_mul(2)
            .ok_or_else(|| GCardError::InvalidData("entry_count overflow".into()))?;
        if bytes.len() < need + 8 {
            return Err(GCardError::InvalidData(
                "block too short for bucket_ids and prefix".into(),
            ));
        }
        let bucket_ids = bytes[0..entry_count].to_vec();
        let prefix = bytes[entry_count..entry_count + entry_count].to_vec();
        let rlen = u64::from_le_bytes(
            bytes[need..need + 8]
                .try_into()
                .map_err(|_| GCardError::InvalidData("res_vec len".into()))?,
        ) as usize;
        let res_start = need + 8;
        if bytes.len() < res_start + rlen * 8 {
            return Err(GCardError::InvalidData(
                "block too short for res_vec".into(),
            ));
        }
        let mut res_vec = Vec::with_capacity(rlen);
        for i in 0..rlen {
            let start = res_start + i * 8;
            let v = u64::from_le_bytes(
                bytes[start..start + 8]
                    .try_into()
                    .map_err(|_| GCardError::InvalidData("res_vec u64".into()))?,
            );
            res_vec.push(v);
        }
        Ok(BlockStatistic {
            bucket_ids,
            prefix,
            res_vec,
        })
    }

    pub fn deserialize_compressed(bytes: &[u8]) -> GCardResult<Self> {
        let bucket_ids_are_rle = if bytes.starts_with(COMPRESSED_MAGIC) {
            true
        } else if bytes.starts_with(COMPRESSED_ZSTD_MAGIC) {
            false
        } else {
            return Err(GCardError::InvalidData(
                "compressed block magic mismatch".into(),
            ));
        };
        let mut pos = COMPRESSED_MAGIC.len();
        let entry_count = read_u64(bytes, &mut pos, "entry_count")? as usize;

        let bucket_len = read_u64(bytes, &mut pos, "bucket_ids len")? as usize;
        let bucket_bytes = take_bytes(bytes, &mut pos, bucket_len, "bucket_ids")?;
        let bucket_ids = if bucket_ids_are_rle {
            decode_rle(bucket_bytes)
                .ok_or_else(|| GCardError::InvalidData("decode bucket_ids rle".into()))?
        } else {
            decode_zstd(bucket_bytes)
                .ok_or_else(|| GCardError::InvalidData("decode bucket_ids zstd".into()))?
        };

        let prefix_len = read_u64(bytes, &mut pos, "prefix len")? as usize;
        let prefix_bytes = take_bytes(bytes, &mut pos, prefix_len, "prefix")?;
        let prefix = decode_zstd(prefix_bytes)
            .ok_or_else(|| GCardError::InvalidData("decode prefix zstd".into()))?;

        let res_len = read_u64(bytes, &mut pos, "res_vec len")? as usize;
        let res_bytes = take_bytes(bytes, &mut pos, res_len, "res_vec")?;
        let res_vec = decode_raw_u64s(res_bytes)
            .ok_or_else(|| GCardError::InvalidData("decode res_vec".into()))?;

        if bucket_ids.len() != entry_count || prefix.len() != entry_count {
            return Err(GCardError::InvalidData(
                "compressed block entry_count mismatch".into(),
            ));
        }

        Ok(BlockStatistic {
            bucket_ids,
            prefix,
            res_vec,
        })
    }
}

fn read_u64(bytes: &[u8], pos: &mut usize, field: &str) -> GCardResult<u64> {
    let end = pos
        .checked_add(8)
        .ok_or_else(|| GCardError::InvalidData(format!("{} offset overflow", field)))?;
    if bytes.len() < end {
        return Err(GCardError::InvalidData(format!("missing {}", field)));
    }
    let value = u64::from_le_bytes(
        bytes[*pos..end]
            .try_into()
            .map_err(|_| GCardError::InvalidData(format!("invalid {}", field)))?,
    );
    *pos = end;
    Ok(value)
}

fn take_bytes<'a>(
    bytes: &'a [u8],
    pos: &mut usize,
    len: usize,
    field: &str,
) -> GCardResult<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| GCardError::InvalidData(format!("{} offset overflow", field)))?;
    if bytes.len() < end {
        return Err(GCardError::InvalidData(format!("missing {}", field)));
    }
    let out = &bytes[*pos..end];
    *pos = end;
    Ok(out)
}

impl Serialize for BlockStatistic {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let bytes = self.serialize_compressed();
        s.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for BlockStatistic {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        if bytes.starts_with(COMPRESSED_MAGIC) || bytes.starts_with(COMPRESSED_ZSTD_MAGIC) {
            return Self::deserialize_compressed(&bytes).map_err(serde::de::Error::custom);
        }
        if bytes.len() < 8 {
            return Err(serde::de::Error::custom("block too short for entry_count"));
        }
        let entry_count =
            u64::from_le_bytes(bytes[0..8].try_into().map_err(serde::de::Error::custom)?) as usize;
        Self::deserialize(&bytes[8..], entry_count).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_u64_sequence_empty() {
        let bs = BlockStatistic::from_u64_sequence(&[]).unwrap();
        assert!(bs.bucket_ids.is_empty());
        assert!(bs.prefix.is_empty());
        assert!(bs.res_vec.is_empty());
    }

    #[test]
    fn test_from_u64_sequence_and_get() {
        let values: Vec<u64> = vec![0, 1, 2, 3, 4, 100, 1000];
        let bs = BlockStatistic::from_u64_sequence(&values).unwrap();
        assert_eq!(bs.bucket_ids.len(), values.len());
        assert_eq!(bs.prefix.len(), values.len());
        assert!(bs.res_vec.len() <= bs.bucket_ids.iter().copied().max().unwrap_or(0) as usize + 1);

        for (rank, &v) in values.iter().enumerate() {
            let (b, p, r) = bs.get_by_rank(rank).unwrap();
            let shift = (b as usize).min(56);
            let expected_prefix = if v == 0 { 0 } else { (v >> shift) as u8 };
            let expected_remainder = v & ((1u64 << shift).saturating_sub(1));
            assert_eq!(p, expected_prefix, "rank {} prefix", rank);
            assert!(r >= expected_remainder, "rank {} res_vec upper bound", rank);
        }
    }

    #[test]
    fn test_bucket_ids() {
        let values: Vec<u64> = vec![1, 2, 10, 100];
        let bs = BlockStatistic::from_u64_sequence(&values).unwrap();
        let ids = bs.bucket_ids();
        assert_eq!(ids.len(), 4);
        assert_eq!(ids, bs.bucket_ids.as_slice());
    }

    #[test]
    fn test_serialize_deserialize() {
        let values: Vec<u64> = vec![1, 100, 1000];
        let bs = BlockStatistic::from_u64_sequence(&values).unwrap();
        let entry_count = bs.bucket_ids.len();
        let bytes = bs.serialize();
        let restored = BlockStatistic::deserialize(&bytes, entry_count).unwrap();
        assert_eq!(restored.bucket_ids, bs.bucket_ids);
        assert_eq!(restored.prefix, bs.prefix);
        assert_eq!(restored.res_vec, bs.res_vec);
    }

    #[test]
    fn test_recover_upper_bound_clamps_to_bucket_upper() {
        // Both values are in bucket 12.  The second value raises the shared
        // remainder, which would otherwise make rank 0 recover above 2^12.
        let bs = BlockStatistic::from_u64_sequence(&[4096u64, 2049u64]).unwrap();
        assert_eq!(bs.bucket_ids[0], 12);
        assert_eq!(bs.bucket_ids[1], 12);
        assert_eq!(bs.recover_upper_bound_at_rank(0), Some(4096));
        assert_eq!(bs.recover_upper_bound_at_rank(1), Some(2049));
    }

    #[test]
    fn test_bucket_max_can_be_tighter_than_bucket_id_upper() {
        let bs = BlockStatistic::from_u64_sequence(&[3000u64]).unwrap();
        let bucket = bs.bucket_ids[0];
        let bucket_id_upper = bucket_upper_bound(bucket);
        assert_eq!(bucket, 12);
        assert_eq!(bucket_id_upper, 4096);

        let Some(CompressedDegreeSeq::BucketMax {
            bucket_max_values, ..
        }) = bs.get_bucket_max_degree_seq().unwrap()
        else {
            panic!("expected bucket max degree sequence");
        };
        assert_eq!(bucket_max_values[bucket as usize], 3000);
        assert!(bucket_max_values[bucket as usize] < bucket_id_upper);
    }

    #[test]
    fn test_path_compressed_degree_seq_uses_bucket_max_values() {
        let bs = BlockStatistic::from_u64_sequence(&[3000u64]).unwrap();
        let bucket = bs.bucket_ids[0];

        let Some(CompressedDegreeSeq::BucketMax {
            counts,
            bucket_max_values,
        }) = bs.get_compressed_degree_seq().unwrap()
        else {
            panic!("expected path degree sequence to use bucket max values");
        };
        assert_eq!(counts[bucket as usize], 1);
        assert_eq!(bucket_max_values[bucket as usize], 3000);
        assert!(bucket_max_values[bucket as usize] < bucket_upper_bound(bucket));
    }

    // ── upper_limit_ratio 测试 ────────────────────────────────────────────────

    /// 空序列返回 0.0。
    #[test]
    fn test_upper_limit_ratio_empty() {
        let bs = BlockStatistic::new();
        assert_eq!(bs.upper_limit_ratio(), 0.0);
    }

    /// bucket b < 8 时 rem_bits = 0，max_r = 0，res_vec[b] >= 0 恒成立，
    /// 所有条目均触限，返回 1.0。
    #[test]
    fn test_upper_limit_ratio_small_bucket_always_at_limit() {
        // 1→bucket 0, 3→bucket 2, 5→bucket 3，均 b < 8
        let bs = BlockStatistic::from_u64_sequence(&[1, 3, 5]).unwrap();
        assert!(bs.bucket_ids.iter().all(|&b| b < 8));
        assert_eq!(bs.upper_limit_ratio(), 1.0);
    }

    /// value = 2^20（精确 2 的幂）落在 bucket 20，remainder = 0。
    /// rem_bits = 20 - 8 = 12，max_r = (1<<12)-1 = 4095。
    /// res_vec[20] = 0 < 4095 → 不触限，返回 0.0。
    #[test]
    fn test_upper_limit_ratio_exact_power_not_at_limit() {
        let v = 1u64 << 20; // 1_048_576
        let bs = BlockStatistic::from_u64_sequence(&[v]).unwrap();
        assert_eq!(bs.bucket_ids[0], 20, "应落在 bucket 20");
        assert_eq!(bs.res_vec[20], 0, "remainder 应为 0");
        assert_eq!(bs.upper_limit_ratio(), 0.0);
    }

    /// bucket 20 的普通值（remainder >> max_r），应触限，返回 1.0。
    /// 2^19 + 1 = 524_289 落在 bucket 20，remainder = 524_289 >> (1<<12)-1。
    #[test]
    fn test_upper_limit_ratio_large_bucket_at_limit() {
        let v = (1u64 << 19) + 1; // 524_289，落在 bucket 20
        let bs = BlockStatistic::from_u64_sequence(&[v]).unwrap();
        assert_eq!(bs.bucket_ids[0], 20);
        let rem_bits: usize = 20usize.saturating_sub(8);
        let max_r = (1u64 << rem_bits) - 1; // 4095
        assert!(bs.res_vec[20] >= max_r, "remainder 应触及余量上界");
        assert_eq!(bs.upper_limit_ratio(), 1.0);
    }

    /// 混合：1<<20（bucket 20，不触限）+ 5（bucket 3，b<8 恒触限）→ 0.5。
    #[test]
    fn test_upper_limit_ratio_mixed() {
        let bs = BlockStatistic::from_u64_sequence(&[1u64 << 20, 5]).unwrap();
        let ratio = bs.upper_limit_ratio();
        assert!((ratio - 0.5).abs() < f64::EPSILON, "期望 0.5，实际 {ratio}");
    }

    /// 同桶感染：bucket 12 内两个条目——4096（remainder=0）和 2049（remainder=2049）。
    /// rem_bits = 4，max_r = 15。res_vec[12] = max(0, 2049) = 2049 >= 15，
    /// 导致 bucket 12 的所有条目（含 4096）均被计为触限，返回 1.0。
    #[test]
    fn test_upper_limit_ratio_bucket_infection() {
        // 4096 = 2^12 → bucket 12，remainder = 0（本身不触限）
        // 2049 → bucket 12，remainder = 2049（触限）
        // 两者共享 res_vec[12]，res_vec[12] 被拉高后连带 4096 也触限
        let bs = BlockStatistic::from_u64_sequence(&[4096u64, 2049u64]).unwrap();
        assert_eq!(bs.bucket_ids[0], 12);
        assert_eq!(bs.bucket_ids[1], 12);
        let max_r: u64 = (1u64 << (12usize.saturating_sub(8))) - 1; // 15
        assert!(bs.res_vec[12] >= max_r);
        assert_eq!(bs.upper_limit_ratio(), 1.0);
    }
}
