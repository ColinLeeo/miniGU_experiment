use thiserror::Error;

pub type GCardResult<T> = Result<T, GCardError>;

#[derive(Debug, Error)]
pub enum GCardError {
    #[error("CatalogError: {0}")]
    Catalog(String),
    #[error("SchemaError: {0}")]
    Schema(String),
    #[error("SampleError: {0}")]
    Sample(String),
    #[error("PatternError: {0}")]
    Pattern(String),
    #[error("EstimateError: {0}")]
    Estimate(String),
    #[error("GraphError: {0}")]
    Graph(String),
    #[error(transparent)]
    DuckDb(#[from] duckdb::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Bincode(#[from] bincode::Error),
    #[error(transparent)]
    ThreadPool(#[from] rayon::ThreadPoolBuildError),
}
