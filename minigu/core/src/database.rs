use std::path::{Path, PathBuf};
use std::sync::Arc;

use minigu_catalog::memory::MemoryCatalog;
use minigu_catalog::memory::directory::MemoryDirectoryCatalog;
use minigu_catalog::memory::schema::MemorySchemaCatalog;
use minigu_catalog::provider::{CatalogProvider, DirectoryOrSchema, SchemaRef};
use minigu_common::constants::DEFAULT_SCHEMA_NAME;
pub use minigu_context::database::DatabaseConfig;
use minigu_context::database::DatabaseContext;
use minigu_context::graph::{GraphContainer, GraphStorage};
use minigu_storage::tp::MemoryGraph;
use rayon::ThreadPoolBuilder;

use crate::catalog_persistence::{
    catalog_entry_to_graph_type, flatgraph_path, graph_data_path, load_catalog, statistic_path,
};
use crate::error::Result;
use crate::procedures::{
    FlatGraph, build_predefined_procedures, load_statistic, new_gcard_update_log,
};
use crate::session::Session;

pub struct Database {
    context: Arc<DatabaseContext>,
    default_schema: Arc<MemorySchemaCatalog>,
}

impl Database {
    /// Open (or create) an on-disk database at the given directory path.
    ///
    /// The directory will be created if it does not exist. On subsequent opens,
    /// the catalog and graph data are restored from disk.
    pub fn open<P: AsRef<Path>>(path: P, config: DatabaseConfig) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();

        // Create the database directory if it doesn't exist
        if !db_path.exists() {
            std::fs::create_dir_all(&db_path)?;
        }

        let (catalog, default_schema) = init_memory_catalog()?;

        // Load catalog from disk and restore graphs
        if let Some(db_catalog) = load_catalog(&db_path)? {
            for (graph_name, entry) in &db_catalog.graphs {
                let graph_type = catalog_entry_to_graph_type(entry);

                // Try to load FlatGraph — if available, skip the heavy .minigu checkpoint
                let fg_path = flatgraph_path(&db_path, graph_name);
                let flat_graph = if fg_path.exists() {
                    eprintln!(
                        "[database] loading FlatGraph from {} ...",
                        fg_path.display()
                    );
                    match FlatGraph::import_bincode(&fg_path) {
                        Ok(fg) => {
                            eprintln!("[database] FlatGraph loaded");
                            Some(fg)
                        }
                        Err(e) => {
                            eprintln!("[database] warning: failed to load FlatGraph: {}", e);
                            None
                        }
                    }
                } else {
                    None
                };

                // Always use an empty MemoryGraph — skip the heavy .minigu checkpoint.
                // FlatGraph + Statistic (loaded below) are sufficient for GCard queries.
                let graph = MemoryGraph::in_memory();

                let container =
                    Arc::new(GraphContainer::new(graph_type, GraphStorage::Memory(graph)));

                if let Some(fg) = flat_graph {
                    container.set_gcard_flat_graph(Arc::new(fg));
                }

                // Try to load Statistic + rebuild DegreeSeqGraphCompressed
                let stat_path = statistic_path(&db_path, graph_name);
                if stat_path.exists() {
                    eprintln!(
                        "[database] loading Statistic from {} ...",
                        stat_path.display()
                    );
                    match load_statistic(&stat_path) {
                        Ok(Some(statistic)) => {
                            match statistic.to_degree_seq_graph_compressed() {
                                Ok(dsgc) => {
                                    container.set_degree_seq_graph_compressed(Arc::new(dsgc));
                                    eprintln!("[database] DegreeSeqGraphCompressed rebuilt");
                                }
                                Err(e) => {
                                    eprintln!("[database] warning: failed to build DSGC: {}", e);
                                }
                            }
                            container.set_statistic(Arc::new(statistic));
                            // Initialize an empty update log (default max_k=3)
                            container.set_gcard_update_log(new_gcard_update_log(2));
                            eprintln!("[database] Statistic loaded, update log initialized");
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[database] warning: failed to load Statistic: {}", e);
                        }
                    }
                }

                default_schema.add_graph(graph_name.clone(), container);
            }
        }

        let config = DatabaseConfig {
            db_path: Some(db_path),
            ..config
        };
        let runtime = ThreadPoolBuilder::new()
            .num_threads(config.num_threads)
            .build()?;
        let context = Arc::new(DatabaseContext::new(catalog, runtime, config));
        Ok(Self {
            context,
            default_schema,
        })
    }

    pub fn open_in_memory(config: DatabaseConfig) -> Result<Self> {
        let (catalog, default_schema) = init_memory_catalog()?;
        let runtime = ThreadPoolBuilder::new()
            .num_threads(config.num_threads)
            .build()?;
        let context = Arc::new(DatabaseContext::new(catalog, runtime, config));
        Ok(Self {
            context,
            default_schema,
        })
    }

    pub fn session(&self) -> Result<Session> {
        Session::new(self.context.clone(), self.default_schema().clone())
    }

    fn default_schema(&self) -> &Arc<MemorySchemaCatalog> {
        &self.default_schema
    }
}

fn init_memory_catalog() -> Result<(MemoryCatalog, Arc<MemorySchemaCatalog>)> {
    let root = Arc::new(MemoryDirectoryCatalog::new(None));
    let parent = Arc::downgrade(&root);
    let default_schema = Arc::new(MemorySchemaCatalog::new(Some(parent)));
    for (name, procedure) in build_predefined_procedures() {
        default_schema.add_procedure(name, Arc::new(procedure));
    }
    root.add_child(
        DEFAULT_SCHEMA_NAME.into(),
        DirectoryOrSchema::Schema(default_schema.clone()),
    );
    let catalog = MemoryCatalog::new(DirectoryOrSchema::Directory(root));
    Ok((catalog, default_schema))
}
