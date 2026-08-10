//! 若干跨模块共用的小工具：
//! 1. schema 里的边端点描述；
//! 2. 路径模式的规范化表示；
//! 3. 从 catalog/schema 中提取路径枚举需要的结构信息。

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

use minigu_catalog::provider::GraphTypeProvider;
use minigu_common::types::LabelId;
use serde::{Deserialize, Serialize};

use crate::procedures::gcard_query::catalog::{AltKey, EdgeCardinality};
use crate::procedures::gcard_query::make_alt_key;

/// Source and destination vertex label names for one edge type.
/// The edge label name itself is the key in `HashMap<String, EdgeEndpoints>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeEndpoints {
    /// 边类型允许的源点标签。
    pub src_label: String,
    /// 边类型允许的终点标签。
    pub dst_label: String,
    /// Schema-level edge cardinality, manually maintained like PathCE.
    pub cardinality: EdgeCardinality,
}

pub fn manual_edge_cardinality(edge_label: &str) -> EdgeCardinality {
    match edge_label.to_ascii_lowercase().as_str() {
        "city_ispartof_country"
        | "comment_hascreator_person"
        | "comment_islocatedin_country"
        | "comment_replyof_comment"
        | "comment_replyof_post"
        | "company_islocatedin_country"
        | "country_ispartof_continent"
        | "forum_hasmoderator_person"
        | "person_islocatedin_city"
        | "post_hascreator_person"
        | "post_islocatedin_country"
        | "tag_hastype_tagclass"
        | "tagclass_issubclassof_tagclass"
        | "university_islocatedin_city" => EdgeCardinality::ManyToOne,
        "forum_containerof_post" => EdgeCardinality::OneToMany,
        _ => EdgeCardinality::ManyToMany,
    }
}

#[derive(Debug, Deserialize)]
struct EdgeCardinalitySidecar {
    edges: HashMap<String, EdgeCardinalitySidecarEntry>,
}

#[derive(Debug, Deserialize)]
struct EdgeCardinalitySidecarEntry {
    src_label: String,
    dst_label: String,
    cardinality: EdgeCardinality,
}

pub fn edge_cardinality_name(cardinality: EdgeCardinality) -> &'static str {
    match cardinality {
        EdgeCardinality::ManyToMany => "ManyToMany",
        EdgeCardinality::ManyToOne => "ManyToOne",
        EdgeCardinality::OneToMany => "OneToMany",
    }
}

pub fn edge_cardinality_from_name(value: &str) -> Option<EdgeCardinality> {
    match value {
        "ManyToMany" => Some(EdgeCardinality::ManyToMany),
        "ManyToOne" => Some(EdgeCardinality::ManyToOne),
        "OneToMany" => Some(EdgeCardinality::OneToMany),
        _ => None,
    }
}

pub fn edge_cardinalities_to_names(
    edges: &HashMap<String, EdgeEndpoints>,
) -> HashMap<String, String> {
    edges
        .iter()
        .map(|(label, endpoint)| {
            (
                label.clone(),
                edge_cardinality_name(endpoint.cardinality).to_string(),
            )
        })
        .collect()
}

pub fn edge_cardinalities_from_names(
    values: &HashMap<String, String>,
) -> HashMap<String, EdgeCardinality> {
    values
        .iter()
        .filter_map(|(label, value)| {
            edge_cardinality_from_name(value).map(|cardinality| (label.clone(), cardinality))
        })
        .collect()
}

pub fn get_edges_from_catalog_with_cardinalities(
    catalog: &dyn GraphTypeProvider,
    overrides: &HashMap<String, EdgeCardinality>,
) -> Result<HashMap<String, EdgeEndpoints>, anyhow::Error> {
    let mut edges = get_edges_from_catalog(catalog)?;
    for (label, endpoint) in &mut edges {
        if let Some(cardinality) = overrides
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(label))
            .map(|(_, cardinality)| cardinality)
        {
            endpoint.cardinality = *cardinality;
        }
    }
    Ok(edges)
}

pub fn get_edges_from_catalog_with_dataset_cardinalities(
    catalog: &dyn GraphTypeProvider,
    dataset_dir: &std::path::Path,
) -> Result<HashMap<String, EdgeEndpoints>, anyhow::Error> {
    let path = dataset_dir.join("edge_cardinalities.json");
    if !path.is_file() {
        return get_edges_from_catalog(catalog);
    }

    let sidecar: EdgeCardinalitySidecar = serde_json::from_str(&std::fs::read_to_string(&path)?)
        .map_err(|e| anyhow::anyhow!("cannot parse {}: {}", path.display(), e))?;
    let mut edges = get_edges_from_catalog(catalog)?;

    for (label, endpoint) in &mut edges {
        let (described_label, described) = sidecar
            .edges
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(label))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} does not describe schema edge '{}'",
                    path.display(),
                    label
                )
            })?;
        if !described
            .src_label
            .eq_ignore_ascii_case(&endpoint.src_label)
            || !described
                .dst_label
                .eq_ignore_ascii_case(&endpoint.dst_label)
        {
            return Err(anyhow::anyhow!(
                "{} endpoint mismatch for '{}': expected {}->{}, got {}->{}",
                path.display(),
                described_label,
                endpoint.src_label,
                endpoint.dst_label,
                described.src_label,
                described.dst_label
            ));
        }
        endpoint.cardinality = described.cardinality;
    }
    for described_label in sidecar.edges.keys() {
        if !edges
            .keys()
            .any(|label| label.eq_ignore_ascii_case(described_label))
        {
            return Err(anyhow::anyhow!(
                "{} describes unknown schema edge '{}'",
                path.display(),
                described_label
            ));
        }
    }

    eprintln!(
        "loaded {} edge cardinalities from {}",
        edges.len(),
        path.display()
    );
    Ok(edges)
}

pub fn edge_cardinalities_from_schema(
    edges: &HashMap<String, EdgeEndpoints>,
) -> HashMap<String, EdgeCardinality> {
    edges
        .iter()
        .map(|(label, endpoints)| (label.clone(), endpoints.cardinality))
        .collect()
}

// ----- Path pattern (string-based, canonical form for path set equality) -----

/// Path pattern with node/edge label names. Canonical form: (vs, es) <= (reverse(vs), reverse(es)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathPattern {
    /// 顶点标签序列，长度永远比 `es` 大 1。
    pub vs: Vec<String>,
    /// 边标签序列。
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
        // 与 AltKey 一样，会自动把一条路径和它的逆路径规约到同一规范形式。
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
        // 常用于“长路径递推依赖短路径”时取后缀。
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StarStatKey {
    pub center_label: String,
    pub degree: usize,
    pub max_arm_len: usize,
    pub arms: Vec<PathPattern>,
}

impl StarStatKey {
    pub fn new(center_label: String, mut arms: Vec<PathPattern>) -> Self {
        arms.sort_by(|a, b| a.vs.cmp(&b.vs).then(a.es.cmp(&b.es)));
        let degree = arms.len();
        let max_arm_len = arms.iter().map(|p| p.es.len()).max().unwrap_or(0);
        StarStatKey {
            center_label,
            degree,
            max_arm_len,
            arms,
        }
    }
}

impl fmt::Display for StarStatKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arms = self
            .arms
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        write!(
            f,
            "star(center={}, degree={}, max_arm_len={}, arms=[{}])",
            self.center_label, self.degree, self.max_arm_len, arms
        )
    }
}

pub fn get_edges_from_catalog(
    catalog: &dyn GraphTypeProvider,
) -> Result<HashMap<String, EdgeEndpoints>, anyhow::Error> {
    // 从 catalog 的 edge type schema 中提取“边标签 -> 两端顶点标签”的纯净映射。
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
            edge_name.clone(),
            EdgeEndpoints {
                src_label,
                dst_label,
                cardinality: manual_edge_cardinality(&edge_name),
            },
        );
    }
    Ok(edges)
}

pub fn build_undirected_adj(
    edges: &HashMap<String, EdgeEndpoints>,
) -> HashMap<String, Vec<(String, String)>> {
    // schema 层路径枚举时不关心方向约束那么严格，
    // 所以这里把 edge type 视作可双向连接的邻接关系。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ldbc_edge_cardinality_is_schema_defined() {
        assert_eq!(
            manual_edge_cardinality("comment_replyof_comment"),
            EdgeCardinality::ManyToOne
        );
        assert_eq!(
            manual_edge_cardinality("forum_containerof_post"),
            EdgeCardinality::OneToMany
        );
        assert_eq!(
            manual_edge_cardinality("person_knows_person"),
            EdgeCardinality::ManyToMany
        );
    }
}
