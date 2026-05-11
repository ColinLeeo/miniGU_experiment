mod catalog_pattern;
pub mod decompose;
pub mod join;

pub use catalog_pattern::CatalogPattern;
use decompose::heuristic::HeuristicDecomposer;
use decompose::PatternDecomposer;
use itertools::Itertools;

use crate::catalog::DuckCatalog;
use crate::common::TagId;
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
        let cards: Vec<_> = patterns
            .into_iter()
            .map(|p| join::estimate(p, self.catalog.conn(), &mut id_generator, None))
            .try_collect()?;
        self.catalog
            .next_table_id()
            .set(id_generator.next().unwrap());
        Ok(cards.into_iter().min_by(|a, b| a.total_cmp(b)).unwrap())
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
