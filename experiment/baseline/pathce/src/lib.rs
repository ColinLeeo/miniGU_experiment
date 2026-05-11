#![feature(
    impl_trait_in_assoc_type,
    float_minimum_maximum,
    int_roundings,
    slice_partition_dedup,
    iterator_try_reduce
)]

mod binning;
pub mod catalog;
pub mod catalog_builder;
pub mod common;
pub mod counter;
mod error;
pub mod estimate;
mod factorization;
pub mod graph;
pub mod pattern;
pub mod sample;
pub mod schema;
mod statistics;
#[cfg(test)]
mod test_utils;
