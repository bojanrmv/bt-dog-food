//! Measures parsing and scoring speed on a folder of real code, one thread,
//! with the files already in memory (no disk time):
//!
//!     cargo run --release -p dog-food-core --example bench -- <folder> [rounds]
//!
//! For end-to-end numbers (discovery, all cores, output) time `dgf analyze`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dog_food_core::{analyze_source, Language};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: bench <folder> [rounds]"));
    let rounds: usize = args
        .next()
        .map_or(5, |r| r.parse().expect("rounds is a number"));

    let mut files = Vec::new();
    collect(&root, &mut files);
    let bytes: usize = files.iter().map(|(_, _, source)| source.len()).sum();
    println!(
        "{} files, {:.1} MB, {} rounds",
        files.len(),
        bytes as f64 / 1e6,
        rounds
    );

    let mut best = f64::MAX;
    let mut functions = 0;
    for _ in 0..rounds {
        let started = Instant::now();
        functions = files
            .iter()
            .map(|(_, language, source)| analyze_source(source, *language).map_or(0, |f| f.len()))
            .sum();
        best = best.min(started.elapsed().as_secs_f64());
    }
    println!(
        "best round: {:.1} ms · {} functions · {:.1} MB/s · {:.0} files/s (one thread)",
        best * 1000.0,
        functions,
        bytes as f64 / 1e6 / best,
        files.len() as f64 / best
    );
}

/// Every supported file below `dir`, read into memory.
fn collect(dir: &Path, files: &mut Vec<(PathBuf, Language, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            collect(&path, files);
        } else if let Some(language) = Language::from_path(&path) {
            if let Ok(source) = std::fs::read_to_string(&path) {
                files.push((path, language, source));
            }
        }
    }
}
