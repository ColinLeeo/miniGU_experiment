//! GCard 的 PCF/分段函数工具集合。
//!
//! `function.rs` 放底层数值结构与操作，
//! `wrapper.rs` 放更贴近算法语义的 alpha/beta 接口，
//! `fast_compressor.rs` 放极简桶压缩逻辑。

pub mod fast_compressor;
pub mod function;
pub mod wrapper;

pub use fast_compressor::*;
pub use function::{PiecewiseConstantFunction, *};
pub use wrapper::*;
