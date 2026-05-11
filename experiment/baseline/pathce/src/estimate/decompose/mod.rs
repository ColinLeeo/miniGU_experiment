pub mod heuristic;

use super::catalog_pattern::CatalogPattern;
use crate::pattern::GraphPattern;

pub trait PatternDecomposer {
    fn decompose<P: GraphPattern>(self, pattern: &P) -> Vec<CatalogPattern>;
}
