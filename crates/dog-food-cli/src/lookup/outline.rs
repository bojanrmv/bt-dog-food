//! `dgf outline`: the shape of files without their bodies.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dog_food_core::outline::{outline, Symbol};
use globset::Glob;
use rayon::prelude::*;
use regex::Regex;

use super::{allot, changed, nearest, range, read, shown, CommonArgs, DEFAULT_LINES};
use crate::discover;
use crate::{usage, Failure, Settings, EXIT_FAIL};

#[derive(clap::Args)]
pub struct Args {
    /// Files, folders, or globs such as "src/**/*Handler.cs"
    paths: Vec<String>,
    /// Outline the files this branch and its uncommitted edits changed
    #[arg(long)]
    changes: bool,
    /// Base for --changes [default: `base` in dgf.toml, else main or master]
    #[arg(long, value_name = "REF", requires = "changes")]
    base: Option<String>,
    /// Keep only declarations whose signature matches this regular expression, and the types they are in
    #[arg(long = "match", value_name = "REGEX")]
    pattern: Option<String>,
    /// Leave out private declarations
    #[arg(long)]
    public: bool,
    #[command(flatten)]
    common: CommonArgs,
}

/// The declarations of one file that are printed.
struct Outlined {
    path: String,
    lines: usize,
    rows: Vec<String>,
    /// Why the file has no rows, when a filter left none.
    empty: &'static str,
}

pub fn run(args: Args) -> Result<ExitCode, Failure> {
    let settings = args.common.settings()?;
    if args.paths.is_empty() && !args.changes {
        return Err(usage(
            "name files, folders or globs to outline, or pass --changes".to_string(),
        ));
    }
    let filter = Filter::new(&args)?;
    let (mut files, notes) = changed_files(&args, &settings)?;
    let (named, missing) = named_files(&args.paths, &settings)?;
    files.extend(named);
    files.sort();
    files.dedup();
    let results: Vec<Result<Outlined, String>> = files
        .par_iter()
        .map(|file| outline_file(file, &settings.cwd, &filter))
        .collect();
    let outlined: Vec<&Outlined> = results.iter().flatten().collect();
    let sizes: Vec<usize> = outlined.iter().map(|o| o.rows.len()).collect();
    let given = allot(&sizes, args.common.budget(DEFAULT_LINES));
    let failed = results.iter().filter_map(|r| r.as_ref().err());
    let lines = notes
        .into_iter()
        .map(|note| format!("{note}\n"))
        .chain(
            outlined
                .iter()
                .zip(given)
                .map(|(file, given)| render(file, given)),
        )
        .chain(failed.map(|message| format!("{message}\n")))
        .chain(missing.iter().map(|path| not_found(path, &settings)));
    let text: String = lines.collect();
    crate::progress::finish();
    say!("{text}");
    let code = if outlined.is_empty() { EXIT_FAIL } else { 0 };
    Ok(ExitCode::from(code))
}

/// With `--changes`, the changed source files and what to say about them.
fn changed_files(args: &Args, settings: &Settings) -> Result<(Vec<PathBuf>, Vec<String>), Failure> {
    if !args.changes {
        return Ok((Vec::new(), Vec::new()));
    }
    let found = changed(settings, args.base.clone())?;
    let mut notes = vec![format!(
        "{} changed source files against {}",
        found.files.len(),
        found.against
    )];
    notes.extend(found.warnings.iter().map(|w| format!("warning: {w}")));
    Ok((found.files, notes))
}

/// The files the given paths name, and the paths that name none.
fn named_files(
    paths: &[String],
    settings: &Settings,
) -> Result<(Vec<PathBuf>, Vec<String>), Failure> {
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        let found = expand(path, settings)?;
        if found.is_empty() {
            missing.push(path.clone());
        }
        files.extend(found);
    }
    Ok((files, missing))
}

/// The files `path` names: itself, the source files in a folder, or the
/// source files under the current folder that a glob matches.
fn expand(path: &str, settings: &Settings) -> Result<Vec<PathBuf>, Failure> {
    let given = Path::new(path);
    if given.is_file() {
        return Ok(vec![given.to_path_buf()]);
    }
    if given.is_dir() {
        return Ok(discover::discover(
            &[given.to_path_buf()],
            settings.excludes.clone(),
        ));
    }
    if !path.contains(['*', '?', '[']) {
        return Ok(Vec::new());
    }
    let glob = Glob::new(&path.replace('\\', "/"))
        .map_err(|err| usage(format!("{path}: {err}")))?
        .compile_matcher();
    let all = discover::discover(
        std::slice::from_ref(&settings.cwd),
        settings.excludes.clone(),
    );
    Ok(all
        .into_iter()
        .filter(|file| glob.is_match(shown(file, &settings.cwd)))
        .collect())
}

struct Filter {
    pattern: Option<Regex>,
    public: bool,
}

impl Filter {
    fn new(args: &Args) -> Result<Filter, Failure> {
        let pattern = args
            .pattern
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|err| usage(format!("--match: {err}")))?;
        Ok(Filter {
            pattern,
            public: args.public,
        })
    }

    /// Which of `symbols` are printed: the ones the filters keep, and the
    /// declarations a kept one is inside.
    fn kept(&self, symbols: &[Symbol]) -> Vec<bool> {
        let hidden = self.hidden(symbols);
        let mut kept: Vec<bool> = symbols
            .iter()
            .zip(&hidden)
            .map(|(symbol, hidden)| !hidden && self.matches(symbol))
            .collect();
        for index in (0..symbols.len()).rev() {
            if let (true, Some(parent)) = (kept[index], symbols[index].parent) {
                kept[parent] = true;
            }
        }
        kept
    }

    /// With `--public`, the private declarations and what is inside them.
    fn hidden(&self, symbols: &[Symbol]) -> Vec<bool> {
        let mut hidden = vec![false; symbols.len()];
        for (index, symbol) in symbols.iter().enumerate() {
            let inside_hidden = symbol.parent.is_some_and(|parent| hidden[parent]);
            hidden[index] = inside_hidden || (self.public && symbol.private);
        }
        hidden
    }

    fn matches(&self, symbol: &Symbol) -> bool {
        self.pattern
            .as_ref()
            .is_none_or(|pattern| pattern.is_match(&symbol.signature))
    }
}

fn outline_file(file: &Path, cwd: &Path, filter: &Filter) -> Result<Outlined, String> {
    let path = shown(file, cwd);
    let (text, language) = read(file).map_err(|err| format!("{path}: {err}"))?;
    let symbols = outline(&text, language).map_err(|err| format!("{path}: {err}"))?;
    let kept = filter.kept(&symbols);
    let rows: Vec<String> = symbols
        .iter()
        .zip(&kept)
        .filter(|(_, kept)| **kept)
        .map(|(symbol, _)| row(symbol))
        .collect();
    let empty = match (symbols.is_empty(), rows.is_empty()) {
        (true, _) => "no declarations",
        (false, true) => "no declaration is kept by the filters",
        _ => "",
    };
    Ok(Outlined {
        path,
        lines: text.lines().count(),
        rows,
        empty,
    })
}

fn row(symbol: &Symbol) -> String {
    format!(
        "{}{}  {}",
        "  ".repeat(symbol.depth + 1),
        range(symbol.start_line, symbol.end_line),
        symbol.signature
    )
}

/// The file's heading and its first `given` rows.
fn render(file: &Outlined, given: usize) -> String {
    let mut text = format!("{}  ({} lines", file.path, file.lines);
    if !file.empty.is_empty() {
        text.push_str(&format!(", {}", file.empty));
    }
    text.push_str(")\n");
    for row in file.rows.iter().take(given) {
        text.push_str(row);
        text.push('\n');
    }
    let cut = file.rows.len().saturating_sub(given);
    if cut > 0 {
        text.push_str(&format!(
            "  ... {cut} more declarations, narrow with --match or raise --max-lines\n"
        ));
    }
    text
}

/// A path that names nothing, with the source files of the same name.
fn not_found(path: &str, settings: &Settings) -> String {
    let wanted = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if path.contains(['*', '?', '[']) {
        return format!("{path}: matches no source file\n");
    }
    let all = discover::discover(
        std::slice::from_ref(&settings.cwd),
        settings.excludes.clone(),
    );
    let shown_files: Vec<String> = all.iter().map(|f| shown(f, &settings.cwd)).collect();
    let names = shown_files
        .iter()
        .filter_map(|f| f.rsplit('/').next())
        .collect::<Vec<_>>();
    let close = nearest(&wanted, names.iter().copied(), 3);
    let places: Vec<&str> = shown_files
        .iter()
        .filter(|f| {
            f.rsplit('/')
                .next()
                .is_some_and(|name| close.contains(&name))
        })
        .map(String::as_str)
        .take(3)
        .collect();
    if places.is_empty() {
        format!("{path}: not found\n")
    } else {
        format!("{path}: not found; nearest: {}\n", places.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dog_food_core::Language;

    const SOURCE: &str = "\
public class Cart
{
    private int _count;
    public int Count => _count;
    public void Add(Item item) { _count++; }
    void Reset() { _count = 0; }
}
";

    fn rows(filter: &Filter) -> Vec<String> {
        let symbols = outline(SOURCE, Language::CSharp).unwrap();
        let kept = filter.kept(&symbols);
        symbols
            .iter()
            .zip(kept)
            .filter(|(_, kept)| *kept)
            .map(|(symbol, _)| row(symbol))
            .collect()
    }

    #[test]
    fn rows_are_indented_by_depth_with_their_lines() {
        let all = rows(&Filter {
            pattern: None,
            public: false,
        });
        assert_eq!(
            all,
            [
                "  1-7  public class Cart",
                "    3  private int _count",
                "    4  public int Count => _count",
                "    5  public void Add(Item item) { _count++; }",
                "    6  void Reset() { _count = 0; }",
            ]
        );
    }

    #[test]
    fn public_drops_private_members_and_match_keeps_the_type() {
        let public = rows(&Filter {
            pattern: None,
            public: true,
        });
        assert_eq!(public.len(), 3, "{public:?}");
        assert!(public
            .iter()
            .all(|row| !row.contains("_count;") && !row.contains("Reset")));
        let matched = rows(&Filter {
            pattern: Some(Regex::new("Add|Reset").unwrap()),
            public: true,
        });
        assert_eq!(
            matched,
            [
                "  1-7  public class Cart",
                "    5  public void Add(Item item) { _count++; }",
            ]
        );
    }

    #[test]
    fn a_cut_outline_says_how_much_is_left() {
        let file = Outlined {
            path: "src/Cart.cs".to_string(),
            lines: 7,
            rows: vec![
                "  1-7  class Cart".into(),
                "    3  int a".into(),
                "    4  int b".into(),
            ],
            empty: "",
        };
        assert_eq!(
            render(&file, 1),
            "src/Cart.cs  (7 lines)\n  1-7  class Cart\n  ... 2 more declarations, narrow with --match or raise --max-lines\n"
        );
        assert_eq!(render(&file, 3).lines().count(), 4);
    }
}
