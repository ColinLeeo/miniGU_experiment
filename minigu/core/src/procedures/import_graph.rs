//! Graph import/export utilities for `MemoryGraph`
//! # File layout produced by `export_graph`
//!
//! ```text
//! <output‑dir>/
//! ├── person.csv        #  vertex records labelled "person"
//! ├── friend.csv        #  edge records labelled "friend"
//! ├── follow.csv        #  edge records labelled "follow"
//! └── manifest.json       #  manifest generated from `Manifest`
//! ```
//!
//! Each vertex CSV row encodes
//!
//! ```csv
//! <vid>,<prop‑1>,<prop‑2>, ...
//! ```
//!
//! while edges are encoded as
//!
//! ```csv
//! <eid>,<src‑vid>,<dst‑vid>,<prop‑1>,<prop‑2>, ...
//! ```
//!
//! call import_graph(<graph_name>, <manifest_path>);
//!
//! Import a graph from CSV files plus a JSON `manifest.json` on disk into an in-memory graph,
//! then register it in the current schema under `<graph_name>`.
//!
//! ## Inputs
//! * `<graph_name>` – Name to register the imported graph under in the current schema.
//! * `<manifest_path>` – `manifest.json` path.
//!
//! ## Output
//! * Returns nothing. On success the graph is added to the current schema. Errors (missing files,
//!   schema mismatch, duplicate graph name, etc.) are surfaced via `Result`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use csv::ReaderBuilder;
use dashmap::DashMap as ConcurrentMap;
use minigu_catalog::label_set::LabelSet;
use minigu_catalog::memory::graph_type::{
    MemoryEdgeTypeCatalog, MemoryGraphTypeCatalog, MemoryVertexTypeCatalog,
};
use minigu_catalog::property::Property;
use minigu_catalog::provider::{GraphTypeProvider, SchemaProvider};
use minigu_common::data_type::{DataSchema, LogicalType};
use minigu_common::error::not_implemented;
use minigu_common::types::{LabelId, VertexId};
use minigu_common::value::ScalarValue;
use minigu_context::graph::{GraphContainer, GraphStorage};
use minigu_context::procedure::Procedure;
use minigu_context::session::SessionContext;
use minigu_storage::common::{Edge, PropertyRecord, Vertex};
use minigu_storage::tp::MemoryGraph;
use minigu_transaction::GraphTxnManager;
use rayon::prelude::*;

use super::common::{EdgeSpec, FileSpec, Manifest, RecordType, Result, VertexSpec};

// ============================================================================
// Import-specific implementation
// ============================================================================

fn build_manifest<P: AsRef<Path>>(manifest_path: P) -> Result<Manifest> {
    let data = std::fs::read(manifest_path)?;

    let data_str = std::str::from_utf8(&data)?;
    Manifest::from_str(data_str)
}

/// Convert a *string* coming from CSV into an owned [`ScalarValue`] according
/// to a given property definition.
pub(crate) fn property_to_scalar_value(property: &Property, value: &str) -> Result<ScalarValue> {
    if value.is_empty() {
        if property.nullable() {
            return match property.logical_type() {
                LogicalType::Int8 => Ok(ScalarValue::Int8(None)),
                LogicalType::Int16 => Ok(ScalarValue::Int16(None)),
                LogicalType::Int32 => Ok(ScalarValue::Int32(None)),
                LogicalType::Int64 => Ok(ScalarValue::Int64(None)),
                LogicalType::UInt8 => Ok(ScalarValue::UInt8(None)),
                LogicalType::UInt16 => Ok(ScalarValue::UInt16(None)),
                LogicalType::UInt32 => Ok(ScalarValue::UInt32(None)),
                LogicalType::UInt64 => Ok(ScalarValue::UInt64(None)),
                LogicalType::Boolean => Ok(ScalarValue::Boolean(None)),
                LogicalType::Float32 => Ok(ScalarValue::Float32(None)),
                LogicalType::Float64 => Ok(ScalarValue::Float64(None)),
                LogicalType::String => Ok(ScalarValue::String(None)),
                LogicalType::Null => Ok(ScalarValue::Null),
                _ => not_implemented("", None),
            };
        } else {
            return Err(anyhow::anyhow!(
                "Cannot parse empty string for non-nullable property '{}' of type {:?}",
                property.name(),
                property.logical_type()
            )
            .into());
        }
    }

    match property.logical_type() {
        LogicalType::Int8 => Ok(ScalarValue::Int8(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Int8 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Int16 => Ok(ScalarValue::Int16(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Int16 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Int32 => Ok(ScalarValue::Int32(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Int32 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Int64 => Ok(ScalarValue::Int64(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Int64 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::UInt8 => Ok(ScalarValue::UInt8(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse UInt8 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::UInt16 => Ok(ScalarValue::UInt16(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse UInt16 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::UInt32 => Ok(ScalarValue::UInt32(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse UInt32 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::UInt64 => Ok(ScalarValue::UInt64(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse UInt64 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Boolean => Ok(ScalarValue::Boolean(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Boolean from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Float32 => Ok(ScalarValue::Float32(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Float32 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::Float64 => Ok(ScalarValue::Float64(Some(value.parse().map_err(|e| {
            anyhow::anyhow!(
                "Cannot parse Float64 from '{}' for property '{}': {}",
                value,
                property.name(),
                e
            )
        })?))),
        LogicalType::String => Ok(ScalarValue::String(Some(value.to_string()))),
        LogicalType::Null => Err(anyhow::anyhow!("str isn't empty").into()),
        _ => not_implemented("", None),
    }
}

fn build_properties<'a>(
    props_schema: Vec<(u32, Property)>,
    record_iter: impl Iterator<Item = &'a str>,
) -> Result<Vec<ScalarValue>> {
    let mut props = Vec::with_capacity(props_schema.len());

    for ((_, property), value) in props_schema.iter().zip(record_iter) {
        props.push(property_to_scalar_value(property, value)?);
    }

    Ok(props)
}

pub fn import<P: AsRef<Path>>(
    context: SessionContext,
    graph_name: impl Into<String>,
    manifest_path: P,
) -> Result<()> {
    let graph_name = graph_name.into();
    let schema = context
        .current_schema
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("current schema not set"))?;

    if schema.get_graph(&graph_name)?.is_some() {
        return Err(anyhow::anyhow!("graph {graph_name} already exists").into());
    }

    let db_path = context.database().config().db_path.clone();
    let (graph, graph_type) =
        import_internal(manifest_path.as_ref(), db_path.as_deref(), &graph_name)?;

    let container = GraphContainer::new(
        Arc::clone(&graph_type),
        GraphStorage::Memory(Arc::clone(&graph)),
    );

    if !schema.add_graph(graph_name.clone(), Arc::new(container)) {
        return Err(anyhow::anyhow!("graph {graph_name} already exists").into());
    }

    // Persist catalog if using on-disk database
    if let Some(ref db_path) = db_path {
        crate::catalog_persistence::save_catalog(db_path, schema)?;
    }

    Ok(())
}

/// Fast line count using byte-level newline scanning (avoids full CSV parsing overhead).
fn count_csv_lines(path: &Path) -> usize {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut buf = [0u8; 64 * 1024];
    let mut count = 0usize;
    loop {
        let bytes_read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        count += buf[..bytes_read].iter().filter(|&&b| b == b'\n').count();
    }
    // Subtract 1 for header row; guard against empty files
    count.saturating_sub(1)
}

pub(crate) fn import_internal<P: AsRef<Path>>(
    manifest_path: P,
    db_path: Option<&Path>,
    graph_name: &str,
) -> Result<(Arc<MemoryGraph>, Arc<MemoryGraphTypeCatalog>)> {
    // Graph type
    let manifest = build_manifest(&manifest_path)?;
    let graph_type = get_graph_type_from_manifest(&manifest)?;

    let manifest_parent_dir = manifest_path.as_ref().parent().ok_or_else(|| {
        anyhow::anyhow!(
            "manifest path has no parent directory: {}",
            manifest_path.as_ref().display()
        )
    })?;

    // ========================================================================
    // Phase 0: Estimate record counts for capacity pre-allocation
    // ========================================================================
    let (vertex_count, edge_count) = {
        let vc: usize = manifest
            .vertices
            .par_iter()
            .map(|vs| count_csv_lines(&manifest_parent_dir.join(&vs.file.path)))
            .sum();
        let ec: usize = manifest
            .edges
            .par_iter()
            .map(|es| count_csv_lines(&manifest_parent_dir.join(&es.file.path)))
            .sum();
        (vc, ec)
    };
    eprintln!(
        "Import: estimated {} vertices, {} edges. Pre-allocating storage...",
        vertex_count, edge_count
    );

    // Graph - use file-backed storage when db_path is set, with capacity hints
    let graph = if let Some(db_path) = db_path {
        let data_path = crate::catalog_persistence::graph_data_path(db_path, graph_name);
        MemoryGraph::with_db_file(&data_path)?
    } else {
        MemoryGraph::in_memory_with_capacity(vertex_count, edge_count)
    };

    // ========================================================================
    // Phase 1: Parse vertex CSV files in parallel, insert directly (no buffering)
    // ========================================================================
    let vid_counter = AtomicU64::new(1);
    let vid_mapping: ConcurrentMap<(LabelId, VertexId), VertexId> =
        ConcurrentMap::with_capacity(vertex_count);
    manifest
        .vertices
        .par_iter()
        .try_for_each(|vertex_spec| -> Result<()> {
            let path = manifest_parent_dir.join(&vertex_spec.file.path);
            let label_id = graph_type
                .get_label_id(&vertex_spec.label.to_lowercase().as_str())?
                .expect("label id not found");
            let label_set = LabelSet::from_iter(vec![label_id]);
            let props_schema = graph_type
                .get_vertex_type(&label_set)?
                .expect("vertex type not found")
                .properties();

            let mut rdr = ReaderBuilder::new()
                .has_headers(true)
                .delimiter(b',')
                .from_path(&path)?;

            for record in rdr.records() {
                let record = record?;
                assert_eq!(props_schema.len() + 1, record.len());
                let old_vid_str = record.get(0).expect("record too short");
                if old_vid_str.is_empty() {
                    eprintln!(
                        "Warning: Empty vertex ID in vertex '{}'. Skipping record.",
                        vertex_spec.label
                    );
                    continue;
                }
                let old_vid: VertexId = match old_vid_str.parse() {
                    Ok(vid) => vid,
                    Err(e) => {
                        eprintln!(
                            "Warning: Cannot parse vertex ID '{}' in vertex '{}': {}. Skipping record.",
                            old_vid_str, vertex_spec.label, e
                        );
                        continue;
                    }
                };

                let props = match build_properties(props_schema.clone(), record.iter().skip(1)) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to build properties for vertex '{}' with ID '{}': {}. Skipping record.",
                            vertex_spec.label, old_vid, e
                        );
                        continue;
                    }
                };

                let vid = vid_counter.fetch_add(1, Ordering::Relaxed);
                let vertex = Vertex::new(vid, label_id, PropertyRecord::new(props));
                graph.bulk_insert_vertex(vertex);
                if vid_mapping.insert((label_id, old_vid), vid).is_some() {
                    eprintln!(
                        "Warning: Duplicate vertex ID {} for label {:?}.",
                        old_vid, label_id
                    );
                }
            }
            Ok(())
        })?;

    // ========================================================================
    // Phase 2: Parse edge CSV files in parallel, insert directly (no buffering)
    // ========================================================================
    let eid_counter = AtomicU64::new(1);
    // Scope vid_mapping so it is dropped immediately after edge import,
    // freeing ~48+ bytes per vertex of DashMap overhead before checkpoint.
    {
        let vid_mapping = &vid_mapping;
        manifest
        .edges
        .par_iter()
        .try_for_each(|edge_spec| -> Result<()> {
            let path = manifest_parent_dir.join(&edge_spec.file.path);
            let label_id = graph_type
                .get_label_id(&edge_spec.label.to_lowercase().as_str())?
                .expect("label id not found");
            let label_set = LabelSet::from_iter(vec![label_id]);
            let edge_props_schema = graph_type
                .get_edge_type(&label_set)?
                .expect("edge type not found")
                .properties();
            let src_label_id = graph_type
                .get_label_id(&edge_spec.src_label.to_lowercase().as_str())?
                .expect("label id not found");
            let dst_label_id = graph_type
                .get_label_id(&edge_spec.dst_label.to_lowercase().as_str())?
                .expect("label id not found");

            let mut rdr = ReaderBuilder::new()
                .has_headers(true)
                .delimiter(b',')
                .from_path(&path)?;

            for record in rdr.records() {
                let record = record?;
                if record.len() < 2 {
                    eprintln!(
                        "Warning: Edge record too short in edge '{}'. Expected at least 2 columns, got {}. Skipping record.",
                        edge_spec.label, record.len()
                    );
                    continue;
                }
                assert_eq!(record.len() - 2, edge_props_schema.len());

                let old_src_id_str = record.get(0).unwrap();
                let old_dst_id_str = record.get(1).unwrap();

                if old_src_id_str.is_empty() || old_dst_id_str.is_empty() {
                    eprintln!(
                        "Warning: Empty source or destination ID in edge '{}'. Skipping record.",
                        edge_spec.label
                    );
                    continue;
                }

                let old_src_id: VertexId = match old_src_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!(
                            "Warning: Cannot parse source ID '{}' in edge '{}': {}. Skipping record.",
                            old_src_id_str, edge_spec.label, e
                        );
                        continue;
                    }
                };

                let old_dst_id: VertexId = match old_dst_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!(
                            "Warning: Cannot parse destination ID '{}' in edge '{}': {}. Skipping record.",
                            old_dst_id_str, edge_spec.label, e
                        );
                        continue;
                    }
                };

                let src_id = match vid_mapping
                    .get(&(src_label_id, old_src_id))
                {
                    Some(id) => *id,
                    None => {
                        eprintln!(
                            "Warning: Source vertex ID {} not found in mapping. Skipping record.",
                            old_src_id
                        );
                        continue;
                    }
                };

                let dst_id = match vid_mapping
                    .get(&(dst_label_id, old_dst_id))
                {
                    Some(id) => *id,
                    None => {
                        eprintln!(
                            "Warning: Destination vertex ID {} not found in mapping. Skipping record.",
                            old_dst_id
                        );
                        continue;
                    }
                };

                let props =
                    match build_properties(edge_props_schema.clone(), record.iter().skip(2)) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to build properties for edge '{}' (src: {}, dst: {}): {}. Skipping record.",
                                edge_spec.label, old_src_id, old_dst_id, e
                            );
                            continue;
                        }
                    };

                let eid = eid_counter.fetch_add(1, Ordering::Relaxed);
                let edge = Edge::new(
                    eid,
                    src_id,
                    dst_id,
                    label_id,
                    PropertyRecord::new(props),
                );
                graph.bulk_insert_edge(edge);
            }
            Ok(())
        })?;
    } // vid_mapping dropped here, freeing mapping memory before checkpoint
    drop(vid_mapping);

    // Persist by writing a snapshot directly (bypasses WAL entirely).
    // No transaction was used, so no WAL/undo/redo overhead.
    if db_path.is_some() {
        graph.create_checkpoint()?;
    }

    Ok((graph, graph_type))
}

pub(crate) fn get_graph_type_from_manifest(
    manifest: &Manifest,
) -> Result<Arc<MemoryGraphTypeCatalog>> {
    let mut graph_type = MemoryGraphTypeCatalog::new();
    let mut label_vertex_type = HashMap::new();

    // Vertex
    for vs in manifest.vertices_spec().iter() {
        let label = vs.label_name();
        let label_id = graph_type
            .add_label(label.clone())
            .expect("add label failed");
        let label_set = LabelSet::from_iter(vec![label_id]);
        let vertex_type = Arc::new(MemoryVertexTypeCatalog::new(
            label_set.clone(),
            vs.properties().clone(),
        ));
        graph_type.add_vertex_type(label_set, Arc::clone(&vertex_type));

        label_vertex_type.insert(label.clone(), vertex_type);
    }

    // Edge
    for es in manifest.edges_spec().iter() {
        let label_id = graph_type
            .add_label(es.label_name().clone())
            .expect("add label failed");
        let label_set = LabelSet::from_iter(vec![label_id]);
        let src_type = label_vertex_type
            .get(es.src_label())
            .expect("vertex type not found");
        let dst_type = label_vertex_type
            .get(es.dst_label())
            .expect("vertex type not found");

        let edge_type = MemoryEdgeTypeCatalog::new(
            label_set.clone(),
            src_type.clone(),
            dst_type.clone(),
            es.properties().clone(),
        );
        graph_type.add_edge_type(label_set, Arc::new(edge_type));
    }

    Ok(Arc::new(graph_type))
}

pub fn build_procedure() -> Procedure {
    // Name, directory path, Manifest relative path
    let parameters = vec![LogicalType::String, LogicalType::String];

    Procedure::new(parameters, None, |context, args| {
        assert_eq!(args.len(), 2);
        let graph_name = args[0]
            .try_as_string()
            .expect("graph name must be a string")
            .clone()
            .expect("graph name can't be empty");
        let manifest_path = args[1]
            .try_as_string()
            .expect("manifest path must be a string")
            .clone()
            .expect("manifest path can't be empty");

        import(context, graph_name, manifest_path)?;

        Ok(vec![])
    })
}
