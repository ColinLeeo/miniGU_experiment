use std::fs::{create_dir_all, File};
use std::io::BufWriter;
use std::path::PathBuf;

use clap::{Args, ValueEnum};
use pathce::pattern::GeneralPattern;
use pathce::schema::Schema;
use itertools::Itertools;
use rand::rngs::StdRng;
use rand::seq::IteratorRandom;
use rand::SeedableRng;

#[derive(Debug, ValueEnum, Clone)]
enum PatternType {
    Path,
    Cycle,
    Star,
}

#[derive(Debug, Args)]
pub struct GeneratePatternsArgs {
    /// Specify the schema path.
    #[arg(short, long, value_name = "SCHEMA_JSON")]
    schema: PathBuf,
    /// Speficy the pattern size.
    #[arg(short, long)]
    length: usize,
    /// Specify the pattern type.
    #[arg(short, long)]
    ty: PatternType,
    /// Specify the output directory.
    #[arg(short, long)]
    output: PathBuf,
    /// Speficy the random seed.
    #[arg(long, default_value = "12345")]
    seed: u64,
    /// Specify the maximum number of patterns.
    #[arg(long)]
    limit: Option<usize>,
    /// Specify whether N:1 edges should be avoided
    #[arg(long)]
    no_many_to_one: bool,
    /// Specify whether to generate only single direction paths
    #[arg(long)]
    single_direction: bool,
}

pub fn generate_patterns(args: GeneratePatternsArgs) {
    println!("{:#?}", args);
    let schema = Schema::import_json(args.schema).unwrap();
    let mut patterns: Vec<GeneralPattern> = match args.ty {
        PatternType::Path => {
            let mut paths = if args.no_many_to_one {
                schema
                    .generate_paths_without_many_to_one(args.length)
                    .into_iter()
                    .collect_vec()
            } else {
                schema.generate_paths(args.length).into_iter().collect_vec()
            };
            if args.single_direction {
                paths = paths
                    .into_iter()
                    .filter(|p| p.is_single_direction())
                    .collect_vec()
            }
            println!("generate {} paths of length {}", paths.len(), args.length);
            paths.into_iter().map(Into::into).collect_vec()
        }
        PatternType::Cycle => {
            let cycles = schema.generate_cycles(args.length);
            println!("generate {} cycles of length {}", cycles.len(), args.length);
            cycles
        }
        PatternType::Star => {
            let stars = schema.generate_stars(args.length);
            println!("generate {} stars of degree {}", stars.len(), args.length);
            stars
        }
    };
    if let Some(limit) = args.limit {
        let mut rng = StdRng::seed_from_u64(args.seed);
        patterns = patterns.into_iter().choose_multiple(&mut rng, limit);
        println!("sample {} patterns", patterns.len());
    }
    create_dir_all(&args.output).unwrap();
    for (i, p) in patterns.into_iter().enumerate() {
        let file = File::create(args.output.join(format!("{i}.json"))).unwrap();
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &p).unwrap();
    }
}
