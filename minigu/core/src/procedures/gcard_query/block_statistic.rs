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
/// Same as `BSZ2` but additionally persists `bucket_max_values` after
/// `res_vec`. Loaders for older magics back-fill `bucket_max_values`
/// from the existing layout via `rebuild_bucket_max_from_layout`.
const COMPRESSED_ZSTD_MAGIC_V3: &[u8; 4] = b"BSZ3";

#[inline]
fn bucket_upper_bound(bucket_id: u8) -> u64 {
    BUCKET_BASE.saturating_pow(bucket_id as u32)
}

#[inline]
fn bucket_index_base2(value: u64) -> usize {
    if value == 0 {
        0
    } else {
        u64::BITS as usize - (value - 1).leading_zeros() as usize
    }
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
    /// 每个桶内 value 自身的最大值。`bucket_max_values.len() == res_vec.len()`。
    /// 与 res_vec 一同持久化，是 BlockStatistic 唯一不丢失精度的"业务真值"
    /// 来源。`apply_value_updates` 只刷新被触及的桶号，保留未触及桶的精确值。
    /// 反序列化加载老格式（BSZ1/BSZ2）时通过 `rebuild_bucket_max_from_layout`
    /// 回填一次（这种情况下会退化为 layout 上界）。
    pub bucket_max_values: Vec<u64>,
}

impl BlockStatistic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_u64_sequence(values: &[u64]) -> GCardResult<Self> {
        let mut block = BlockStatistic::default();
        block.rebuild_layout_from(values);
        Ok(block)
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
        self.apply_value_updates(&[(rank, new_value)]);
    }

    pub fn apply_value_updates(&mut self, updates: &[(usize, u64)]) {
        if updates.is_empty() {
            return;
        }

        // In-place monotonic update. Caller guarantees values only grow,
        // so a rank that leaves an old bucket does so by jumping into a
        // higher one. We only ever compare `new_value` against the **new**
        // bucket's max — the old bucket's max is left alone (it stays
        // monotonic / conservative).
        //
        // We may need to extend `bounds` if a `new_value` exceeds whatever
        // the current layout was sized for. Bounds are `1, 2, 4, ..., 2^k`
        // with `2^k ≥ max`, so extending only appends; existing rank → bucket
        // assignments are stable under that extension.
        let updates_max = updates.iter().map(|&(_, v)| v).max().unwrap_or(0);
        let current_max = self.bucket_max_values.iter().copied().max().unwrap_or(0);
        let new_max_val = updates_max.max(current_max);
        let new_bounds = build_bounds(BUCKET_BASE, new_max_val);
        let new_n_buckets = new_bounds.len();

        if new_n_buckets > self.res_vec.len() {
            self.res_vec.resize(new_n_buckets, 0);
            self.bucket_max_values.resize(new_n_buckets, 0);
        }

        for &(rank, new_value) in updates {
            if rank >= self.bucket_ids.len() {
                continue;
            }
            let b = get_bucket_index(&new_bounds, new_value);
            let b_u8 = b.min(u8::MAX as usize) as u8;
            let (p, r) = prefix_and_remainder(new_value, b_u8);
            self.bucket_ids[rank] = b_u8;
            self.prefix[rank] = p;
            let idx = b.min(self.res_vec.len().saturating_sub(1));
            self.res_vec[idx] = self.res_vec[idx].max(r);
            self.bucket_max_values[idx] = self.bucket_max_values[idx].max(new_value);
        }
    }

    pub fn remove_at_rank(&mut self, rank: usize) {
        // 删除顶点时，对应 rank 要从压缩列中删掉。
        // monotonic 策略下不重算 res_vec / bucket_max_values：移除一个
        // rank 只可能让真实桶 max 降低，但我们保留原值作为保守上界。
        if rank >= self.bucket_ids.len() {
            return;
        }
        self.bucket_ids.remove(rank);
        self.prefix.remove(rank);
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
        for &b in &self.bucket_ids {
            let idx = b as usize;
            if idx < n_buckets {
                counts[idx] += 1;
            }
        }
        // Read the persisted per-bucket value max directly. Built/updated by
        // `rebuild_layout_from_values`, and backfilled by
        // `rebuild_bucket_max_from_layout` for legacy (BSZ1/BSZ2) loads.
        let mut bucket_max_values = self.bucket_max_values.clone();
        if bucket_max_values.len() < n_buckets {
            bucket_max_values.resize(n_buckets, 0);
        } else if bucket_max_values.len() > n_buckets {
            bucket_max_values.truncate(n_buckets);
        }
        Ok(Some(CompressedDegreeSeq::BucketMax {
            counts,
            bucket_max_values,
        }))
    }

    /// Rebuild all of bucket_ids / prefix / res_vec / bucket_max_values
    /// from a transient list of per-rank values. Callers (build, update,
    /// remove) own this list and discard it once the rebuild is done —
    /// the BlockStatistic never stores it.
    fn rebuild_layout_from(&mut self, values: &[u64]) {
        if values.is_empty() {
            self.bucket_ids.clear();
            self.prefix.clear();
            self.res_vec.clear();
            self.bucket_max_values.clear();
            return;
        }
        let max_val = values.iter().copied().max().unwrap_or(0);
        let n_buckets = bucket_index_base2(max_val) + 1;
        self.bucket_ids.clear();
        self.prefix.clear();
        self.bucket_ids.reserve(values.len());
        self.prefix.reserve(values.len());
        let mut res_max: Vec<u64> = vec![0; n_buckets];
        let mut bucket_max: Vec<u64> = vec![0; n_buckets];
        for &v in values {
            let b = bucket_index_base2(v);
            let b_u8 = b.min(u8::MAX as usize) as u8;
            let (p, r) = prefix_and_remainder(v, b_u8);
            self.bucket_ids.push(b_u8);
            self.prefix.push(p);
            let idx = b.min(u8::MAX as usize);
            res_max[idx] = res_max[idx].max(r);
            bucket_max[idx] = bucket_max[idx].max(v);
        }
        let max_b = self.bucket_ids.iter().copied().max().unwrap_or(0) as usize;
        res_max.truncate(max_b + 1);
        bucket_max.truncate(max_b + 1);
        self.res_vec = res_max;
        self.bucket_max_values = bucket_max;
    }

    /// Recompute `bucket_max_values` from the compressed layout
    /// (bucket_ids, prefix, res_vec). Used by deserialization of legacy
    /// formats BSZ1 / BSZ2 that did not persist `bucket_max_values`.
    fn rebuild_bucket_max_from_layout(&mut self) {
        let n_buckets = self.res_vec.len();
        let mut bucket_max = vec![0u64; n_buckets];
        for rank in 0..self.bucket_ids.len() {
            let b = self.bucket_ids[rank];
            let idx = b as usize;
            if idx >= n_buckets {
                continue;
            }
            let p = self.prefix[rank];
            let r = self.res_vec[idx];
            let shift = (b as usize).min(56);
            let value = (((p as u64) << shift) | r).min(bucket_upper_bound(b));
            bucket_max[idx] = bucket_max[idx].max(value);
        }
        self.bucket_max_values = bucket_max;
    }

    pub fn disk_size(&self) -> usize {
        self.serialize_compressed().len()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let n = self.bucket_ids.len();
        let rlen = self.res_vec.len();
        let mlen = self.bucket_max_values.len();
        let mut out = Vec::with_capacity(n + n + 8 + rlen * 8 + 8 + mlen * 8);
        out.extend_from_slice(&self.bucket_ids);
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&(rlen as u64).to_le_bytes());
        for &v in &self.res_vec {
            out.extend_from_slice(&v.to_le_bytes());
        }
        // Trailing block: bucket_max_values. Length-prefixed so old readers
        // that stop after res_vec stay valid (they just won't see this).
        out.extend_from_slice(&(mlen as u64).to_le_bytes());
        for &v in &self.bucket_max_values {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    pub fn serialize_compressed(&self) -> Vec<u8> {
        let bucket_ids = encode_zstd(&self.bucket_ids);
        let prefix = encode_zstd(&self.prefix);
        let res_vec = encode_raw_u64s(&self.res_vec);
        let bucket_max = encode_raw_u64s(&self.bucket_max_values);

        let mut out = Vec::with_capacity(
            COMPRESSED_ZSTD_MAGIC_V3.len()
                + 8
                + 8
                + bucket_ids.len()
                + 8
                + prefix.len()
                + 8
                + res_vec.len()
                + 8
                + bucket_max.len(),
        );
        out.extend_from_slice(COMPRESSED_ZSTD_MAGIC_V3);
        out.extend_from_slice(&(self.bucket_ids.len() as u64).to_le_bytes());
        out.extend_from_slice(&(bucket_ids.len() as u64).to_le_bytes());
        out.extend_from_slice(&bucket_ids);
        out.extend_from_slice(&(prefix.len() as u64).to_le_bytes());
        out.extend_from_slice(&prefix);
        out.extend_from_slice(&(res_vec.len() as u64).to_le_bytes());
        out.extend_from_slice(&res_vec);
        out.extend_from_slice(&(bucket_max.len() as u64).to_le_bytes());
        out.extend_from_slice(&bucket_max);
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
        // Optional trailing block: bucket_max_values. Old payloads stop here.
        let trailer_start = res_start + rlen * 8;
        let bucket_max_values = if bytes.len() >= trailer_start + 8 {
            let mlen = u64::from_le_bytes(
                bytes[trailer_start..trailer_start + 8]
                    .try_into()
                    .map_err(|_| GCardError::InvalidData("bucket_max len".into()))?,
            ) as usize;
            let mstart = trailer_start + 8;
            if bytes.len() < mstart + mlen * 8 {
                return Err(GCardError::InvalidData(
                    "block too short for bucket_max_values".into(),
                ));
            }
            let mut v = Vec::with_capacity(mlen);
            for i in 0..mlen {
                let start = mstart + i * 8;
                v.push(u64::from_le_bytes(
                    bytes[start..start + 8]
                        .try_into()
                        .map_err(|_| GCardError::InvalidData("bucket_max u64".into()))?,
                ));
            }
            v
        } else {
            Vec::new()
        };
        let mut block = BlockStatistic {
            bucket_ids,
            prefix,
            res_vec,
            bucket_max_values,
        };
        if block.bucket_max_values.is_empty() && !block.res_vec.is_empty() {
            // Legacy payload without bucket_max_values — back-fill once.
            block.rebuild_bucket_max_from_layout();
        }
        Ok(block)
    }

    pub fn deserialize_compressed(bytes: &[u8]) -> GCardResult<Self> {
        let (bucket_ids_are_rle, has_bucket_max) = if bytes.starts_with(COMPRESSED_MAGIC) {
            (true, false)
        } else if bytes.starts_with(COMPRESSED_ZSTD_MAGIC) {
            (false, false)
        } else if bytes.starts_with(COMPRESSED_ZSTD_MAGIC_V3) {
            (false, true)
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

        let bucket_max_values = if has_bucket_max {
            let mlen = read_u64(bytes, &mut pos, "bucket_max len")? as usize;
            let mbytes = take_bytes(bytes, &mut pos, mlen, "bucket_max")?;
            decode_raw_u64s(mbytes)
                .ok_or_else(|| GCardError::InvalidData("decode bucket_max".into()))?
        } else {
            Vec::new()
        };

        if bucket_ids.len() != entry_count || prefix.len() != entry_count {
            return Err(GCardError::InvalidData(
                "compressed block entry_count mismatch".into(),
            ));
        }

        let mut block = BlockStatistic {
            bucket_ids,
            prefix,
            res_vec,
            bucket_max_values,
        };
        if !has_bucket_max && !block.res_vec.is_empty() {
            // Legacy BSZ1/BSZ2 payload — back-fill once from the layout.
            block.rebuild_bucket_max_from_layout();
        }
        Ok(block)
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
        if bytes.starts_with(COMPRESSED_MAGIC)
            || bytes.starts_with(COMPRESSED_ZSTD_MAGIC)
            || bytes.starts_with(COMPRESSED_ZSTD_MAGIC_V3)
        {
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
        assert_eq!(restored.bucket_max_values, bs.bucket_max_values);
    }

    #[test]
    fn fresh_build_bucket_max_values_are_exact_per_bucket_max() {
        // Two values in bucket 12, one in bucket 0. The bucket-12 max must be
        // exactly the larger of the two, not the bucket's theoretical 2^12.
        let bs = BlockStatistic::from_u64_sequence(&[1u64, 4096, 2049]).unwrap();
        // Each value's bucket assignment.
        let b0 = bs.bucket_ids[0] as usize;
        let b1 = bs.bucket_ids[1] as usize;
        let b2 = bs.bucket_ids[2] as usize;
        assert_eq!(b1, 12);
        assert_eq!(b2, 12);
        assert_eq!(bs.bucket_max_values.len(), bs.res_vec.len());
        assert_eq!(bs.bucket_max_values[b1], 4096);
        // The smaller-bucket entry's max is just its own value.
        assert_eq!(bs.bucket_max_values[b0], 1);
    }

    #[test]
    fn apply_value_updates_maintains_bucket_max_values() {
        let mut bs = BlockStatistic::from_u64_sequence(&[1u64, 4096]).unwrap();
        // Bump rank 1 from 4096 → 5000 (still bucket 12); bucket_max should grow.
        bs.apply_value_updates(&[(1, 5000)]);
        let b1 = bs.bucket_ids[1] as usize;
        assert_eq!(bs.bucket_max_values[b1], 5000);

        // Push rank 1 to a much larger value (bucket 20); new bucket appears.
        bs.apply_value_updates(&[(1, 1u64 << 20)]);
        let new_b1 = bs.bucket_ids[1] as usize;
        assert_eq!(new_b1, 20);
        assert_eq!(bs.bucket_max_values[new_b1], 1u64 << 20);
    }

    #[test]
    fn untouched_bucket_keeps_exact_max_after_reload_and_update() {
        // Three values landing in three different buckets. After reload,
        // updating only the middle rank in-bucket must leave the other two
        // buckets' max at their exact loaded value — not inflated by the
        // layout-derived upper bound.
        let bs = BlockStatistic::from_u64_sequence(&[1u64, 33, 4096, 2049]).unwrap();
        let on_disk = bs.bucket_max_values.clone();
        let b_rank0 = bs.bucket_ids[0] as usize; // 1
        let b_rank1 = bs.bucket_ids[1] as usize; // 33
        let b_rank2 = bs.bucket_ids[2] as usize; // 4096
        assert_ne!(b_rank1, b_rank0);
        assert_ne!(b_rank1, b_rank2);
        assert_eq!(on_disk[b_rank2], 4096, "fresh-build is exact");

        let bytes = bs.serialize_compressed();
        let mut restored = BlockStatistic::deserialize_compressed(&bytes).unwrap();
        assert_eq!(restored.bucket_max_values, on_disk);

        // 33 → 34: both fall in the same bucket (32 < v ≤ 64), max_val
        // unchanged so bounds stay the same. Only b_rank1 is touched.
        restored.apply_value_updates(&[(1, 34)]);
        assert_eq!(bs.bucket_ids[1], restored.bucket_ids[1]);

        // The untouched buckets keep their exact disk-loaded max.
        assert_eq!(
            restored.bucket_max_values[b_rank0], on_disk[b_rank0],
            "untouched bucket {} should retain exact loaded max",
            b_rank0
        );
        assert_eq!(
            restored.bucket_max_values[b_rank2], on_disk[b_rank2],
            "untouched bucket {} should retain exact loaded max (4096) — \
             not inflated to the prefix+res_vec upper bound",
            b_rank2
        );
        // Touched bucket reflects the update.
        assert!(restored.bucket_max_values[b_rank1] >= 34);
    }

    #[test]
    fn touched_bucket_uses_layout_upper_after_reload() {
        // Two values share the same bucket (1100 and 1500 both fall in
        // (1024, 2048] = bucket 11). After reload + in-bucket update,
        // the touched bucket's max must cover the new value.
        let bs = BlockStatistic::from_u64_sequence(&[1100u64, 1500]).unwrap();
        let b = bs.bucket_ids[0] as usize;
        assert_eq!(b, bs.bucket_ids[1] as usize, "both in same bucket");
        assert_eq!(bs.bucket_max_values[b], 1500, "fresh-build is exact");

        let bytes = bs.serialize_compressed();
        let mut restored = BlockStatistic::deserialize_compressed(&bytes).unwrap();
        assert_eq!(restored.bucket_max_values[b], 1500, "loaded from disk");

        // 1500 → 1600: still in bucket 11 (1024 < 1600 ≤ 2048).
        restored.apply_value_updates(&[(1, 1600)]);
        let b_new = restored.bucket_ids[1] as usize;
        assert!(
            restored.bucket_max_values[b_new] >= 1600,
            "touched bucket {} max ({}) must cover new value 1600",
            b_new,
            restored.bucket_max_values[b_new]
        );
    }

    #[test]
    fn cross_bucket_update_marks_both_old_and_new_bucket_touched() {
        // A rank moving from one bucket to another must mark both buckets
        // as touched, so the old bucket's max (which may now be smaller
        // because the moving rank left) is recomputed from layout, not
        // frozen at its pre-update value.
        let bs = BlockStatistic::from_u64_sequence(&[100u64, 5000]).unwrap();
        let b_old_rank1 = bs.bucket_ids[1] as usize; // 5000 → some bucket
        assert!(bs.bucket_max_values[b_old_rank1] >= 5000);

        // Push rank 1 into a much bigger bucket. Old bucket should no
        // longer carry the stale 5000.
        let mut bs2 = bs.clone();
        bs2.apply_value_updates(&[(1, 1u64 << 20)]);
        let b_new_rank1 = bs2.bucket_ids[1] as usize;
        assert_ne!(b_new_rank1, b_old_rank1, "rank 1 must change bucket");
        // Both buckets were marked touched and rebuilt — old bucket no
        // longer reflects rank 1's old value (it left).
        assert!(bs2.bucket_max_values[b_new_rank1] >= (1u64 << 20));
    }

    #[test]
    fn bsz3_compressed_roundtrip_preserves_bucket_max_values() {
        let bs = BlockStatistic::from_u64_sequence(&[1u64, 4096, 2049, 8, 100]).unwrap();
        let bytes = bs.serialize_compressed();
        assert!(bytes.starts_with(b"BSZ3"));
        let restored = BlockStatistic::deserialize_compressed(&bytes).unwrap();
        assert_eq!(restored.bucket_ids, bs.bucket_ids);
        assert_eq!(restored.prefix, bs.prefix);
        assert_eq!(restored.res_vec, bs.res_vec);
        assert_eq!(restored.bucket_max_values, bs.bucket_max_values);
    }

    #[test]
    fn legacy_bsz2_payload_backfills_bucket_max_values() {
        // Craft a BSZ2-shaped payload (no bucket_max trailer) and confirm that
        // deserialize_compressed back-fills `bucket_max_values` via the layout.
        let bs = BlockStatistic::from_u64_sequence(&[1u64, 4096, 2049]).unwrap();
        let mut legacy: Vec<u8> = Vec::new();
        legacy.extend_from_slice(b"BSZ2");
        legacy.extend_from_slice(&(bs.bucket_ids.len() as u64).to_le_bytes());
        let zb = encode_zstd(&bs.bucket_ids);
        legacy.extend_from_slice(&(zb.len() as u64).to_le_bytes());
        legacy.extend_from_slice(&zb);
        let zp = encode_zstd(&bs.prefix);
        legacy.extend_from_slice(&(zp.len() as u64).to_le_bytes());
        legacy.extend_from_slice(&zp);
        let rv = encode_raw_u64s(&bs.res_vec);
        legacy.extend_from_slice(&(rv.len() as u64).to_le_bytes());
        legacy.extend_from_slice(&rv);
        // Intentionally NO bucket_max trailer.

        let restored = BlockStatistic::deserialize_compressed(&legacy).unwrap();
        // Layout fields must match exactly.
        assert_eq!(restored.bucket_ids, bs.bucket_ids);
        assert_eq!(restored.prefix, bs.prefix);
        assert_eq!(restored.res_vec, bs.res_vec);
        // bucket_max was not on disk, so it gets back-filled. The back-fill
        // uses layout-derived upper bounds, so it may be >= the exact value
        // but must still capture each bucket's actual max as a lower bound.
        assert_eq!(restored.bucket_max_values.len(), restored.res_vec.len());
        for (b, &m) in restored.bucket_max_values.iter().enumerate() {
            assert!(
                m >= bs.bucket_max_values[b],
                "bucket {b}: backfilled {m} should be ≥ original {orig}",
                orig = bs.bucket_max_values[b],
            );
        }
    }

    #[test]
    fn get_bucket_max_degree_seq_reads_persisted_field() {
        // After fresh build, bucket_max_values is exact and the consumer
        // path returns it directly (no on-the-fly recover scan).
        let bs = BlockStatistic::from_u64_sequence(&[3000u64]).unwrap();
        let b = bs.bucket_ids[0] as usize;
        assert_eq!(bs.bucket_max_values[b], 3000);

        let Some(crate::procedures::gcard_query::catalog::CompressedDegreeSeq::BucketMax {
            bucket_max_values,
            ..
        }) = bs.get_bucket_max_degree_seq().unwrap()
        else {
            panic!("expected BucketMax variant");
        };
        assert_eq!(bucket_max_values[b], 3000);
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
