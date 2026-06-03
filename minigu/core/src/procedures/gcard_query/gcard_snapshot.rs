//! `gcard_snapshot` / `gcard_restore` procedures — capture and restore the
//! in-memory GCard graph state without reloading from disk.
//!
//! Usage:
//!   `call gcard_snapshot()`   — capture the current FlatGraph + Statistic +
//!                               DegreeSeqGraphCompressed into a side slot
//!   `call gcard_restore()`    — write the captured state back, discarding any
//!                               mutations made since the snapshot
//!
//! Intended for update experiments that need to run several independent updates
//! from the same original graph: snapshot once, then `gcard_restore` before each
//! `random_insert` to reset — avoiding a slow reload from the database files.
//!
//! The three structures are held as `Arc`s; the snapshot only clones the `Arc`
//! (O(1)).  `random_insert` mutates via `Arc::try_unwrap`, which falls back to a
//! deep clone while the snapshot keeps a reference — so the snapshot stays intact.
//! The update log is not snapshotted: `compact_and_apply_flat` drains it on every
//! `random_insert`, so it is already empty at the start of each round.

use minigu_context::graph::GraphContainer;
use minigu_context::procedure::Procedure;
use minigu_context::session::SessionContext;

fn current_container(context: &SessionContext) -> anyhow::Result<&GraphContainer> {
    let graph_ref = context
        .current_graph
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no current graph selected"))?;
    graph_ref
        .object()
        .downcast_ref::<GraphContainer>()
        .ok_or_else(|| anyhow::anyhow!("current graph is not a GraphContainer"))
}

pub fn build_snapshot_procedure() -> Procedure {
    Procedure::new(vec![], None, move |context, _args| {
        let container = current_container(&context)?;

        let flat_graph = container
            .gcard_flat_graph()
            .ok_or_else(|| anyhow::anyhow!("FlatGraph not loaded (run load_ldbc first)"))?;
        let statistic = container
            .statistic()
            .ok_or_else(|| anyhow::anyhow!("statistic not set (run load_ldbc first)"))?;
        let dsgc = container.degree_seq_graph_compressed().ok_or_else(|| {
            anyhow::anyhow!("degree_seq_graph_compressed not set (run load_ldbc first)")
        })?;

        container.set_gcard_snapshot(flat_graph, statistic, dsgc);
        println!("gcard_snapshot: captured FlatGraph + Statistic + DegreeSeqGraphCompressed");

        Ok(vec![])
    })
}

pub fn build_restore_procedure() -> Procedure {
    Procedure::new(vec![], None, move |context, _args| {
        let container = current_container(&context)?;

        let (flat_graph, statistic, dsgc) = container
            .gcard_snapshot()
            .ok_or_else(|| anyhow::anyhow!("no snapshot captured (run gcard_snapshot first)"))?;

        container.set_gcard_flat_graph(flat_graph);
        container.set_statistic(statistic);
        container.set_degree_seq_graph_compressed(dsgc);
        println!("gcard_restore: restored graph state from snapshot");

        Ok(vec![])
    })
}
