//! `compact_gcard_log` procedure — flush the pending GCard update log for the current graph.
//!
//! No parameters.  Operates on `context.current_graph`.
//!
//! Returns `(pending_entries: Int64)` — the number of log entries that were compacted.

use std::io;
use std::sync::Arc;

use arrow::array::Int64Array;
use minigu_common::data_chunk::DataChunk;
use minigu_common::data_type::{DataField, DataSchema, LogicalType};
use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_execution::error::ExecutionError;

use crate::procedures::gcard_query::Statistic;
use crate::procedures::gcard_query::flat_graph::FlatGraph;
use crate::procedures::gcard_query::update_log::GCardUpdateLog;

// ── GCard compact + apply ─────────────────────────────────────────────────────

/// Expand all pending update-log entries via graph traversal, apply the net degree
/// deltas to the stored [`Statistic`], remove deleted vertices, and clear the log.
///
/// This is the full "compact" described in 更新算法.md:
/// 1. Expand `res_len > 0` entries by walking actual graph neighbours.
/// 2. Accumulate net per-vertex deltas (skipping deleted vertices).
/// 3. Apply deltas to [`Statistic`] and remove deleted vertices.
/// 4. Regenerate [`DegreeSeqGraphCompressed`] from the updated statistics.
/// 5. Clear the log.
///
/// Returns an error if the container has no update log / statistic (run `GCard_build`
/// first) or if a storage operation fails.
pub fn compact_gcard_update_log(container: &GraphContainer) -> anyhow::Result<()> {
    // 这个函数是“增量更新真正落盘/落内存”的收口点。
    // random_update / insert 等过程只是往日志里记；
    // 真正改 Statistic 和 catalog，是在这里做。

    // ── Resolve FlatGraph ─────────────────────────────────────────────────
    let flat_graph_arc = container
        .gcard_flat_graph()
        .and_then(|arc| std::sync::Arc::downcast::<FlatGraph>(arc).ok())
        .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded (run load_ldbc first)"))?;

    // ── Get update log ────────────────────────────────────────────────────
    let log_arc = container
        .gcard_update_log()
        .ok_or_else(|| anyhow::anyhow!("gcard_update_log not set (run GCard_build first)"))?;
    let log_arc = log_arc
        .downcast::<std::sync::Mutex<GCardUpdateLog>>()
        .map_err(|_| anyhow::anyhow!("gcard_update_log type mismatch"))?;

    // Statistic 当前是通过 Arc 挂在容器里的，
    // 所以这里先 clone 一份做就地修改，最后再整体替换回去。
    let stat_arc = container
        .statistic()
        .ok_or_else(|| anyhow::anyhow!("statistic not set (run GCard_build first)"))?;
    let mut statistic = stat_arc
        .downcast_ref::<Statistic>()
        .ok_or_else(|| anyhow::anyhow!("statistic type mismatch"))?
        .clone();

    // ── Run compact_and_apply_flat ────────────────────────────────────────
    let dirty_keys = {
        let mut guard = log_arc
            .lock()
            .map_err(|_| anyhow::anyhow!("gcard_update_log mutex poisoned"))?;
        guard.compact_and_apply_flat(&flat_graph_arc, &mut statistic)?
    };

    // 只重建 dirty key，避免把整个 compressed catalog 全量刷新一遍。
    let dsgc_arc = container
        .degree_seq_graph_compressed()
        .ok_or_else(|| anyhow::anyhow!("degree_seq_graph_compressed not set"))?;
    let mut new_dsgc = dsgc_arc
        .downcast_ref::<super::catalog::DegreeSeqGraphCompressed>()
        .ok_or_else(|| anyhow::anyhow!("dsgc type mismatch"))?
        .clone();
    new_dsgc.update_dirty(&statistic, &dirty_keys);
    container.set_statistic(Arc::new(statistic));
    container.set_degree_seq_graph_compressed(Arc::new(new_dsgc));

    Ok(())
}

pub fn build_procedure() -> Procedure {
    let schema = Arc::new(DataSchema::new(vec![DataField::new(
        "pending_entries".into(),
        LogicalType::Int64,
        false,
    )]));

    Procedure::new(vec![], Some(schema), move |context, _args| {
        let graph_ref = context.current_graph.clone().ok_or_else(|| {
            ExecutionError::Custom(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "no current graph selected",
            )))
        })?;

        let container = graph_ref
            .object()
            .downcast_ref::<GraphContainer>()
            .ok_or_else(|| {
                ExecutionError::Custom(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "current graph is not a GraphContainer",
                )))
            })?;

        // Count pending entries before compaction.
        let pending = container
            .gcard_update_log()
            .and_then(|any| any.downcast::<std::sync::Mutex<GCardUpdateLog>>().ok())
            .map(|log| {
                log.lock()
                    .expect("GCardUpdateLog mutex poisoned")
                    .entries
                    .len()
            })
            .unwrap_or(0);

        compact_gcard_update_log(container)?;

        let chunk = DataChunk::new(vec![Arc::new(Int64Array::from_iter_values([
            pending as i64
        ]))]);
        Ok(vec![chunk])
    })
}
