use std::time::Instant;

use clap::Parser;
use miette::{IntoDiagnostic, Result};
use minigu::common::data_chunk::display::{TableBuilder, TableOptions, TableStyle};
use minigu::database::{Database, DatabaseConfig};

#[derive(Debug, Parser, Clone)]
pub struct ScriptExecutor {}

impl ScriptExecutor {
    pub fn execute_file(&self, file: String, path: Option<String>) -> Result<()> {
        let db = if let Some(path) = path {
            Database::open(path, DatabaseConfig::default()).unwrap()
        } else {
            Database::open_in_memory(DatabaseConfig::default()).unwrap()
        };
        let mut session = db.session().unwrap();
        let content = std::fs::read_to_string(&file).into_diagnostic()?;
        let mut timing = false;
        for line in content.lines() {
            let line = line.trim();
            match line {
                "" => continue,
                _ if line.starts_with("--") => continue,
                ":quit" => break,
                ":timing" => {
                    timing = !timing;
                    eprintln!("Timing is {}", if timing { "on" } else { "off" });
                }
                line => {
                    let (timed, query) = if let Some(rest) = line.strip_prefix(":time ") {
                        (true, rest)
                    } else {
                        (timing, line)
                    };
                    let start = Instant::now();
                    let result = session.query(query)?;
                    let elapsed = start.elapsed();

                    // Print result table (like shell mode)
                    if let Some(schema) = result.schema() {
                        let options = TableOptions::new().with_style(TableStyle::Csv(b','));
                        let mut builder = TableBuilder::new(Some(schema.clone()), options);
                        let mut num_rows = 0;
                        for chunk in result {
                            num_rows += chunk.cardinality();
                            builder = builder.append_chunk(&chunk);
                        }
                        let table = builder.build();
                        println!("{table}");
                        eprintln!("({} rows)", num_rows);
                    }

                    if timed {
                        eprintln!("Time: {:.3}s", elapsed.as_secs_f64());
                    }
                }
            };
        }
        Ok(())
    }
}
