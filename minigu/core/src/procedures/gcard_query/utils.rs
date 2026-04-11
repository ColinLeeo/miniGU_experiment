use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

use minigu_catalog::provider::GraphTypeProvider;
use minigu_common::types::LabelId;
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::catalog::AltKey;
use crate::procedures::gcard_query::make_alt_key;

/// Source and destination vertex label names for one edge type.
/// The edge label name itself is the key in `HashMap<String, EdgeEndpoints>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeEndpoints {
    pub src_label: String,
    pub dst_label: String,
}

// ----- Path pattern (string-based, canonical form for path set equality) -----

/// Path pattern with node/edge label names. Canonical form: (vs, es) <= (reverse(vs), reverse(es)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathPattern {
    pub vs: Vec<String>,
    pub es: Vec<String>,
}

impl fmt::Display for PathPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.vs.is_empty() {
            return Ok(());
        }

        write!(f, "{}", self.vs[0])?;

        for (e, v) in self.es.iter().zip(self.vs.iter().skip(1)) {
            write!(f, " -{}-> {}", e, v)?;
        }

        Ok(())
    }
}

impl PathPattern {
    pub fn new(vs: Vec<String>, es: Vec<String>) -> PathPattern {
        assert_eq!(vs.len(), es.len() + 1);
        let mut rvs = vs.clone();
        rvs.reverse();
        let mut res = es.clone();
        res.reverse();
        if (&vs, &es) <= (&rvs, &res) {
            PathPattern { vs, es }
        } else {
            PathPattern { vs: rvs, es: res }
        }
    }

    pub fn new_without_reverse(vs: Vec<String>, es: Vec<String>) -> PathPattern {
        assert_eq!(vs.len(), es.len() + 1);
        PathPattern { vs, es }
    }

    pub fn to_alt_key(&self) -> AltKey {
        make_alt_key(&self.vs, &self.es)
    }

    /// Return a new pattern with reversed vertex and edge sequences.
    pub fn reversed(&self) -> Self {
        PathPattern {
            vs: self.vs.iter().cloned().rev().collect(),
            es: self.es.iter().cloned().rev().collect(),
        }
    }

    /// Return the suffix pattern (drop the first vertex and edge).
    pub fn suffix(&self) -> Self {
        PathPattern::new_without_reverse(self.vs[1..].to_vec(), self.es[1..].to_vec())
    }

    /// Return the canonical (lexicographically smaller direction) form of this pattern.
    /// Used as cache key so that forward and reverse of the same path map to the same entry.
    pub fn canonical(&self) -> Self {
        let mut vs = self.vs.clone();
        vs.reverse();
        let mut es = self.es.clone();
        es.reverse();
        if (&vs, &es) <= (&self.vs, &self.es) {
            PathPattern { vs, es }
        } else {
            PathPattern {
                vs: self.vs.clone(),
                es: self.es.clone(),
            }
        }
    }
}

impl PartialEq for PathPattern {
    fn eq(&self, other: &Self) -> bool {
        self.vs == other.vs && self.es == other.es
    }
}
impl Eq for PathPattern {}

impl Hash for PathPattern {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.vs.hash(state);
        self.es.hash(state);
    }
}

pub fn get_edges_from_catalog(
    catalog: &dyn GraphTypeProvider,
) -> Result<HashMap<String, EdgeEndpoints>, anyhow::Error> {
    let mut label_id_to_name: HashMap<LabelId, String> = HashMap::new();
    for name in catalog.label_names() {
        if let Ok(Some(id)) = catalog.get_label_id(&name) {
            label_id_to_name.insert(id, name);
        }
    }
    let mut edges = HashMap::new();
    for edge_type in catalog.edge_type_keys() {
        let edge_type_ref = catalog
            .get_edge_type(&edge_type)?
            .ok_or_else(|| anyhow::anyhow!("edge type not found"))?;
        let src_id = edge_type_ref
            .src()
            .label_set()
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty src label set"))?;
        let dst_id = edge_type_ref
            .dst()
            .label_set()
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty dst label set"))?;
        let edge_label_id = edge_type
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty edge label set"))?;
        let edge_name = label_id_to_name
            .get(&edge_label_id)
            .cloned()
            .unwrap_or_else(|| format!("Unknown_{}", edge_label_id));
        let src_label = label_id_to_name
            .get(&src_id)
            .cloned()
            .unwrap_or_else(|| format!("Unknown_{}", src_id));
        let dst_label = label_id_to_name
            .get(&dst_id)
            .cloned()
            .unwrap_or_else(|| format!("Unknown_{}", dst_id));
        edges.insert(
            edge_name,
            EdgeEndpoints {
                src_label,
                dst_label,
            },
        );
    }
    Ok(edges)
}

pub fn build_undirected_adj(
    edges: &HashMap<String, EdgeEndpoints>,
) -> HashMap<String, Vec<(String, String)>> {
    let mut adj: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (edge_name, e) in edges {
        adj.entry(e.src_label.clone())
            .or_default()
            .push((edge_name.clone(), e.dst_label.clone()));
        adj.entry(e.dst_label.clone())
            .or_default()
            .push((edge_name.clone(), e.src_label.clone()));
    }
    adj
}
