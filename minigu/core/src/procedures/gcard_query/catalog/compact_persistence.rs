//! Deterministic compact persistence for the query-ready GCard catalog.
//!
//! Paths and labels are dictionary encoded once. Histogram counts and bucket
//! maxima use unsigned LEB128 varints; a dense vector position is the bucket
//! id, so no bucket-id column is persisted.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use super::{AltKey, CompressedDegreeSeq, DegreeSeqGraphCompressed, EdgeCardinality, PathAlias};
use crate::procedures::gcard_query::degreepiecewise::PiecewiseConstantFunction;
use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::utils::{PathPattern, StarStatKey};

const MAGIC: &[u8; 4] = b"GCC1";
const MAX_COLLECTION_LEN: usize = 100_000_000;
const MAX_STRING_LEN: usize = 16 * 1024 * 1024;

struct StringDictionary {
    values: Vec<String>,
    ids: HashMap<String, u64>,
}

impl StringDictionary {
    fn from_catalog(catalog: &DegreeSeqGraphCompressed) -> Self {
        let mut values = BTreeSet::new();

        for (path, endpoints) in &catalog.edge_set_to_endpoints {
            values.extend(path.raw.iter().cloned());
            values.extend(endpoints.keys().cloned());
        }
        for (path, alias) in &catalog.path_aliases {
            values.extend(path.raw.iter().cloned());
            values.extend(alias.source.raw.iter().cloned());
            for (from, to) in &alias.endpoint_map {
                values.insert(from.clone());
                values.insert(to.clone());
            }
        }
        for star in catalog.star_stats.keys() {
            values.insert(star.center_label.clone());
            for arm in &star.arms {
                values.extend(arm.vs.iter().cloned());
                values.extend(arm.es.iter().cloned());
            }
        }
        values.extend(catalog.edge_cardinalities.keys().cloned());

        let values = values.into_iter().collect::<Vec<_>>();
        let ids = values
            .iter()
            .enumerate()
            .map(|(id, value)| (value.clone(), id as u64))
            .collect();
        Self { values, ids }
    }

    fn id(&self, value: &str) -> GCardResult<u64> {
        self.ids.get(value).copied().ok_or_else(|| {
            GCardError::InvalidState(format!("compact catalog dictionary misses '{value}'"))
        })
    }
}

impl DegreeSeqGraphCompressed {
    /// Persist the query-ready catalog in the compact, deterministic `GCC1` format.
    pub fn export_compact<P: AsRef<Path>>(&self, path: P) -> GCardResult<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        self.write_compact(&mut writer)?;
        writer.flush()?;
        Ok(())
    }

    /// Load a query-ready catalog written by [`Self::export_compact`].
    pub fn import_compact<P: AsRef<Path>>(path: P) -> GCardResult<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read_compact(&mut reader)
    }

    pub(crate) fn write_compact<W: Write>(&self, writer: &mut W) -> GCardResult<()> {
        writer.write_all(MAGIC)?;
        let dictionary = StringDictionary::from_catalog(self);
        write_varint(writer, dictionary.values.len() as u64)?;
        for value in &dictionary.values {
            write_bytes(writer, value.as_bytes())?;
        }

        let mut paths = self.edge_set_to_endpoints.iter().collect::<Vec<_>>();
        paths.sort_by(|(left, _), (right, _)| left.raw.cmp(&right.raw));
        write_varint(writer, paths.len() as u64)?;
        for (path, endpoints) in paths {
            write_path(writer, &dictionary, path)?;
            let mut endpoints = endpoints.iter().collect::<Vec<_>>();
            endpoints.sort_by(|(left, _), (right, _)| left.cmp(right));
            write_varint(writer, endpoints.len() as u64)?;
            for (endpoint, sequence) in endpoints {
                write_varint(writer, dictionary.id(endpoint)?)?;
                write_sequence(writer, sequence)?;
            }
        }

        let mut aliases = self.path_aliases.iter().collect::<Vec<_>>();
        aliases.sort_by(|(left, _), (right, _)| left.raw.cmp(&right.raw));
        write_varint(writer, aliases.len() as u64)?;
        for (path, alias) in aliases {
            write_path(writer, &dictionary, path)?;
            write_path(writer, &dictionary, &alias.source)?;
            let mut endpoints = alias.endpoint_map.iter().collect::<Vec<_>>();
            endpoints.sort();
            write_varint(writer, endpoints.len() as u64)?;
            for (from, to) in endpoints {
                write_varint(writer, dictionary.id(from)?)?;
                write_varint(writer, dictionary.id(to)?)?;
            }
        }

        let mut stars = self.star_stats.iter().collect::<Vec<_>>();
        stars.sort_by_key(|(star, _)| bincode::serialize(star).unwrap_or_default());
        write_varint(writer, stars.len() as u64)?;
        for (star, sequence) in stars {
            write_star(writer, &dictionary, star)?;
            write_sequence(writer, sequence)?;
        }

        let mut cardinalities = self.edge_cardinalities.iter().collect::<Vec<_>>();
        cardinalities.sort_by(|(left, _), (right, _)| left.cmp(right));
        write_varint(writer, cardinalities.len() as u64)?;
        for (label, cardinality) in cardinalities {
            write_varint(writer, dictionary.id(label)?)?;
            writer.write_all(&[match cardinality {
                EdgeCardinality::ManyToMany => 0,
                EdgeCardinality::ManyToOne => 1,
                EdgeCardinality::OneToMany => 2,
            }])?;
        }
        Ok(())
    }

    pub(crate) fn read_compact<R: Read>(reader: &mut R) -> GCardResult<Self> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(GCardError::InvalidData(
                "not a GCC1 compact GCard catalog".to_string(),
            ));
        }

        let dictionary_len = read_len(reader)?;
        let mut dictionary = Vec::with_capacity(dictionary_len);
        for _ in 0..dictionary_len {
            let bytes = read_bytes(reader, MAX_STRING_LEN)?;
            dictionary.push(String::from_utf8(bytes).map_err(|error| {
                GCardError::InvalidData(format!("invalid UTF-8 in compact dictionary: {error}"))
            })?);
        }

        let mut catalog = Self::new();
        for _ in 0..read_len(reader)? {
            let path = read_path(reader, &dictionary)?;
            let endpoint_len = read_len(reader)?;
            let mut endpoints = HashMap::with_capacity(endpoint_len);
            for _ in 0..endpoint_len {
                let endpoint = read_dictionary_value(reader, &dictionary)?;
                let sequence = read_sequence(reader)?;
                if endpoints.insert(endpoint, sequence).is_some() {
                    return Err(GCardError::InvalidData(
                        "duplicate endpoint in compact catalog".to_string(),
                    ));
                }
            }
            if catalog
                .edge_set_to_endpoints
                .insert(path, endpoints)
                .is_some()
            {
                return Err(GCardError::InvalidData(
                    "duplicate path in compact catalog".to_string(),
                ));
            }
        }

        for _ in 0..read_len(reader)? {
            let path = read_path(reader, &dictionary)?;
            let source = read_path(reader, &dictionary)?;
            let endpoint_len = read_len(reader)?;
            let mut endpoint_map = HashMap::with_capacity(endpoint_len);
            for _ in 0..endpoint_len {
                let from = read_dictionary_value(reader, &dictionary)?;
                let to = read_dictionary_value(reader, &dictionary)?;
                if endpoint_map.insert(from, to).is_some() {
                    return Err(GCardError::InvalidData(
                        "duplicate alias endpoint in compact catalog".to_string(),
                    ));
                }
            }
            if catalog
                .path_aliases
                .insert(
                    path,
                    PathAlias {
                        source,
                        endpoint_map,
                    },
                )
                .is_some()
            {
                return Err(GCardError::InvalidData(
                    "duplicate alias in compact catalog".to_string(),
                ));
            }
        }

        for _ in 0..read_len(reader)? {
            let star = read_star(reader, &dictionary)?;
            let sequence = read_sequence(reader)?;
            if catalog.star_stats.insert(star, sequence).is_some() {
                return Err(GCardError::InvalidData(
                    "duplicate star in compact catalog".to_string(),
                ));
            }
        }

        for _ in 0..read_len(reader)? {
            let label = read_dictionary_value(reader, &dictionary)?;
            let cardinality = match read_byte(reader)? {
                0 => EdgeCardinality::ManyToMany,
                1 => EdgeCardinality::ManyToOne,
                2 => EdgeCardinality::OneToMany,
                value => {
                    return Err(GCardError::InvalidData(format!(
                        "unknown edge-cardinality tag {value}"
                    )));
                }
            };
            if catalog
                .edge_cardinalities
                .insert(label, cardinality)
                .is_some()
            {
                return Err(GCardError::InvalidData(
                    "duplicate edge cardinality in compact catalog".to_string(),
                ));
            }
        }
        Ok(catalog)
    }
}

fn write_path<W: Write>(
    writer: &mut W,
    dictionary: &StringDictionary,
    path: &AltKey,
) -> GCardResult<()> {
    write_varint(writer, path.raw.len() as u64)?;
    for value in &path.raw {
        write_varint(writer, dictionary.id(value)?)?;
    }
    Ok(())
}

fn read_path<R: Read>(reader: &mut R, dictionary: &[String]) -> GCardResult<AltKey> {
    let len = read_len(reader)?;
    let mut raw = Vec::with_capacity(len);
    for _ in 0..len {
        raw.push(read_dictionary_value(reader, dictionary)?);
    }
    Ok(AltKey::new(raw))
}

fn write_star<W: Write>(
    writer: &mut W,
    dictionary: &StringDictionary,
    star: &StarStatKey,
) -> GCardResult<()> {
    write_varint(writer, dictionary.id(&star.center_label)?)?;
    write_varint(writer, star.arms.len() as u64)?;
    for arm in &star.arms {
        write_string_ids(writer, dictionary, &arm.vs)?;
        write_string_ids(writer, dictionary, &arm.es)?;
    }
    Ok(())
}

fn read_star<R: Read>(reader: &mut R, dictionary: &[String]) -> GCardResult<StarStatKey> {
    let center = read_dictionary_value(reader, dictionary)?;
    let arm_len = read_len(reader)?;
    let mut arms = Vec::with_capacity(arm_len);
    for _ in 0..arm_len {
        let vs = read_string_ids(reader, dictionary)?;
        let es = read_string_ids(reader, dictionary)?;
        if vs.len() != es.len() + 1 {
            return Err(GCardError::InvalidData(
                "invalid star arm in compact catalog".to_string(),
            ));
        }
        arms.push(PathPattern::new_without_reverse(vs, es));
    }
    Ok(StarStatKey::new(center, arms))
}

fn write_string_ids<W: Write>(
    writer: &mut W,
    dictionary: &StringDictionary,
    values: &[String],
) -> GCardResult<()> {
    write_varint(writer, values.len() as u64)?;
    for value in values {
        write_varint(writer, dictionary.id(value)?)?;
    }
    Ok(())
}

fn read_string_ids<R: Read>(reader: &mut R, dictionary: &[String]) -> GCardResult<Vec<String>> {
    let len = read_len(reader)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(read_dictionary_value(reader, dictionary)?);
    }
    Ok(values)
}

fn write_sequence<W: Write>(writer: &mut W, sequence: &CompressedDegreeSeq) -> GCardResult<()> {
    match sequence {
        CompressedDegreeSeq::SafeBound { function } => {
            writer.write_all(&[0])?;
            write_f64_vec(writer, &function.constants)?;
            write_f64_vec(writer, &function.right_interval_edges)?;
            write_f64_vec(writer, &function.cumulative_rows)?;
        }
        CompressedDegreeSeq::FastCompressor { len, base, counts } => {
            writer.write_all(&[1])?;
            write_varint(writer, *len as u64)?;
            write_varint(writer, *base)?;
            write_u64_vec(writer, counts)?;
        }
        CompressedDegreeSeq::BucketMax {
            counts,
            bucket_max_values,
        } => {
            if counts.len() != bucket_max_values.len() {
                return Err(GCardError::InvalidState(format!(
                    "bucket count/max length mismatch: {} != {}",
                    counts.len(),
                    bucket_max_values.len()
                )));
            }
            writer.write_all(&[2])?;
            write_u64_vec(writer, counts)?;
            // The dense vector index is the bucket id; persist no separate id.
            write_u64_vec(writer, bucket_max_values)?;
        }
    }
    Ok(())
}

fn read_sequence<R: Read>(reader: &mut R) -> GCardResult<CompressedDegreeSeq> {
    match read_byte(reader)? {
        0 => Ok(CompressedDegreeSeq::SafeBound {
            function: PiecewiseConstantFunction {
                constants: read_f64_vec(reader)?,
                right_interval_edges: read_f64_vec(reader)?,
                cumulative_rows: read_f64_vec(reader)?,
            },
        }),
        1 => Ok(CompressedDegreeSeq::FastCompressor {
            len: read_len(reader)?,
            base: read_varint(reader)?,
            counts: read_u64_vec(reader)?,
        }),
        2 => {
            let counts = read_u64_vec(reader)?;
            let bucket_max_values = read_u64_vec(reader)?;
            if counts.len() != bucket_max_values.len() {
                return Err(GCardError::InvalidData(format!(
                    "bucket count/max length mismatch: {} != {}",
                    counts.len(),
                    bucket_max_values.len()
                )));
            }
            Ok(CompressedDegreeSeq::BucketMax {
                counts,
                bucket_max_values,
            })
        }
        value => Err(GCardError::InvalidData(format!(
            "unknown compressed-sequence tag {value}"
        ))),
    }
}

fn write_u64_vec<W: Write>(writer: &mut W, values: &[u64]) -> GCardResult<()> {
    write_varint(writer, values.len() as u64)?;
    for value in values {
        write_varint(writer, *value)?;
    }
    Ok(())
}

fn read_u64_vec<R: Read>(reader: &mut R) -> GCardResult<Vec<u64>> {
    let len = read_len(reader)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(read_varint(reader)?);
    }
    Ok(values)
}

fn write_f64_vec<W: Write>(writer: &mut W, values: &[f64]) -> GCardResult<()> {
    write_varint(writer, values.len() as u64)?;
    for value in values {
        writer.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

fn read_f64_vec<R: Read>(reader: &mut R) -> GCardResult<Vec<f64>> {
    let len = read_len(reader)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        let mut bytes = [0u8; 8];
        reader.read_exact(&mut bytes)?;
        values.push(f64::from_le_bytes(bytes));
    }
    Ok(values)
}

fn write_bytes<W: Write>(writer: &mut W, value: &[u8]) -> GCardResult<()> {
    write_varint(writer, value.len() as u64)?;
    writer.write_all(value)?;
    Ok(())
}

fn read_bytes<R: Read>(reader: &mut R, max_len: usize) -> GCardResult<Vec<u8>> {
    let len = read_len(reader)?;
    if len > max_len {
        return Err(GCardError::InvalidData(format!(
            "compact value length {len} exceeds limit {max_len}"
        )));
    }
    let mut value = vec![0u8; len];
    reader.read_exact(&mut value)?;
    Ok(value)
}

fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> GCardResult<()> {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        writer.write_all(&[byte])?;
        if value == 0 {
            return Ok(());
        }
    }
}

fn read_varint<R: Read>(reader: &mut R) -> GCardResult<u64> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = read_byte(reader)?;
        if shift == 63 && byte > 1 {
            return Err(GCardError::InvalidData("varint overflow".to_string()));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(GCardError::InvalidData("varint overflow".to_string()))
}

fn read_len<R: Read>(reader: &mut R) -> GCardResult<usize> {
    let value = read_varint(reader)?;
    let len = usize::try_from(value)
        .map_err(|_| GCardError::InvalidData(format!("length does not fit usize: {value}")))?;
    if len > MAX_COLLECTION_LEN {
        return Err(GCardError::InvalidData(format!(
            "compact collection length {len} exceeds limit {MAX_COLLECTION_LEN}"
        )));
    }
    Ok(len)
}

fn read_byte<R: Read>(reader: &mut R) -> GCardResult<u8> {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn read_dictionary_value<R: Read>(reader: &mut R, values: &[String]) -> GCardResult<String> {
    let id = read_len(reader)?;
    values.get(id).cloned().ok_or_else(|| {
        GCardError::InvalidData(format!("compact dictionary id {id} is out of range"))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Cursor;

    use super::*;

    fn bucket_sequence() -> CompressedDegreeSeq {
        CompressedDegreeSeq::BucketMax {
            counts: vec![0, 3, 127, 128, 16_384],
            bucket_max_values: vec![0, 1, 127, 3000, 1 << 20],
        }
    }

    fn sample_catalog() -> DegreeSeqGraphCompressed {
        let path = AltKey::new(vec![
            "person".to_string(),
            "knows".to_string(),
            "person".to_string(),
        ]);
        let alias_path = AltKey::new(vec![
            "person".to_string(),
            "knows".to_string(),
            "person".to_string(),
            "likes".to_string(),
            "post".to_string(),
        ]);
        let star = StarStatKey::new(
            "person".to_string(),
            vec![PathPattern::new_without_reverse(
                vec!["person".to_string(), "post".to_string()],
                vec!["likes".to_string()],
            )],
        );
        let mut catalog = DegreeSeqGraphCompressed::new();
        catalog.edge_set_to_endpoints.insert(
            path.clone(),
            HashMap::from([("person".to_string(), bucket_sequence())]),
        );
        catalog.path_aliases.insert(
            alias_path,
            PathAlias {
                source: path,
                endpoint_map: HashMap::from([("post".to_string(), "person".to_string())]),
            },
        );
        catalog.star_stats.insert(star, bucket_sequence());
        catalog
            .edge_cardinalities
            .insert("knows".to_string(), EdgeCardinality::ManyToMany);
        catalog
    }

    #[test]
    fn varint_roundtrip_boundaries() {
        for value in [0, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut encoded = Vec::new();
            write_varint(&mut encoded, value).unwrap();
            assert_eq!(read_varint(&mut Cursor::new(encoded)).unwrap(), value);
        }
    }

    #[test]
    fn compact_catalog_is_deterministic_and_roundtrips() {
        let catalog = sample_catalog();
        let mut first = Vec::new();
        let mut second = Vec::new();
        catalog.write_compact(&mut first).unwrap();
        catalog.write_compact(&mut second).unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..4], MAGIC);

        let restored = DegreeSeqGraphCompressed::read_compact(&mut Cursor::new(first)).unwrap();
        assert_eq!(restored.edge_set_to_endpoints.len(), 1);
        assert_eq!(restored.path_aliases.len(), 1);
        assert_eq!(restored.star_stats.len(), 1);
        assert_eq!(restored.edge_cardinalities.len(), 1);

        let sequence = restored
            .edge_set_to_endpoints
            .values()
            .next()
            .unwrap()
            .get("person")
            .unwrap();
        match sequence {
            CompressedDegreeSeq::BucketMax {
                counts,
                bucket_max_values,
            } => {
                assert_eq!(counts, &[0, 3, 127, 128, 16_384]);
                assert_eq!(bucket_max_values, &[0, 1, 127, 3000, 1 << 20]);
            }
            _ => panic!("expected bucket-max sequence"),
        }
    }
}
