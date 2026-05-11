//! Lightweight column/table statistics for selectivity estimation.
//!
//! Follows the Kuzu/DuckDB approach: per-column HyperLogLog (NDV) + min/max,
//! per-table row count.  Built during [`super::FlatGraphBuilder`] construction.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use minigu_common::value::ScalarValue;
use serde::{Deserialize, Serialize};

// ── HyperLogLog ──────────────────────────────────────────────────────────────

/// HyperLogLog sketch for cardinality (NDV) estimation.
///
/// Parameters match Kuzu/DuckDB: P=6, M=64 registers, 64 bytes total.
#[derive(Clone, Serialize, Deserialize)]
pub struct HyperLogLog {
    registers: Vec<u8>,
}

impl HyperLogLog {
    // 64
    const ALPHA: f64 = 0.721347520444481703680;
    const M: usize = 1 << Self::P;
    const P: u32 = 6;
    const Q: u32 = 64 - Self::P;

    // 1 / (2 ln(2))

    pub fn new() -> Self {
        Self {
            registers: vec![0u8; Self::M],
        }
    }

    /// Insert a pre-hashed element.
    pub fn insert_hash(&mut self, mut h: u64) {
        let i = (h as usize) & (Self::M - 1);
        h >>= Self::P;
        h |= 1u64 << Self::Q;
        let z = (h.trailing_zeros() + 1) as u8;
        if z > self.registers[i] {
            self.registers[i] = z;
        }
    }

    /// Insert a [`ScalarValue`] by hashing it.
    pub fn insert(&mut self, value: &ScalarValue) {
        let h = Self::hash_scalar(value);
        self.insert_hash(h);
    }

    /// Estimated number of distinct values.
    pub fn count(&self) -> usize {
        let mut c = [0u32; (Self::Q + 2) as usize];
        for &reg in &self.registers {
            c[reg as usize] += 1;
        }
        Self::estimate_cardinality(&c).max(0) as usize
    }

    /// Merge another HLL sketch into this one.
    pub fn merge(&mut self, other: &Self) {
        for i in 0..Self::M {
            if other.registers[i] > self.registers[i] {
                self.registers[i] = other.registers[i];
            }
        }
    }

    fn hash_scalar(value: &ScalarValue) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    // Redis-derived sigma function (Algorithm 6 from Ertl 2017).
    fn sigma(x: f64) -> f64 {
        if x == 1.0 {
            return f64::INFINITY;
        }
        let mut x = x;
        let mut y = 1.0f64;
        let mut z = x;
        loop {
            x *= x;
            let z_prime = z;
            z += x * y;
            y += y;
            if z_prime == z {
                break;
            }
        }
        z
    }

    // Redis-derived tau function.
    fn tau(x: f64) -> f64 {
        if x == 0.0 || x == 1.0 {
            return 0.0;
        }
        let mut x = x;
        let mut y = 1.0f64;
        let mut z = 1.0 - x;
        loop {
            x = x.sqrt();
            let z_prime = z;
            y *= 0.5;
            z -= (1.0 - x) * (1.0 - x) * y;
            if z_prime == z {
                break;
            }
        }
        z / 3.0
    }

    fn estimate_cardinality(c: &[u32]) -> i64 {
        let m = Self::M as f64;
        let q = Self::Q as usize;
        let mut z = m * Self::tau((m - c[q] as f64) / m);
        for k in (1..=q).rev() {
            z += c[k] as f64;
            z *= 0.5;
        }
        z += m * Self::sigma(c[0] as f64 / m);
        (Self::ALPHA * m * m / z).round() as i64
    }
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HyperLogLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HyperLogLog(ndv≈{})", self.count())
    }
}

// ── Column Statistics ────────────────────────────────────────────────────────

/// Per-column statistics: NDV (via HLL), min, max.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnStats {
    pub total_count: usize,
    pub null_count: usize,
    pub hll: HyperLogLog,
    pub min: Option<ScalarValue>,
    pub max: Option<ScalarValue>,
}

impl ColumnStats {
    pub fn new() -> Self {
        Self {
            total_count: 0,
            null_count: 0,
            hll: HyperLogLog::new(),
            min: None,
            max: None,
        }
    }

    /// Update statistics with a new value.
    pub fn observe(&mut self, value: &ScalarValue) {
        self.total_count += 1;
        if matches!(value, ScalarValue::Null) || is_option_null(value) {
            self.null_count += 1;
            return;
        }
        self.hll.insert(value);
        self.update_min_max(value);
    }

    /// Estimated number of distinct non-null values.
    pub fn ndv(&self) -> usize {
        self.hll.count()
    }

    /// Merge another ColumnStats into this one.
    pub fn merge(&mut self, other: &Self) {
        self.total_count += other.total_count;
        self.null_count += other.null_count;
        self.hll.merge(&other.hll);
        if let Some(ref other_min) = other.min {
            self.update_min_max(other_min);
        }
        if let Some(ref other_max) = other.max {
            self.update_min_max(other_max);
        }
    }

    fn update_min_max(&mut self, value: &ScalarValue) {
        match &self.min {
            None => {
                self.min = Some(value.clone());
                self.max = Some(value.clone());
            }
            Some(current_min) => {
                if cmp_scalar(value, current_min) == Some(std::cmp::Ordering::Less) {
                    self.min = Some(value.clone());
                }
                if let Some(ref current_max) = self.max {
                    if cmp_scalar(value, current_max) == Some(std::cmp::Ordering::Greater) {
                        self.max = Some(value.clone());
                    }
                }
            }
        }
    }
}

impl Default for ColumnStats {
    fn default() -> Self {
        Self::new()
    }
}

// ── Table Statistics ─────────────────────────────────────────────────────────

/// Per-table (vertex label or edge label) statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableStats {
    pub cardinality: usize,
    /// property_name → column stats.
    pub columns: HashMap<String, ColumnStats>,
}

impl TableStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one row.  `props` must align with `prop_names`.
    pub fn observe_row(&mut self, prop_names: &[String], props: &[ScalarValue]) {
        self.cardinality += 1;
        for (name, value) in prop_names.iter().zip(props.iter()) {
            self.columns.entry(name.clone()).or_default().observe(value);
        }
    }
}

// ── Aggregate container ──────────────────────────────────────────────────────

/// All table-level statistics in a FlatGraph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStats {
    /// vertex_label → table stats.
    pub vertex_stats: HashMap<String, TableStats>,
    /// edge_label → table stats.
    pub edge_stats: HashMap<String, TableStats>,
}

// ── ScalarValue comparison ───────────────────────────────────────────────────

/// Check if a ScalarValue is a None-variant (e.g., Int64(None)).
fn is_option_null(v: &ScalarValue) -> bool {
    use ScalarValue::*;
    matches!(
        v,
        Boolean(None)
            | Int8(None)
            | Int16(None)
            | Int32(None)
            | Int64(None)
            | UInt8(None)
            | UInt16(None)
            | UInt32(None)
            | UInt64(None)
            | Float32(None)
            | Float64(None)
            | String(None)
    )
}

/// Compare two [`ScalarValue`]s, returning `None` for incompatible types.
pub fn cmp_scalar(a: &ScalarValue, b: &ScalarValue) -> Option<std::cmp::Ordering> {
    use ScalarValue::*;
    match (a, b) {
        (Int8(Some(a)), Int8(Some(b))) => Some(a.cmp(b)),
        (Int16(Some(a)), Int16(Some(b))) => Some(a.cmp(b)),
        (Int32(Some(a)), Int32(Some(b))) => Some(a.cmp(b)),
        (Int64(Some(a)), Int64(Some(b))) => Some(a.cmp(b)),
        (UInt8(Some(a)), UInt8(Some(b))) => Some(a.cmp(b)),
        (UInt16(Some(a)), UInt16(Some(b))) => Some(a.cmp(b)),
        (UInt32(Some(a)), UInt32(Some(b))) => Some(a.cmp(b)),
        (UInt64(Some(a)), UInt64(Some(b))) => Some(a.cmp(b)),
        (Float32(Some(a)), Float32(Some(b))) => Some(a.cmp(b)),
        (Float64(Some(a)), Float64(Some(b))) => Some(a.cmp(b)),
        (String(Some(a)), String(Some(b))) => Some(a.cmp(b)),
        (Boolean(Some(a)), Boolean(Some(b))) => Some(a.cmp(b)),
        _ => None,
    }
}
