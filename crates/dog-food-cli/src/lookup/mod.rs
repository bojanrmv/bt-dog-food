//! Looking code up without reading whole files: `dgf outline`, `dgf show`
//! and `dgf refs`. Each takes many targets in one call, prints plain text
//! with a line number on every row, and says what it left out when it cuts
//! its output.

pub mod outline;
pub mod refs;
pub mod show;

use std::path::{Path, PathBuf};

use dog_food_core::Language;

use crate::discover::{self, Excludes};
use crate::{git, usage, Failure, LimitArgs, Settings};

/// Lines printed when `--max-lines` is not given.
pub const DEFAULT_LINES: usize = 200;

#[derive(clap::Args)]
pub struct CommonArgs {
    /// Print at most this many lines of code or rows, 0 = no limit [default: 200; 400 for show]
    #[arg(long, value_name = "N")]
    pub max_lines: Option<usize>,
    /// Skip files or folders matching this .gitignore-style pattern (repeatable)
    #[arg(long = "exclude", value_name = "PATTERN")]
    pub excludes: Vec<String>,
    /// Ignore dgf.toml
    #[arg(long)]
    pub no_config: bool,
}

impl CommonArgs {
    /// dgf.toml and its excludes, as the other commands read them.
    pub fn settings(&self) -> Result<Settings, Failure> {
        crate::load_settings(&LimitArgs {
            warn: None,
            fail: None,
            excludes: self.excludes.clone(),
            no_config: self.no_config,
        })
    }

    /// How many lines may be printed.
    pub fn budget(&self, default: usize) -> usize {
        match self.max_lines {
            Some(0) => usize::MAX,
            Some(lines) => lines,
            None => default,
        }
    }
}

/// The path as the reports show it: relative to the current folder when it
/// is inside, with forward slashes.
pub fn shown(path: &Path, cwd: &Path) -> String {
    let full = discover::absolute(path);
    let relative = full.strip_prefix(cwd).unwrap_or(&full);
    crate::display_path(&relative.to_string_lossy())
}

/// The text and language of a source file.
pub fn read(path: &Path) -> Result<(String, Language), String> {
    let language = Language::from_path(path)
        .ok_or_else(|| "not a C#, TypeScript, JavaScript or Rust file".to_string())?;
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    Ok((text, language))
}

/// Shares `budget` lines between items that want `sizes` lines: every item
/// gets an equal share, and what a small item leaves goes to the others.
pub fn allot(sizes: &[usize], budget: usize) -> Vec<usize> {
    let mut given = vec![0; sizes.len()];
    let mut left = budget;
    let mut open: Vec<usize> = (0..sizes.len()).filter(|i| sizes[*i] > 0).collect();
    while left > 0 && !open.is_empty() {
        let share = (left / open.len()).max(1);
        let mut still_open = Vec::new();
        for &item in &open {
            let take = share.min(sizes[item] - given[item]).min(left);
            given[item] += take;
            left -= take;
            if given[item] < sizes[item] {
                still_open.push(item);
            }
        }
        open = still_open;
    }
    given
}

/// The source files the branch and its uncommitted edits changed.
pub struct Changed {
    pub files: Vec<PathBuf>,
    /// What they were compared with: `main (1a2b3c4)`.
    pub against: String,
    pub warnings: Vec<String>,
}

pub fn changed(settings: &Settings, base: Option<String>) -> Result<Changed, Failure> {
    let root = git::root(&settings.cwd).map_err(usage)?;
    let (base, mut warnings) = crate::base_for(base, settings, &root)?;
    let (commit, note) = git::merge_base(&root, &base).map_err(usage)?;
    warnings.extend(note);
    let (files, merged) = git::own_changes(&root, &commit, false).map_err(usage)?;
    let short = git::short(&root, &commit);
    if !merged.is_empty() {
        warnings.push(format!(
            "{} changed files came in only by merge commits since {short} and are left out",
            merged.len()
        ));
    }
    let files = files
        .into_iter()
        .map(|file| root.join(file.path))
        .filter(|path| is_source(path, &settings.excludes))
        .collect();
    Ok(Changed {
        files,
        against: format!("{base} ({short})"),
        warnings,
    })
}

fn is_source(path: &Path, excludes: &Excludes) -> bool {
    Language::from_path(path).is_some() && path.is_file() && !excludes.is_excluded(path, false)
}

/// The parts of a name written `Cart.Total` or `Cart::total`.
pub fn name_parts(name: &str) -> Vec<&str> {
    name.split("::")
        .flat_map(|part| part.split('.'))
        .filter(|part| !part.is_empty())
        .collect()
}

/// Up to `limit` of `names` that look like `wanted`: the same letters in
/// another case first, then names that contain it or are contained in it,
/// then names a few edits away.
pub fn nearest<'a>(
    wanted: &str,
    names: impl Iterator<Item = &'a str>,
    limit: usize,
) -> Vec<&'a str> {
    let lower = wanted.to_lowercase();
    let mut ranked: Vec<(usize, &str)> = names
        .filter(|_| !lower.is_empty())
        .filter_map(|name| {
            let other = name.to_lowercase();
            let rank = if other == lower {
                0
            } else if other.contains(&lower) || lower.contains(&other) {
                1
            } else {
                let distance = edits(&lower, &other);
                if distance > 1 + lower.chars().count() / 4 {
                    return None;
                }
                1 + distance
            };
            Some((rank, name))
        })
        .collect();
    ranked.sort();
    ranked.dedup();
    ranked
        .into_iter()
        .map(|(_, name)| name)
        .take(limit)
        .collect()
}

/// How many letters must be added, removed or replaced to turn `a` into `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, from) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, to) in b.iter().enumerate() {
            let replaced = previous + usize::from(from != *to);
            previous = row[j + 1];
            row[j + 1] = replaced.min(row[j] + 1).min(previous + 1);
        }
    }
    row[b.len()]
}

/// `12-40`, or `12` for a single line.
pub fn range(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_item_leaves_its_share_to_the_others() {
        assert_eq!(allot(&[10, 100, 100], 110), [10, 50, 50]);
        assert_eq!(allot(&[5, 5], 100), [5, 5]);
        assert_eq!(allot(&[0, 7, 3], 4), [0, 2, 2]);
        assert_eq!(allot(&[9, 9, 9], 2), [1, 1, 0]);
        assert_eq!(allot(&[], 10), Vec::<usize>::new());
        assert_eq!(allot(&[3], usize::MAX), [3]);
    }

    #[test]
    fn nearest_names_come_closest_first() {
        let names = ["Handle", "HandleAsync", "Validate", "handle", "Candle"];
        assert_eq!(
            nearest("Handel", names.iter().copied(), 3),
            ["Handle", "handle"]
        );
        assert_eq!(
            nearest("handle", names.iter().copied(), 5),
            ["Handle", "handle", "HandleAsync", "Candle"]
        );
        assert!(nearest("Total", names.iter().copied(), 5).is_empty());
        assert!(nearest("", names.iter().copied(), 5).is_empty());
    }

    #[test]
    fn names_split_on_dots_and_double_colons() {
        assert_eq!(name_parts("Cart.Total"), ["Cart", "Total"]);
        assert_eq!(name_parts("Cart::total"), ["Cart", "total"]);
        assert_eq!(name_parts("total"), ["total"]);
    }

    #[test]
    fn ranges_of_one_line_are_one_number() {
        assert_eq!(range(4, 4), "4");
        assert_eq!(range(4, 9), "4-9");
    }
}
