//! Per-component compression for Statistic persistence.
//!
//! Each component uses the encoding best suited to its data characteristics:
//! - vertex_ids (sorted u64): delta encoding + varint
//! - bucket_ids (u8, 0-63, repetitive): RLE
//! - prefix (u8, 0-255): zstd block compression
//! - res_vec (short Vec<u64>): no compression
//! - AltKey strings (repetitive labels): string dictionary + index references

// ── Delta + Varint encoding for sorted vertex_ids ───────────────────────────

/// Encode a u64 as a variable-length integer (LEB128).
fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        } else {
            out.push(byte | 0x80);
        }
    }
}

/// Decode a varint from the given slice, returning (value, bytes_consumed).
fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in data.iter().enumerate() {
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

/// Delta-encode sorted vertex_ids: store first value as-is, then deltas, all as varints.
pub fn encode_vertex_ids(vertex_ids: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vertex_ids.len() * 4);
    encode_varint(vertex_ids.len() as u64, &mut out);
    if vertex_ids.is_empty() {
        return out;
    }
    encode_varint(vertex_ids[0], &mut out);
    for i in 1..vertex_ids.len() {
        let delta = vertex_ids[i] - vertex_ids[i - 1];
        encode_varint(delta, &mut out);
    }
    out
}

/// Decode delta-encoded vertex_ids.
pub fn decode_vertex_ids(data: &[u8]) -> Option<Vec<u64>> {
    let mut pos = 0;
    let (count, consumed) = decode_varint(&data[pos..])?;
    pos += consumed;
    let count = count as usize;
    if count == 0 {
        return Some(Vec::new());
    }
    let mut result = Vec::with_capacity(count);
    let (first, consumed) = decode_varint(&data[pos..])?;
    pos += consumed;
    result.push(first);
    for _ in 1..count {
        let (delta, consumed) = decode_varint(&data[pos..])?;
        pos += consumed;
        result.push(result.last().unwrap() + delta);
    }
    Some(result)
}

// ── RLE encoding for bucket_ids ─────────────────────────────────────────────

/// RLE-encode a byte slice: (value, run_length) pairs.
/// Format: [total_count: u32] [value: u8, run_length: varint]*
pub fn encode_rle(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    if data.is_empty() {
        return out;
    }
    let mut cur = data[0];
    let mut run: u64 = 1;
    for &b in &data[1..] {
        if b == cur {
            run += 1;
        } else {
            out.push(cur);
            encode_varint(run, &mut out);
            cur = b;
            run = 1;
        }
    }
    out.push(cur);
    encode_varint(run, &mut out);
    out
}

/// Decode RLE-encoded bytes.
pub fn decode_rle(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 {
        return None;
    }
    let total = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let mut result = Vec::with_capacity(total);
    let mut pos = 4;
    while result.len() < total {
        if pos >= data.len() {
            return None;
        }
        let value = data[pos];
        pos += 1;
        let (run, consumed) = decode_varint(&data[pos..])?;
        pos += consumed;
        for _ in 0..run {
            result.push(value);
        }
    }
    Some(result)
}

// ── zstd block compression for prefix ───────────────────────────────────────

/// Compress bytes with zstd level 3.
pub fn encode_zstd(data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_le_bytes().to_vec();
    let compressed = zstd::encode_all(data, 3).unwrap_or_else(|_| data.to_vec());
    out.extend(compressed);
    out
}

/// Decompress zstd-compressed bytes.
pub fn decode_zstd(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 {
        return None;
    }
    let _original_len = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    zstd::decode_all(&data[4..]).ok()
}

// ── No compression for res_vec (raw u64 LE) ─────────────────────────────────

pub fn encode_raw_u64s(values: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + values.len() * 8);
    out.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for &v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn decode_raw_u64s(data: &[u8]) -> Option<Vec<u64>> {
    if data.len() < 4 {
        return None;
    }
    let count = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if data.len() < 4 + count * 8 {
        return None;
    }
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let start = 4 + i * 8;
        let v = u64::from_le_bytes(data[start..start + 8].try_into().ok()?);
        result.push(v);
    }
    Some(result)
}

// ── String dictionary for AltKey ────────────────────────────────────────────

use std::collections::HashMap;

use crate::procedures::gcard_query::catalog::AltKey;

/// Build a string dictionary from a set of AltKeys.
/// Returns (dictionary: Vec<String>, encoded_keys: Vec<Vec<u32>>).
/// Each AltKey becomes a list of dictionary indices.
pub fn build_string_dict(keys: &[&AltKey]) -> (Vec<String>, Vec<Vec<u32>>) {
    let mut dict: Vec<String> = Vec::new();
    let mut str_to_idx: HashMap<&str, u32> = HashMap::new();
    let mut encoded = Vec::with_capacity(keys.len());

    for key in keys {
        let mut indices = Vec::with_capacity(key.raw.len());
        for s in &key.raw {
            let idx = if let Some(&idx) = str_to_idx.get(s.as_str()) {
                idx
            } else {
                let idx = dict.len() as u32;
                str_to_idx.insert(s.as_str(), idx);
                dict.push(s.clone());
                idx
            };
            indices.push(idx);
        }
        encoded.push(indices);
    }
    (dict, encoded)
}

/// Encode the string dictionary to bytes.
pub fn encode_string_dict(dict: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint(dict.len() as u64, &mut out);
    for s in dict {
        let bytes = s.as_bytes();
        encode_varint(bytes.len() as u64, &mut out);
        out.extend_from_slice(bytes);
    }
    out
}

/// Decode a string dictionary from bytes, returning (dict, bytes_consumed).
pub fn decode_string_dict(data: &[u8]) -> Option<(Vec<String>, usize)> {
    let mut pos = 0;
    let (count, consumed) = decode_varint(&data[pos..])?;
    pos += consumed;
    let mut dict = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (len, consumed) = decode_varint(&data[pos..])?;
        pos += consumed;
        let len = len as usize;
        let s = std::str::from_utf8(&data[pos..pos + len]).ok()?;
        dict.push(s.to_string());
        pos += len;
    }
    Some((dict, pos))
}

/// Encode a list of dictionary-indexed AltKeys.
pub fn encode_dict_keys(keys: &[Vec<u32>]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint(keys.len() as u64, &mut out);
    for key in keys {
        encode_varint(key.len() as u64, &mut out);
        for &idx in key {
            encode_varint(idx as u64, &mut out);
        }
    }
    out
}

/// Decode dictionary-indexed AltKeys.
pub fn decode_dict_keys(data: &[u8]) -> Option<(Vec<Vec<u32>>, usize)> {
    let mut pos = 0;
    let (count, consumed) = decode_varint(&data[pos..])?;
    pos += consumed;
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (len, consumed) = decode_varint(&data[pos..])?;
        pos += consumed;
        let mut key = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let (idx, consumed) = decode_varint(&data[pos..])?;
            pos += consumed;
            key.push(idx as u32);
        }
        keys.push(key);
    }
    Some((keys, pos))
}

// ── Per-component size report ───────────────────────────────────────────────

/// Report per-component raw vs compressed sizes for a Statistic.
pub fn report_component_sizes(stat: &super::Statistic) {
    let mut total_vertex_ids_raw: usize = 0;
    let mut total_vertex_ids_compressed: usize = 0;
    let mut path_bucket_ids_raw: usize = 0;
    let mut path_bucket_ids_compressed: usize = 0;
    let mut path_prefix_raw: usize = 0;
    let mut path_prefix_compressed: usize = 0;
    let mut path_res_vec_raw: usize = 0;
    let mut path_res_vec_encoded: usize = 0;
    let mut star_bucket_ids_raw: usize = 0;
    let mut star_bucket_ids_compressed: usize = 0;
    let mut star_prefix_raw: usize = 0;
    let mut star_prefix_compressed: usize = 0;
    let mut star_res_vec_raw: usize = 0;
    let mut star_res_vec_encoded: usize = 0;
    let mut all_alt_keys: Vec<&AltKey> = Vec::new();
    let mut alt_key_raw_size: usize = 0;
    let mut star_key_raw_size: usize = 0;

    for ls in stat.label_path_statistic.values() {
        // vertex_ids
        let raw = ls.vertex_ids.len() * 8;
        let compressed = encode_vertex_ids(&ls.vertex_ids);
        total_vertex_ids_raw += raw;
        total_vertex_ids_compressed += compressed.len();

        for (alt_key, block) in &ls.path_statistic {
            // bucket_ids
            path_bucket_ids_raw += block.bucket_ids.len();
            let zstd = encode_zstd(&block.bucket_ids);
            path_bucket_ids_compressed += zstd.len();

            // prefix
            path_prefix_raw += block.prefix.len();
            let zstd = encode_zstd(&block.prefix);
            path_prefix_compressed += zstd.len();

            // res_vec
            let raw_res = block.res_vec.len() * 8;
            let encoded_res = encode_raw_u64s(&block.res_vec);
            path_res_vec_raw += raw_res;
            path_res_vec_encoded += encoded_res.len();

            // alt_key raw size
            let mut key_size = 8; // segment count
            for s in &alt_key.raw {
                key_size += 8 + s.as_bytes().len();
            }
            alt_key_raw_size += key_size;
            all_alt_keys.push(alt_key);
        }

        for (star_key, block) in &ls.star_statistic {
            star_bucket_ids_raw += block.bucket_ids.len();
            let zstd = encode_zstd(&block.bucket_ids);
            star_bucket_ids_compressed += zstd.len();

            star_prefix_raw += block.prefix.len();
            let zstd = encode_zstd(&block.prefix);
            star_prefix_compressed += zstd.len();

            let raw_res = block.res_vec.len() * 8;
            let encoded_res = encode_raw_u64s(&block.res_vec);
            star_res_vec_raw += raw_res;
            star_res_vec_encoded += encoded_res.len();

            star_key_raw_size += star_key_bincode_size(star_key);
        }
    }

    // String dictionary for AltKeys
    let (dict, encoded_keys) = build_string_dict(&all_alt_keys);
    let dict_bytes = encode_string_dict(&dict);
    let keys_bytes = encode_dict_keys(&encoded_keys);
    let alt_key_compressed = dict_bytes.len() + keys_bytes.len();
    let star_key_compressed = star_key_raw_size;

    let total_raw = total_vertex_ids_raw
        + path_bucket_ids_raw
        + path_prefix_raw
        + path_res_vec_raw
        + star_bucket_ids_raw
        + star_prefix_raw
        + star_res_vec_raw
        + alt_key_raw_size
        + star_key_raw_size;
    let total_compressed = total_vertex_ids_compressed
        + path_bucket_ids_compressed
        + path_prefix_compressed
        + path_res_vec_encoded
        + star_bucket_ids_compressed
        + star_prefix_compressed
        + star_res_vec_encoded
        + alt_key_compressed
        + star_key_compressed;

    println!("=== Per-component compression report ===");
    println!(
        "  {:20} {:>12} {:>12} {:>8}",
        "Component", "Raw", "Compressed", "Ratio"
    );
    println!("  {:-<56}", "");
    print_row(
        "vertex_ids (delta+varint)",
        total_vertex_ids_raw,
        total_vertex_ids_compressed,
    );
    print_row(
        "path bucket_ids (zstd)",
        path_bucket_ids_raw,
        path_bucket_ids_compressed,
    );
    print_row(
        "path prefix (zstd)",
        path_prefix_raw,
        path_prefix_compressed,
    );
    print_row("path res_vec (raw)", path_res_vec_raw, path_res_vec_encoded);
    print_row(
        "star bucket_ids (zstd)",
        star_bucket_ids_raw,
        star_bucket_ids_compressed,
    );
    print_row(
        "star prefix (zstd)",
        star_prefix_raw,
        star_prefix_compressed,
    );
    print_row("star res_vec (raw)", star_res_vec_raw, star_res_vec_encoded);
    print_row("alt_keys (dict)", alt_key_raw_size, alt_key_compressed);
    print_row(
        "star_keys (bincode)",
        star_key_raw_size,
        star_key_compressed,
    );
    println!("  {:-<56}", "");
    print_row("TOTAL", total_raw, total_compressed);
    println!(
        "Estimated component-compressed statistic payload: {} bytes",
        total_compressed
    );
    println!(
        "Note: BlockStatistic bucket_ids/prefix compression is persisted by save_statistic; vertex/key dictionary sizes are report-only estimates."
    );
    println!("========================================");
}

fn star_key_bincode_size(star_key: &crate::procedures::gcard_query::utils::StarStatKey) -> usize {
    bincode::serialized_size(star_key).unwrap_or(0) as usize
}

fn print_row(name: &str, raw: usize, compressed: usize) {
    let ratio = if compressed > 0 {
        raw as f64 / compressed as f64
    } else {
        0.0
    };
    println!(
        "  {:25} {:>8.2} MB {:>8.2} MB {:>7.1}x",
        name,
        raw as f64 / 1024.0 / 1024.0,
        compressed as f64 / 1024.0 / 1024.0,
        ratio,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_roundtrip() {
        for &v in &[0u64, 1, 127, 128, 16383, 16384, u64::MAX] {
            let mut buf = Vec::new();
            encode_varint(v, &mut buf);
            let (decoded, _) = decode_varint(&buf).unwrap();
            assert_eq!(v, decoded);
        }
    }

    #[test]
    fn test_vertex_ids_roundtrip() {
        let ids: Vec<u64> = vec![10, 20, 25, 100, 105, 1000];
        let encoded = encode_vertex_ids(&ids);
        let decoded = decode_vertex_ids(&encoded).unwrap();
        assert_eq!(ids, decoded);
    }

    #[test]
    fn test_vertex_ids_empty() {
        let ids: Vec<u64> = vec![];
        let encoded = encode_vertex_ids(&ids);
        let decoded = decode_vertex_ids(&encoded).unwrap();
        assert_eq!(ids, decoded);
    }

    #[test]
    fn test_rle_roundtrip() {
        let data = vec![0u8, 0, 0, 1, 1, 2, 2, 2, 2, 3];
        let encoded = encode_rle(&data);
        let decoded = decode_rle(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_rle_empty() {
        let data: Vec<u8> = vec![];
        let encoded = encode_rle(&data);
        let decoded = decode_rle(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_rle_single() {
        let data = vec![5u8; 1000];
        let encoded = encode_rle(&data);
        assert!(encoded.len() < 20); // should be very small
        let decoded = decode_rle(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_zstd_roundtrip() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 64) as u8).collect();
        let encoded = encode_zstd(&data);
        let decoded = decode_zstd(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_raw_u64s_roundtrip() {
        let values: Vec<u64> = vec![0, 42, u64::MAX, 12345];
        let encoded = encode_raw_u64s(&values);
        let decoded = decode_raw_u64s(&encoded).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_string_dict_roundtrip() {
        let k1 = AltKey::new(vec!["Person".into(), "knows".into(), "Person".into()]);
        let k2 = AltKey::new(vec!["Person".into(), "likes".into(), "Comment".into()]);
        let keys = vec![&k1, &k2];
        let (dict, encoded) = build_string_dict(&keys);

        // "Person" should appear only once in dictionary
        assert_eq!(dict.iter().filter(|s| *s == "Person").count(), 1);

        let dict_bytes = encode_string_dict(&dict);
        let (dict_decoded, _) = decode_string_dict(&dict_bytes).unwrap();
        assert_eq!(dict, dict_decoded);

        let keys_bytes = encode_dict_keys(&encoded);
        let (keys_decoded, _) = decode_dict_keys(&keys_bytes).unwrap();
        assert_eq!(encoded, keys_decoded);

        // Reconstruct AltKeys
        let k1_restored: Vec<String> = keys_decoded[0]
            .iter()
            .map(|&i| dict_decoded[i as usize].clone())
            .collect();
        assert_eq!(k1.raw, k1_restored);
    }
}
