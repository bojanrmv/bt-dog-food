//! `dgf-dup`: duplicated code that can be united, on its own.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dog_food_dup::changes::Changes;
use dog_food_dup::{report, scan, Settings};

#[derive(Parser)]
#[command(
    name = "dgf-dup",
    version,
    about = "Finds code that is the same apart from names and values, and says how to unite it"
)]
struct Args {
    /// Files or folders to scan (honoring .gitignore)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Write the report to this file instead of the terminal
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    /// Fewest tokens a copy has
    #[arg(long, default_value_t = Settings::default().min_tokens)]
    min_tokens: usize,
    /// Fewest lines a copy spans
    #[arg(long, default_value_t = Settings::default().min_lines)]
    min_lines: usize,
    /// Scan test code too
    #[arg(long)]
    tests: bool,
    /// How many clone classes to show; 0 shows all
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// Exit with code 1 when duplicated lines are above this share of all lines, in percent
    #[arg(long, value_name = "PERCENT")]
    fail_above: Option<f64>,
    /// Only findings with a copy in what this branch changed (since its merge
    /// base with --base, uncommitted and new files included); the whole
    /// repository is scanned, so copies of existing code are found
    #[arg(long)]
    changes: bool,
    /// With --changes: the branch to compare with [default: main, else master]
    #[arg(long, requires = "changes")]
    base: Option<String>,
    /// With --changes: exit with code 1 when the branch added any copy
    #[arg(long, requires = "changes")]
    fail_on_copies: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Readable text
    Table,
    /// Compact text for AI assistants
    Agent,
    /// Everything, for other tools
    Json,
    /// A self-contained page: the files with their copies, and each group
    Html,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let settings = Settings {
        min_tokens: args.min_tokens,
        min_lines: args.min_lines,
    };
    let changes = if args.changes {
        match Changes::since(&root, args.base.as_deref()) {
            Ok(changes) => Some(changes),
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    // With --changes and no paths, the whole repository: a new copy is
    // usually a copy of code the branch did not touch.
    let (paths, root) = match &changes {
        Some(c) if args.paths == [PathBuf::from(".")] => (vec![c.root.clone()], c.root.clone()),
        Some(c) => (args.paths.clone(), c.root.clone()),
        None => (args.paths.clone(), root),
    };
    let mut found = scan(&paths, &root, args.tests, settings);
    if let Some(changes) = &changes {
        found.keep_changed(changes);
    }
    let text = match args.format {
        Format::Table => report::table(&found, args.top),
        Format::Agent => report::agent(&found, args.top),
        Format::Json => report::json(&found),
        Format::Html => report::html(&found),
    };
    match &args.output {
        Some(path) => {
            if let Err(err) = std::fs::write(path, &text) {
                eprintln!("error: could not write {}: {err}", path.display());
                return ExitCode::from(3);
            }
            eprintln!("Wrote the report to {}", path.display());
        }
        None => {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(text.as_bytes());
        }
    }
    let copied = args.fail_on_copies && !found.classes.is_empty();
    if copied || args.fail_above.is_some_and(|limit| found.percent() > limit) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
