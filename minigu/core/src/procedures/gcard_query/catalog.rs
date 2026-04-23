use std::collections::HashMap;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::degreepiecewise::PiecewiseConstantFunction;
use crate::procedures::gcard_query::error::{GCardError, GCardResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltKey {
    pub raw: Vec<String>,
    normalized: Vec<String>,
}

impl fmt::Display for AltKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw.join(", "))
    }
}

impl AltKey {
    pub fn new(raw: Vec<String>) -> Self {
        // Canonical form: de-interleave into (vs, es), compare forward vs reverse,
        // pick the lexicographically smaller direction — same logic as PathPattern::new.
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
        function: PiecewiseConstantFunction,
    },
    FastCompressor {
        len: usize,
        base: u64,
        counts: Vec<u64>,
    },
}

impl CompressedDegreeSeq {
    /// Convert to a [`PiecewiseConstantFunction`] directly.
    ///
    /// For `SafeBound` this is a clone.  For `FastCompressor` the histogram
    /// bins are mapped straight to PCF segments (CDF model) without expanding
    /// into a full degree sequence — O(num_bins) instead of O(num_vertices).
    pub fn to_pcf(&self) -> PiecewiseConstantFunction {
        match self {
            CompressedDegreeSeq::SafeBound { function } => function.clone(),
            CompressedDegreeSeq::FastCompressor {
                len: _,
                base,
                counts,
            } => Self::fast_compressor_to_pcf(*base, counts),
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
        let total_rows: f64 = {
            let mut s = 0u128;
            for (i, &c) in counts.iter().enumerate() {
                if c == 0 {
                    continue;
                }
                s += base.pow(i as u32) as u128 * c as u128;
            }
            s as f64
        };

        if total_rows == 0.0 {
            return PiecewiseConstantFunction::empty();
        }

        let mut constants = Vec::new();
        let mut right_interval_edges = Vec::new();
        let mut cumulative_rows = Vec::new();

        let mut cum_row: f64 = 0.0;
        let mut cum_x: f64 = 0.0;

        // Descending order: highest bin first (largest degree values first).
        for (i, &c) in counts.iter().enumerate().rev() {
            if c == 0 {
                continue;
            }
            let degree_val = base.pow(i as u32) as f64;
            let rows_in_bin = degree_val * c as f64;

            // CDF model: width = rows / degree_val = c
            // But cap so cumulative doesn't exceed total_rows.
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DegreeSeqGraphCompressed {
    pub edge_set_to_endpoints: HashMap<AltKey, HashMap<String, CompressedDegreeSeq>>,
}

impl DegreeSeqGraphCompressed {
    pub fn new() -> Self {
        Self {
            edge_set_to_endpoints: HashMap::new(),
        }
    }

    pub fn get_piece_func_by_path(
        &self,
        path: &AltKey,
        target_node: &str,
    ) -> PiecewiseConstantFunction {
        if let Some(endpoints) = self.edge_set_to_endpoints.get(path) {
            endpoints
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(target_node))
                .map(|(_, v)| v.to_pcf())
                .expect("not found")
        } else {
            PiecewiseConstantFunction::empty()
        }
    }

    /// Incrementally rebuild only the dirty `(AltKey, label)` entries
    /// from the given `Statistic`, instead of full reconstruction.
    pub fn update_dirty(
        &mut self,
        statistic: &super::Statistic,
        dirty_keys: &std::collections::HashSet<(AltKey, String)>,
    ) {
        for (altkey, label) in dirty_keys {
            if let Some(block) = statistic
                .label_path_statistic
                .get(label)
                .and_then(|ls| ls.path_statistic.get(altkey))
            {
                if let Ok(Some(seq)) = block.get_compressed_degree_seq() {
                    self.edge_set_to_endpoints
                        .entry(altkey.clone())
                        .or_default()
                        .insert(label.clone(), seq);
                } else {
                    // Empty — remove entry.
                    if let Some(map) = self.edge_set_to_endpoints.get_mut(altkey) {
                        map.remove(label);
                    }
                }
            }
        }
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
