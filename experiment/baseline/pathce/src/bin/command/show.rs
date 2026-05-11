use std::path::PathBuf;

use clap::Args;
use pathce::catalog::DuckCatalog;

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Specify the catalog directory.
    #[arg(short, long, value_name = "CATALOG_DIR")]
    catalog: PathBuf,
}

pub fn show(args: ShowArgs) {
    let catalog = DuckCatalog::import(args.catalog).unwrap();
    println!("{}", catalog);
}
