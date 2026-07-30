use std::num::NonZeroUsize;

use clap::Parser;
use miette::Result;

use crate::script_executor;
use crate::shell::ShellArgs;

#[derive(Debug, Parser)]
pub enum Cli {
    Shell(ShellArgs),
    Execute {
        file: String,
        /// Path to the database directory. If not provided, an in-memory database will be opened.
        #[arg(long)]
        path: Option<String>,
        /// Number of worker threads used by the database runtime.
        #[arg(long, default_value_t = NonZeroUsize::new(1).unwrap())]
        threads: NonZeroUsize,
    },
}

impl Cli {
    pub fn run(self) -> Result<()> {
        eprintln!("[minigu] build: {}", env!("MINIGU_BUILD_TIME"));
        match self {
            Cli::Shell(shell) => shell.run(),
            Cli::Execute {
                file,
                path,
                threads,
            } => {
                let executor = script_executor::ScriptExecutor {};
                executor.execute_file(file, path, threads.get())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;

    #[test]
    fn execute_accepts_database_thread_count() {
        let cli = Cli::try_parse_from([
            "minigu",
            "execute",
            "queries.gql",
            "--path",
            "database",
            "--threads",
            "8",
        ])
        .unwrap();

        match cli {
            Cli::Execute { threads, .. } => assert_eq!(threads.get(), 8),
            Cli::Shell(_) => panic!("expected execute command"),
        }
    }

    #[test]
    fn execute_rejects_zero_database_threads() {
        let result = Cli::try_parse_from(["minigu", "execute", "queries.gql", "--threads", "0"]);

        assert!(result.is_err());
    }
}
