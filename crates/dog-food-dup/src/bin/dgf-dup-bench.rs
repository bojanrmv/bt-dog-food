//! `dgf-dup-bench`: scores duplication findings against the labelled corpus.
//!
//! With no `--report`, it scans the corpus with dgf-dup and times the scan;
//! with `--report`, it scores another tool's JSON in dgf-dup's format.
//! With `--real LABELS --report FILE`, it scores a report on a real
//! repository against hand labels (tests/corpus/real): precision on real code.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use dog_food_dup::bench::{render, render_real, score, score_real, Labels, RealLabels};
use dog_food_dup::{report, scan, Settings};

#[derive(Parser)]
#[command(
    name = "dgf-dup-bench",
    about = "Scores duplication findings against the labelled corpus"
)]
struct Args {
    /// The corpus folder: cases.json and the code under Cases/
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus/csharp"))]
    corpus: PathBuf,
    /// Score this JSON report instead of scanning the corpus
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
    /// Hand labels of a real repository's findings (with --report, a scan of it)
    #[arg(long, value_name = "FILE", requires = "report")]
    real: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<String, String> {
    let read = |path: &PathBuf| {
        std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))
    };
    if let (Some(real), Some(report)) = (&args.real, &args.report) {
        let labels: RealLabels = serde_json::from_str(&read(real)?)
            .map_err(|err| format!("{}: {err}", real.display()))?;
        let value =
            serde_json::from_str(&read(report)?).map_err(|err| format!("the report: {err}"))?;
        return Ok(render_real(&score_real(&value, &labels), &labels));
    }
    let labels = Labels::parse(&read(&args.corpus.join("cases.json"))?)?;
    let (json, seconds) = match &args.report {
        Some(path) => (read(path)?, None),
        None => {
            let started = Instant::now();
            let found = scan(
                &[args.corpus.join("Cases")],
                &args.corpus,
                true,
                Settings::default(),
            );
            let seconds = started.elapsed().as_secs_f64();
            (report::json(&found), Some(seconds))
        }
    };
    let value = serde_json::from_str(&json).map_err(|err| format!("the report: {err}"))?;
    Ok(render(&score(&value, &labels), seconds))
}
