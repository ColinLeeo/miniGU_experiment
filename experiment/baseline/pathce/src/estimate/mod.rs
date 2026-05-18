mod catalog_pattern;
pub mod decompose;
pub mod join;

pub use catalog_pattern::CatalogPattern;
use catalog_pattern::{CatalogEdge, CatalogEdgeKind};
use decompose::heuristic::HeuristicDecomposer;
use decompose::PatternDecomposer;
use itertools::Itertools;

use crate::catalog::{Catalog, DuckCatalog};
use crate::common::{EdgeDirection, LabelId, TagId};
use crate::error::GCardResult;
use crate::pattern::GraphPattern;

pub struct CardinalityEstimator<'a> {
    catalog: &'a DuckCatalog,
    max_path_length: usize,
    max_star_length: usize,
    max_star_degree: usize,
    limit: usize,
    disable_star: bool,
    disable_prune: bool,
    disable_cyclic: bool,
}

impl<'a> CardinalityEstimator<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: &'a DuckCatalog,
        max_path_length: usize,
        max_star_length: usize,
        max_star_degree: usize,
        limit: usize,
        disable_star: bool,
        disable_prune: bool,
        disable_cyclic: bool,
    ) -> Self {
        Self {
            catalog,
            max_path_length,
            max_star_length,
            max_star_degree,
            limit,
            disable_star,
            disable_prune,
            disable_cyclic,
        }
    }

    pub fn estimate_with_order<P: GraphPattern>(
        &self,
        pattern: &P,
        order: Vec<TagId>,
    ) -> GCardResult<f64> {
        let decomposer = HeuristicDecomposer::new(
            self.catalog,
            self.max_path_length,
            self.max_star_length,
            self.max_star_degree,
            self.limit,
            self.disable_star,
            self.disable_prune,
            self.disable_cyclic,
        );
        let pattern = decomposer.decompose_with_pivots(pattern, &order);
        let next_table_id = self.catalog.next_table_id().get();
        let mut id_generator = next_table_id..;
        let card = join::estimate(pattern, self.catalog.conn(), &mut id_generator, Some(order))?;
        self.catalog
            .next_table_id()
            .set(id_generator.next().unwrap());
        Ok(card)
    }

    pub fn estimate<P: GraphPattern>(&self, pattern: &P) -> GCardResult<f64> {
        let decomposer = HeuristicDecomposer::new(
            self.catalog,
            self.max_path_length,
            self.max_star_length,
            self.max_star_degree,
            self.limit,
            self.disable_star,
            self.disable_prune,
            self.disable_cyclic,
        );
        let patterns = decomposer.decompose(pattern);
        assert!(!patterns.is_empty());
        let next_table_id = self.catalog.next_table_id().get();
        let mut id_generator = next_table_id..;
        let mut cards = Vec::with_capacity(patterns.len());
        for p in patterns.iter().cloned() {
            cards.push(join::estimate(
                p,
                self.catalog.conn(),
                &mut id_generator,
                None,
            )?);
        }
        self.catalog
            .next_table_id()
            .set(id_generator.next().unwrap());
        let (best_index, best_card) = cards
            .iter()
            .copied()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .unwrap();
        if std::env::var_os("PATHCE_PRINT_PLAN").is_some() {
            eprintln!(
                "[pathce-plan] candidates={} selected_index={} estimate={}",
                patterns.len(),
                best_index,
                best_card
            );
            eprintln!("[pathce-plan] candidate_cards={:?}", cards);
            self.print_catalog_pattern(&patterns[best_index], pattern);
        }
        Ok(best_card)
    }

    fn print_catalog_pattern<P: GraphPattern>(&self, pattern: &CatalogPattern, query: &P) {
        eprintln!("[pathce-plan] vertices:");
        for v in pattern.vertices() {
            eprintln!("[pathce-plan]   v{} label={}", v.tag_id(), v.label_id());
        }
        eprintln!("[pathce-plan] edges:");
        for e in pattern.edges() {
            let coverage = self.resolve_coverage(query, e);
            let coverage = if coverage.is_empty() {
                "<unresolved>".to_string()
            } else {
                coverage
                    .iter()
                    .map(|edges| {
                        edges
                            .iter()
                            .map(|edge_id| format!("e{}", edge_id))
                            .join(",")
                    })
                    .join(" | ")
            };
            match e.kind() {
                CatalogEdgeKind::Path { src, dst } => {
                    let catalog_path = self
                        .catalog
                        .get_path(e.label_id())
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "<missing path catalog>".to_string());
                    eprintln!(
                        "[pathce-plan]   ce{} Path label={} query=({}->{}) catalog={}",
                        e.tag_id(),
                        e.label_id(),
                        src,
                        dst,
                        catalog_path
                    );
                    eprintln!("[pathce-plan]     covers={}", coverage);
                }
                CatalogEdgeKind::Star { center } => {
                    let catalog_star = self
                        .catalog
                        .get_star(e.label_id())
                        .map(|p| format!("{:?}", p))
                        .unwrap_or_else(|| "<missing star catalog>".to_string());
                    eprintln!(
                        "[pathce-plan]   ce{} Star label={} center={} catalog={}",
                        e.tag_id(),
                        e.label_id(),
                        center,
                        catalog_star
                    );
                    eprintln!("[pathce-plan]     covers={}", coverage);
                }
                CatalogEdgeKind::General(vertices) => {
                    eprintln!(
                        "[pathce-plan]   ce{} General label={} vertices={:?}",
                        e.tag_id(),
                        e.label_id(),
                        vertices
                    );
                    eprintln!("[pathce-plan]     covers={}", coverage);
                }
            }
        }
    }

    fn resolve_coverage<P: GraphPattern>(&self, query: &P, edge: &CatalogEdge) -> Vec<Vec<TagId>> {
        match edge.kind() {
            CatalogEdgeKind::Path { src, dst } => {
                let Some(path) = self.catalog.get_path(edge.label_id()) else {
                    return vec![];
                };
                let start_label = path.start().label_id();
                let Some(src_vertex) = query.get_vertex(*src) else {
                    return vec![];
                };
                if src_vertex.label_id() != start_label {
                    return vec![];
                }
                let mut steps = Vec::new();
                let mut current = path.start().tag_id();
                for (path_edge, direction) in path.edges().iter().zip(path.directions()) {
                    let next = match direction {
                        EdgeDirection::Out => path_edge.dst(),
                        EdgeDirection::In => path_edge.src(),
                    };
                    let next_label = path.get_vertex(next).unwrap().label_id();
                    steps.push((*direction, path_edge.label_id(), next_label));
                    current = next;
                }
                debug_assert_eq!(path.end().tag_id(), current);
                let mut out = Vec::new();
                Self::match_path(query, *src, *dst, &steps, &mut Vec::new(), &mut out);
                out
            }
            CatalogEdgeKind::Star { center } => {
                let Some(star) = self.catalog.get_star(edge.label_id()) else {
                    return vec![];
                };
                let Some(center_vertex) = query.get_vertex(*center) else {
                    return vec![];
                };
                let mut out = Vec::new();
                for catalog_center in star
                    .vertices()
                    .iter()
                    .filter(|v| v.label_id() == center_vertex.label_id())
                {
                    let mut mapping = vec![(catalog_center.tag_id(), *center)];
                    Self::match_general(query, star, 0, &mut mapping, &mut Vec::new(), &mut out);
                }
                out.retain(|edges| edges.contains(&edge.tag_id()));
                out.sort();
                out.dedup();
                out
            }
            CatalogEdgeKind::General(_) => vec![],
        }
    }

    fn match_path<P: GraphPattern>(
        query: &P,
        current: TagId,
        dst: TagId,
        steps: &[(EdgeDirection, LabelId, LabelId)],
        chosen: &mut Vec<TagId>,
        out: &mut Vec<Vec<TagId>>,
    ) {
        if steps.is_empty() {
            if current == dst {
                out.push(chosen.clone());
            }
            return;
        }
        let (direction, edge_label, next_label) = steps[0];
        for query_edge in query.edges() {
            if chosen.contains(&query_edge.tag_id()) || query_edge.label_id() != edge_label {
                continue;
            }
            let next = match direction {
                EdgeDirection::Out if query_edge.src() == current => query_edge.dst(),
                EdgeDirection::In if query_edge.dst() == current => query_edge.src(),
                _ => continue,
            };
            if query
                .get_vertex(next)
                .map(|v| v.label_id() != next_label)
                .unwrap_or(true)
            {
                continue;
            }
            chosen.push(query_edge.tag_id());
            Self::match_path(query, next, dst, &steps[1..], chosen, out);
            chosen.pop();
        }
    }

    fn match_general<P: GraphPattern, S: GraphPattern>(
        query: &P,
        catalog: &S,
        edge_index: usize,
        mapping: &mut Vec<(TagId, TagId)>,
        chosen_edges: &mut Vec<TagId>,
        out: &mut Vec<Vec<TagId>>,
    ) {
        if edge_index == catalog.edges().len() {
            let mut edges = chosen_edges.clone();
            edges.sort_unstable();
            out.push(edges);
            return;
        }

        let catalog_edge = catalog.edges()[edge_index];
        let mut candidates = Vec::new();
        for query_edge in query.edges() {
            if chosen_edges.contains(&query_edge.tag_id())
                || query_edge.label_id() != catalog_edge.label_id()
            {
                continue;
            }
            candidates.push((catalog_edge.src(), query_edge.src()));
            candidates.push((catalog_edge.dst(), query_edge.dst()));
            if !Self::mapping_can_extend(catalog, query, mapping, &candidates) {
                candidates.clear();
                continue;
            }
            let added = Self::extend_mapping(mapping, &candidates);
            chosen_edges.push(query_edge.tag_id());
            Self::match_general(query, catalog, edge_index + 1, mapping, chosen_edges, out);
            chosen_edges.pop();
            for _ in 0..added {
                mapping.pop();
            }
            candidates.clear();
        }
    }

    fn mapping_can_extend<P: GraphPattern, Q: GraphPattern>(
        catalog: &P,
        query: &Q,
        mapping: &[(TagId, TagId)],
        candidates: &[(TagId, TagId)],
    ) -> bool {
        for (catalog_vertex, query_vertex) in candidates {
            if let Some(existing_query) = mapping
                .iter()
                .find_map(|(c, q)| (*c == *catalog_vertex).then_some(*q))
            {
                if existing_query != *query_vertex {
                    return false;
                }
            }
            if let Some(existing_catalog) = mapping
                .iter()
                .find_map(|(c, q)| (*q == *query_vertex).then_some(*c))
            {
                if existing_catalog != *catalog_vertex {
                    return false;
                }
            }
            let Some(catalog_label) = catalog.get_vertex(*catalog_vertex).map(|v| v.label_id())
            else {
                return false;
            };
            let Some(query_label) = query.get_vertex(*query_vertex).map(|v| v.label_id()) else {
                return false;
            };
            if catalog_label != query_label {
                return false;
            }
        }
        true
    }

    fn extend_mapping(mapping: &mut Vec<(TagId, TagId)>, candidates: &[(TagId, TagId)]) -> usize {
        let mut added = 0;
        for (catalog_vertex, query_vertex) in candidates {
            if mapping.iter().any(|(c, _)| *c == *catalog_vertex) {
                continue;
            }
            mapping.push((*catalog_vertex, *query_vertex));
            added += 1;
        }
        added
    }
}

pub struct CardinalityEstimatorManual<'a> {
    catalog: &'a DuckCatalog,
}

impl<'a> CardinalityEstimatorManual<'a> {
    pub fn new(catalog: &'a DuckCatalog) -> Self {
        Self { catalog }
    }

    pub fn estimate(&self, pattern: CatalogPattern) -> GCardResult<f64> {
        let next_table_id = self.catalog.next_table_id().get();
        let mut id_generator = next_table_id..;
        let card = join::estimate(pattern, self.catalog.conn(), &mut id_generator, None)?;
        self.catalog
            .next_table_id()
            .set(id_generator.next().unwrap());
        Ok(card)
    }
}
